use crate::compiler::compile_with_diagnostic_limit;
use crate::scanner::{Scanner, ScannerItemKind, TokenType};
use crate::vm::VM;
use crate::{Diagnostic, RecordingHost, RevisionId, SourceDocument, SourceId, SourceSpan};

pub const MAX_ANALYSIS_SOURCE_BYTES: usize = 256 * 1024;
pub const MAX_ANALYSIS_LEXICAL_ITEMS: usize = 4_096;
pub const MAX_ANALYSIS_DIAGNOSTICS: usize = 128;
pub const MAX_ANALYSIS_NESTING_DEPTH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    Keyword,
    Comment,
    String,
    Number,
    Identifier,
    Operator,
    Punctuation,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    pub kind: HighlightKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticStatus {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolOccurrenceKind {
    Declaration,
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolOccurrence {
    pub name: String,
    pub kind: SymbolOccurrenceKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageAnalysis {
    pub source_id: SourceId,
    pub revision: RevisionId,
    pub diagnostics: Vec<Diagnostic>,
    pub highlights: Vec<HighlightSpan>,
    pub semantic_status: SemanticStatus,
    pub symbol_occurrences: Vec<SymbolOccurrence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisLimit {
    SourceBytes,
    LexicalItems,
    Diagnostics,
    NestingDepth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalysisError {
    LimitExceeded {
        limit: AnalysisLimit,
        max: usize,
        actual: usize,
    },
}

pub fn analyze(document: &SourceDocument) -> Result<LanguageAnalysis, AnalysisError> {
    if document.text.len() > MAX_ANALYSIS_SOURCE_BYTES {
        return Err(limit_error(
            AnalysisLimit::SourceBytes,
            MAX_ANALYSIS_SOURCE_BYTES,
            document.text.len(),
        ));
    }

    let mut scanner = Scanner::new(&document.text);
    let mut highlights = Vec::with_capacity(document.text.len().min(MAX_ANALYSIS_LEXICAL_ITEMS));
    let mut item_count = 0usize;
    let mut delimiters = Vec::with_capacity(MAX_ANALYSIS_NESTING_DEPTH + 1);
    let mut parenthesis_depth = 0usize;
    let mut control_depth = 0usize;
    let mut pending_controls = 0usize;
    let mut pending_control_retirement = None;
    let mut unary_depth = 0usize;
    loop {
        let item = scanner.scan_item();
        let kind = match item.kind {
            ScannerItemKind::Comment => HighlightKind::Comment,
            ScannerItemKind::Token(TokenType::Eof) => break,
            ScannerItemKind::Token(TokenType::Error) if item.token.lexeme().starts_with('"') => {
                HighlightKind::String
            }
            ScannerItemKind::Token(token_type) => highlight_kind(token_type),
        };

        item_count += 1;
        if item_count > MAX_ANALYSIS_LEXICAL_ITEMS {
            return Err(limit_error(
                AnalysisLimit::LexicalItems,
                MAX_ANALYSIS_LEXICAL_ITEMS,
                item_count,
            ));
        }

        if let ScannerItemKind::Token(token_type) = item.kind {
            if let Some(completed_controls) = pending_control_retirement.take() {
                if token_type == TokenType::Else {
                    pending_controls += completed_controls;
                } else {
                    control_depth -= completed_controls;
                }
            }

            match token_type {
                TokenType::LeftParen => {
                    delimiters.push((TokenType::LeftParen, 0));
                    parenthesis_depth += 1;
                }
                TokenType::LeftBracket => delimiters.push((TokenType::LeftBracket, 0)),
                TokenType::LeftBrace => {
                    delimiters.push((TokenType::LeftBrace, pending_controls));
                    pending_controls = 0;
                }
                TokenType::If | TokenType::While | TokenType::For | TokenType::Switch => {
                    control_depth += 1;
                    pending_controls += 1;
                }
                TokenType::Bang | TokenType::Minus => unary_depth += 1,
                _ => {}
            }

            let preflight_depth = delimiters.len() + control_depth + unary_depth;
            if preflight_depth > MAX_ANALYSIS_NESTING_DEPTH {
                return Err(limit_error(
                    AnalysisLimit::NestingDepth,
                    MAX_ANALYSIS_NESTING_DEPTH,
                    preflight_depth,
                ));
            }

            match token_type {
                TokenType::RightParen => {
                    if delimiters.last().map(|frame| frame.0) == Some(TokenType::LeftParen) {
                        delimiters.pop();
                        parenthesis_depth -= 1;
                    }
                }
                TokenType::RightBracket
                    if delimiters.last().map(|frame| frame.0) == Some(TokenType::LeftBracket) =>
                {
                    delimiters.pop();
                }
                TokenType::RightBrace
                    if delimiters.last().map(|frame| frame.0) == Some(TokenType::LeftBrace) =>
                {
                    if let Some((_, completed_controls)) = delimiters.pop() {
                        pending_control_retirement = Some(completed_controls);
                    }
                }
                TokenType::Semicolon if parenthesis_depth == 0 => {
                    pending_control_retirement = Some(pending_controls);
                    pending_controls = 0;
                }
                _ => {}
            }
            if !matches!(
                token_type,
                TokenType::Bang | TokenType::Minus | TokenType::LeftParen | TokenType::LeftBracket
            ) {
                unary_depth = 0;
            }
        }

        highlights.push(HighlightSpan {
            kind,
            span: item.token.span(document.id, document.revision),
        });
    }

    let mut vm = VM::new();
    let mut host = RecordingHost::default();
    let compile_outcome =
        compile_with_diagnostic_limit(document, &mut vm, &mut host, Some(MAX_ANALYSIS_DIAGNOSTICS));
    if compile_outcome.diagnostic_count > MAX_ANALYSIS_DIAGNOSTICS {
        return Err(limit_error(
            AnalysisLimit::Diagnostics,
            MAX_ANALYSIS_DIAGNOSTICS,
            compile_outcome.diagnostic_count,
        ));
    }
    let compiled = compile_outcome.function.is_some();

    Ok(LanguageAnalysis {
        source_id: document.id,
        revision: document.revision,
        diagnostics: host.diagnostics().to_vec(),
        highlights,
        semantic_status: if compiled {
            SemanticStatus::Available
        } else {
            SemanticStatus::Unavailable
        },
        symbol_occurrences: Vec::new(),
    })
}

fn limit_error(limit: AnalysisLimit, max: usize, actual: usize) -> AnalysisError {
    AnalysisError::LimitExceeded { limit, max, actual }
}

fn highlight_kind(token_type: TokenType) -> HighlightKind {
    match token_type {
        TokenType::Identifier => HighlightKind::Identifier,
        TokenType::String => HighlightKind::String,
        TokenType::Number => HighlightKind::Number,
        TokenType::Error => HighlightKind::Invalid,
        TokenType::LeftParen
        | TokenType::RightParen
        | TokenType::LeftBracket
        | TokenType::RightBracket
        | TokenType::LeftBrace
        | TokenType::RightBrace
        | TokenType::Comma
        | TokenType::Dot
        | TokenType::Semicolon
        | TokenType::Colon => HighlightKind::Punctuation,
        TokenType::Minus
        | TokenType::Plus
        | TokenType::Slash
        | TokenType::Backslash
        | TokenType::Star
        | TokenType::Bang
        | TokenType::BangEqual
        | TokenType::Equal
        | TokenType::EqualEqual
        | TokenType::Greater
        | TokenType::GreaterGreater
        | TokenType::GreaterGreaterGreater
        | TokenType::GreaterEqual
        | TokenType::Less
        | TokenType::LessEqual => HighlightKind::Operator,
        TokenType::And
        | TokenType::Class
        | TokenType::Case
        | TokenType::Default
        | TokenType::Else
        | TokenType::False
        | TokenType::For
        | TokenType::Fun
        | TokenType::If
        | TokenType::Nil
        | TokenType::Or
        | TokenType::Print
        | TokenType::Return
        | TokenType::Super
        | TokenType::Switch
        | TokenType::This
        | TokenType::True
        | TokenType::Var
        | TokenType::While => HighlightKind::Keyword,
        TokenType::Eof => unreachable!("EOF is not highlighted"),
    }
}
