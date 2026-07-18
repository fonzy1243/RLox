use crate::compiler::{CompileLimit, CompileOptions, compile_with_options};
use crate::scanner::{Scanner, ScannerItemKind, TokenType};
use crate::vm::VM;
use std::collections::HashMap;

use crate::{
    BindingId, Diagnostic, RecordingHost, RevisionId, SourceDocument, SourceId, SourceSpan,
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Variable,
    Parameter,
    BuiltIn,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolResolution {
    Local,
    CapturedUpvalue,
    Global,
    BuiltIn,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolOccurrence {
    pub name: String,
    pub kind: SymbolOccurrenceKind,
    pub symbol_kind: SymbolKind,
    pub resolution: SymbolResolution,
    pub span: SourceSpan,
    pub declaration_targets: Vec<SourceSpan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvedStorage {
    Local,
    CapturedUpvalue,
    Global,
}

#[derive(Debug, Clone)]
struct CollectedOccurrence {
    name: String,
    kind: SymbolOccurrenceKind,
    symbol_kind: SymbolKind,
    storage: ResolvedStorage,
    span: SourceSpan,
    declaration: Option<SourceSpan>,
}

#[derive(Debug, Default)]
pub(crate) struct AnalysisCollector {
    occurrences: Vec<CollectedOccurrence>,
    binding_kinds: HashMap<BindingId, SymbolKind>,
}

impl AnalysisCollector {
    pub(crate) fn record_declaration(
        &mut self,
        name: &str,
        symbol_kind: SymbolKind,
        storage: ResolvedStorage,
        span: SourceSpan,
        binding_id: Option<BindingId>,
    ) {
        if let Some(binding_id) = binding_id {
            self.binding_kinds.insert(binding_id, symbol_kind);
        }
        self.occurrences.push(CollectedOccurrence {
            name: name.to_owned(),
            kind: SymbolOccurrenceKind::Declaration,
            symbol_kind,
            storage,
            span,
            declaration: Some(span),
        });
    }

    pub(crate) fn record_reference(
        &mut self,
        name: &str,
        kind: SymbolOccurrenceKind,
        storage: ResolvedStorage,
        span: SourceSpan,
        binding_id: Option<BindingId>,
        declaration: Option<SourceSpan>,
    ) {
        let symbol_kind = binding_id
            .and_then(|binding_id| self.binding_kinds.get(&binding_id).copied())
            .unwrap_or(SymbolKind::Unknown);
        self.occurrences.push(CollectedOccurrence {
            name: name.to_owned(),
            kind,
            symbol_kind,
            storage,
            span,
            declaration,
        });
    }

    pub(crate) fn finish(mut self) -> Vec<SymbolOccurrence> {
        self.occurrences.sort_by_key(|value| {
            (
                value.span.start.byte_offset,
                value.span.end.byte_offset,
                match value.kind {
                    SymbolOccurrenceKind::Declaration => 0,
                    SymbolOccurrenceKind::Read => 1,
                    SymbolOccurrenceKind::Write => 2,
                },
            )
        });

        let mut globals: HashMap<String, Vec<(SourceSpan, SymbolKind)>> = HashMap::new();
        for value in &self.occurrences {
            if value.kind == SymbolOccurrenceKind::Declaration
                && value.storage == ResolvedStorage::Global
            {
                globals
                    .entry(value.name.clone())
                    .or_default()
                    .push((value.span, value.symbol_kind));
            }
        }

        self.occurrences
            .into_iter()
            .map(|value| {
                let (symbol_kind, resolution, declaration_targets) = match value.storage {
                    ResolvedStorage::Local => (
                        value.symbol_kind,
                        SymbolResolution::Local,
                        value.declaration.into_iter().collect(),
                    ),
                    ResolvedStorage::CapturedUpvalue => (
                        value.symbol_kind,
                        SymbolResolution::CapturedUpvalue,
                        value.declaration.into_iter().collect(),
                    ),
                    ResolvedStorage::Global if value.kind == SymbolOccurrenceKind::Declaration => (
                        value.symbol_kind,
                        SymbolResolution::Global,
                        vec![value.span],
                    ),
                    ResolvedStorage::Global => match globals.get(&value.name) {
                        Some(candidates) if !candidates.is_empty() => {
                            let first_kind = candidates[0].1;
                            let symbol_kind =
                                if candidates.iter().all(|candidate| candidate.1 == first_kind) {
                                    first_kind
                                } else {
                                    SymbolKind::Unknown
                                };
                            (
                                symbol_kind,
                                SymbolResolution::Global,
                                candidates.iter().map(|candidate| candidate.0).collect(),
                            )
                        }
                        _ if crate::vm::is_native_name(&value.name) => {
                            (SymbolKind::BuiltIn, SymbolResolution::BuiltIn, Vec::new())
                        }
                        _ => (
                            SymbolKind::Unknown,
                            SymbolResolution::Unresolved,
                            Vec::new(),
                        ),
                    },
                };
                SymbolOccurrence {
                    name: value.name,
                    kind: value.kind,
                    symbol_kind,
                    resolution,
                    span: value.span,
                    declaration_targets,
                }
            })
            .collect()
    }
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

        highlights.push(HighlightSpan {
            kind,
            span: item.token.span(document.id, document.revision),
        });
    }

    let mut vm = VM::new();
    let mut host = RecordingHost::default();
    let compile_outcome = compile_with_options(
        document,
        &mut vm,
        &mut host,
        CompileOptions {
            diagnostic_limit: Some(MAX_ANALYSIS_DIAGNOSTICS),
            recursion_limit: Some(MAX_ANALYSIS_NESTING_DEPTH),
            collect_symbols: true,
        },
    );
    if let Some(CompileLimit::RecursionDepth { max, actual }) = compile_outcome.limit {
        return Err(limit_error(AnalysisLimit::NestingDepth, max, actual));
    }
    if compile_outcome.diagnostic_count > MAX_ANALYSIS_DIAGNOSTICS {
        return Err(limit_error(
            AnalysisLimit::Diagnostics,
            MAX_ANALYSIS_DIAGNOSTICS,
            compile_outcome.diagnostic_count,
        ));
    }
    let compiled = compile_outcome.function.is_some();
    let symbol_occurrences = if compiled {
        compile_outcome.symbol_occurrences
    } else {
        Vec::new()
    };

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
        symbol_occurrences,
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
