use crate::{RevisionId, SourceId, SourceSpan, TextPosition};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TokenType {
    // Single-character tokens
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    LeftBrace,
    RightBrace,
    Comma,
    Dot,
    Minus,
    Plus,
    Semicolon,
    Colon,
    Slash,
    Backslash,
    Star,
    // One or two character tokens
    Bang,
    BangEqual,
    Equal,
    EqualEqual,
    Greater,
    GreaterGreater,
    GreaterGreaterGreater,
    GreaterEqual,
    Less,
    LessEqual,
    // Literals
    Identifier,
    String,
    Number,
    // Keywords
    And,
    Class,
    Case,
    Default,
    Else,
    False,
    For,
    Fun,
    If,
    Nil,
    Or,
    Print,
    Return,
    Super,
    Switch,
    This,
    True,
    Var,
    While,
    // Special
    Error,
    Eof,
}

#[derive(Debug, Clone, Copy)]
pub struct Token<'a> {
    pub token_type: TokenType,
    pub start: &'a str,
    pub length: usize,
    pub line: usize,
    pub column: usize,
    pub start_position: TextPosition,
    pub end_position: TextPosition,
    pub error_message: Option<&'static str>,
}

impl Token<'_> {
    pub fn lexeme(&self) -> &str {
        &self.start[..self.length]
    }

    pub fn span(&self, source_id: SourceId, revision: RevisionId) -> SourceSpan {
        SourceSpan {
            source_id,
            revision,
            start: self.start_position,
            end: self.end_position,
        }
    }
}

pub struct Scanner<'a> {
    source: &'a str,
    start: &'a str,
    current: &'a str,
    line: usize,
    column: usize,
    start_position: TextPosition,
}

impl<'a> Scanner<'a> {
    pub fn new(source: &'a str) -> Self {
        Scanner {
            source,
            start: source,
            current: source,
            line: 1,
            column: 1,
            start_position: TextPosition {
                byte_offset: 0,
                line: 1,
                column: 1,
            },
        }
    }

    fn is_alpha(c: char) -> bool {
        c.is_ascii_alphabetic() || c == '_'
    }

    fn is_digit(c: char) -> bool {
        c.is_ascii_digit()
    }

    pub fn scan_token(&mut self) -> Token<'a> {
        self.skip_whitespace();
        self.start = self.current;
        self.start_position = self.current_position();

        if self.is_at_end() {
            return self.make_token(TokenType::Eof);
        }

        let c = self.advance();

        if Self::is_alpha(c) {
            return self.identifier();
        }

        if Self::is_digit(c) {
            return self.number();
        }

        match c {
            '(' => self.make_token(TokenType::LeftParen),
            ')' => self.make_token(TokenType::RightParen),
            '[' => self.make_token(TokenType::LeftBracket),
            ']' => self.make_token(TokenType::RightBracket),
            '{' => self.make_token(TokenType::LeftBrace),
            '}' => self.make_token(TokenType::RightBrace),
            ';' => self.make_token(TokenType::Semicolon),
            ':' => self.make_token(TokenType::Colon),
            ',' => self.make_token(TokenType::Comma),
            '.' => self.make_token(TokenType::Dot),
            '-' => self.make_token(TokenType::Minus),
            '+' => self.make_token(TokenType::Plus),
            '/' => self.make_token(TokenType::Slash),
            '\\' => self.make_token(TokenType::Backslash),
            '*' => self.make_token(TokenType::Star),
            '!' => {
                let t = if self.match_char('=') {
                    TokenType::BangEqual
                } else {
                    TokenType::Bang
                };
                self.make_token(t)
            }
            '=' => {
                let t = if self.match_char('=') {
                    TokenType::EqualEqual
                } else {
                    TokenType::Equal
                };
                self.make_token(t)
            }
            '<' => {
                let t = if self.match_char('=') {
                    TokenType::LessEqual
                } else {
                    TokenType::Less
                };
                self.make_token(t)
            }
            '>' => {
                let t = if self.match_char('=') {
                    TokenType::GreaterEqual
                } else if self.match_char('>') {
                    if self.match_char('>') {
                        TokenType::GreaterGreaterGreater
                    } else {
                        TokenType::GreaterGreater
                    }
                } else {
                    TokenType::Greater
                };
                self.make_token(t)
            }
            '"' => self.string(),
            _ => self.error_token("Unexpected character."),
        }
    }

    fn is_at_end(&self) -> bool {
        self.current.is_empty()
    }

    fn advance(&mut self) -> char {
        let c = self.current.chars().next().unwrap();
        self.current = &self.current[c.len_utf8()..];
        if c == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        c
    }

    fn current_position(&self) -> TextPosition {
        TextPosition {
            byte_offset: self.source.len() - self.current.len(),
            line: self.line,
            column: self.column,
        }
    }

    fn peek(&self) -> char {
        self.current.chars().next().unwrap_or('\0')
    }

    fn peek_next(&self) -> char {
        let mut chars = self.current.chars();
        chars.next();
        chars.next().unwrap_or('\0')
    }

    fn match_char(&mut self, expected: char) -> bool {
        if self.is_at_end() {
            return false;
        }
        if !self.current.starts_with(expected) {
            return false;
        }
        self.advance();
        true
    }

    fn make_token(&self, token_type: TokenType) -> Token<'a> {
        let length = self.start.len() - self.current.len();
        Token {
            token_type,
            start: self.start,
            length,
            line: self.start_position.line,
            column: self.start_position.column,
            start_position: self.start_position,
            end_position: self.current_position(),
            error_message: None,
        }
    }

    fn error_token(&self, message: &'static str) -> Token<'a> {
        let length = self.start.len() - self.current.len();
        Token {
            token_type: TokenType::Error,
            start: self.start,
            length,
            line: self.start_position.line,
            column: self.start_position.column,
            start_position: self.start_position,
            end_position: self.current_position(),
            error_message: Some(message),
        }
    }

    fn skip_whitespace(&mut self) {
        loop {
            match self.peek() {
                ' ' | '\r' | '\t' => {
                    self.advance();
                }
                '\n' => {
                    self.advance();
                }
                '/' => {
                    if self.peek_next() == '/' {
                        // A comment goes until the end of the line.
                        while self.peek() != '\n' && !self.is_at_end() {
                            self.advance();
                        }
                    } else {
                        return;
                    }
                }
                _ => return,
            }
        }
    }

    fn check_keyword(&self, start: usize, rest: &str, token_type: TokenType) -> TokenType {
        let len = self.start.len() - self.current.len();
        if len == start + rest.len() && &self.start[start..start + rest.len()] == rest {
            token_type
        } else {
            TokenType::Identifier
        }
    }

    fn identifier_type(&self) -> TokenType {
        match self.start.chars().next().unwrap() {
            'a' => self.check_keyword(1, "nd", TokenType::And),
            'c' => {
                if self.start.len() - self.current.len() > 1 {
                    match self.start.chars().nth(1) {
                        Some('a') => self.check_keyword(2, "se", TokenType::Case),
                        Some('l') => self.check_keyword(2, "ass", TokenType::Class),
                        _ => TokenType::Identifier,
                    }
                } else {
                    TokenType::Identifier
                }
            }
            'd' => self.check_keyword(1, "efault", TokenType::Default),
            'e' => self.check_keyword(1, "lse", TokenType::Else),
            'f' => {
                if self.start.len() - self.current.len() > 1 {
                    match self.start.chars().nth(1) {
                        Some('a') => self.check_keyword(2, "lse", TokenType::False),
                        Some('o') => self.check_keyword(2, "r", TokenType::For),
                        Some('u') => self.check_keyword(2, "n", TokenType::Fun),
                        _ => TokenType::Identifier,
                    }
                } else {
                    TokenType::Identifier
                }
            }
            'i' => self.check_keyword(1, "f", TokenType::If),
            'n' => self.check_keyword(1, "il", TokenType::Nil),
            'o' => self.check_keyword(1, "r", TokenType::Or),
            'p' => self.check_keyword(1, "rint", TokenType::Print),
            'r' => self.check_keyword(1, "eturn", TokenType::Return),
            's' => {
                if self.start.len() - self.current.len() > 1 {
                    match self.start.chars().nth(1) {
                        Some('u') => self.check_keyword(2, "per", TokenType::Super),
                        Some('w') => self.check_keyword(2, "itch", TokenType::Switch),
                        _ => TokenType::Identifier,
                    }
                } else {
                    TokenType::Identifier
                }
            }
            't' => {
                if self.start.len() - self.current.len() > 1 {
                    match self.start.chars().nth(1) {
                        Some('h') => self.check_keyword(2, "is", TokenType::This),
                        Some('r') => self.check_keyword(2, "ue", TokenType::True),
                        _ => TokenType::Identifier,
                    }
                } else {
                    TokenType::Identifier
                }
            }
            'v' => self.check_keyword(1, "ar", TokenType::Var),
            'w' => self.check_keyword(1, "hile", TokenType::While),
            _ => TokenType::Identifier,
        }
    }

    fn identifier(&mut self) -> Token<'a> {
        while Self::is_alpha(self.peek()) || Self::is_digit(self.peek()) {
            self.advance();
        }

        self.make_token(self.identifier_type())
    }

    fn number(&mut self) -> Token<'a> {
        while Self::is_digit(self.peek()) {
            self.advance();
        }

        // Look for a fractional part.
        if self.peek() == '.' && Self::is_digit(self.peek_next()) {
            // Consume the ".".
            self.advance();
            while Self::is_digit(self.peek()) {
                self.advance();
            }
        }

        self.make_token(TokenType::Number)
    }

    fn string(&mut self) -> Token<'a> {
        while self.peek() != '"' && !self.is_at_end() {
            self.advance();
        }

        if self.is_at_end() {
            return self.error_token("Unterminated string.");
        }

        self.advance();
        self.make_token(TokenType::String)
    }
}

#[cfg(test)]
mod tests {
    use super::{Scanner, TokenType};
    use crate::{RevisionId, SourceId};

    #[test]
    fn compound_operators_advance_the_following_token_column() {
        let mut scanner = Scanner::new(">>> name");

        assert_eq!(
            scanner.scan_token().token_type,
            TokenType::GreaterGreaterGreater
        );
        let identifier = scanner.scan_token();

        assert_eq!(identifier.column, 5);
    }

    #[test]
    fn comments_and_tabs_preserve_half_open_token_positions() {
        let mut scanner = Scanner::new("// note\n\tprint");
        let token = scanner.scan_token();
        let span = token.span(SourceId(4), RevisionId(2));

        assert_eq!(token.token_type, TokenType::Print);
        assert_eq!(span.start.byte_offset, 9);
        assert_eq!((span.start.line, span.start.column), (2, 2));
        assert_eq!(span.end.byte_offset, 14);
        assert_eq!((span.end.line, span.end.column), (2, 7));
    }

    #[test]
    fn multiline_unicode_strings_use_scalar_columns_and_byte_offsets() {
        let mut scanner = Scanner::new("\"a\nβ\" next");
        let string = scanner.scan_token();
        let identifier = scanner.scan_token();

        assert_eq!(string.token_type, TokenType::String);
        assert_eq!(string.start_position.byte_offset, 0);
        assert_eq!(string.end_position.byte_offset, 6);
        assert_eq!(
            (string.end_position.line, string.end_position.column),
            (2, 3)
        );
        assert_eq!((identifier.line, identifier.column), (2, 4));
    }

    #[test]
    fn normalized_line_endings_produce_consistent_coordinates() {
        let document = crate::SourceDocument::new(
            SourceId(1),
            RevisionId(1),
            "lines.ox",
            "print 1;\r\nprint 2;\rprint 3;",
        );
        let mut scanner = Scanner::new(&document.text);

        let mut prints = Vec::new();
        loop {
            let token = scanner.scan_token();
            if token.token_type == TokenType::Print {
                prints.push((token.line, token.column));
            }
            if token.token_type == TokenType::Eof {
                break;
            }
        }

        assert_eq!(prints, [(1, 1), (2, 1), (3, 1)]);
    }

    #[test]
    fn malformed_unicode_scalars_retain_the_source_lexeme_and_span() {
        let mut scanner = Scanner::new("β@");
        let first = scanner.scan_token();
        let second = scanner.scan_token();

        assert_eq!(first.token_type, TokenType::Error);
        assert_eq!(first.lexeme(), "β");
        assert_eq!(
            (
                first.start_position.byte_offset,
                first.end_position.byte_offset
            ),
            (0, 2)
        );
        assert_eq!((first.column, first.end_position.column), (1, 2));
        assert_eq!(second.lexeme(), "@");
        assert_eq!(
            (
                second.start_position.byte_offset,
                second.end_position.byte_offset
            ),
            (2, 3)
        );
        assert_eq!((second.column, second.end_position.column), (2, 3));
    }

    #[test]
    fn eof_is_a_zero_width_span_at_the_normalized_end() {
        let mut scanner = Scanner::new("print 1;\n");
        let eof = loop {
            let token = scanner.scan_token();
            if token.token_type == TokenType::Eof {
                break token;
            }
        };

        assert_eq!(eof.start_position, eof.end_position);
        assert_eq!(eof.start_position.byte_offset, 9);
        assert_eq!((eof.line, eof.column), (2, 1));
    }

    #[test]
    fn unterminated_strings_keep_the_nonempty_source_span() {
        let mut scanner = Scanner::new("\"β");
        let token = scanner.scan_token();

        assert_eq!(token.token_type, TokenType::Error);
        assert_eq!(token.error_message, Some("Unterminated string."));
        assert_eq!(token.lexeme(), "\"β");
        assert_eq!(
            (
                token.start_position.byte_offset,
                token.end_position.byte_offset
            ),
            (0, 3)
        );
    }
}
