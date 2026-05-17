use crate::chunk::{Chunk, OpCode};
#[cfg(feature = "debug_print_code")]
use crate::debug::disassemble_chunk;
use crate::scanner::{Scanner, Token, TokenType};
use crate::value::Value;

struct Parser<'a> {
    current: Token<'a>,
    previous: Token<'a>,
    had_error: bool,
    panic_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
#[repr(u8)]
enum Precedence {
    None,
    Assignment, // =
    Or,         // or
    And,        // and
    Equality,   // == !=
    Comparison, // < > <= >=
    Term,       // + -
    Factor,     // * /
    Unary,      // ! -
    Call,       // . ()
    Primary,
}

type ParseFn = for<'a> fn(&mut Parser<'a>, &mut Scanner<'a>, &mut Chunk);

struct ParseRule {
    prefix: Option<ParseFn>,
    infix: Option<ParseFn>,
    precedence: Precedence,
}

static RULES: &[ParseRule] = &[
    // LeftParen
    ParseRule {
        prefix: Some(grouping),
        infix: None,
        precedence: Precedence::None,
    },
    // RightParen
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // LeftBrace
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // RightBrace
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Comma
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Dot
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Minus
    ParseRule {
        prefix: Some(unary),
        infix: Some(binary),
        precedence: Precedence::Term,
    },
    // Plus
    ParseRule {
        prefix: None,
        infix: Some(binary),
        precedence: Precedence::Term,
    },
    // Semicolon
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Slash
    ParseRule {
        prefix: None,
        infix: Some(binary),
        precedence: Precedence::Factor,
    },
    // Star
    ParseRule {
        prefix: None,
        infix: Some(binary),
        precedence: Precedence::Factor,
    },
    // Bang
    ParseRule {
        prefix: Some(unary),
        infix: None,
        precedence: Precedence::None,
    },
    // BangEqual
    ParseRule {
        prefix: None,
        infix: Some(binary),
        precedence: Precedence::Equality,
    },
    // Equal
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // EqualEqual
    ParseRule {
        prefix: None,
        infix: Some(binary),
        precedence: Precedence::Equality,
    },
    // Greater
    ParseRule {
        prefix: None,
        infix: Some(binary),
        precedence: Precedence::Comparison,
    },
    // GreaterEqual
    ParseRule {
        prefix: None,
        infix: Some(binary),
        precedence: Precedence::Comparison,
    },
    // Less
    ParseRule {
        prefix: None,
        infix: Some(binary),
        precedence: Precedence::Comparison,
    },
    // LessEqual
    ParseRule {
        prefix: None,
        infix: Some(binary),
        precedence: Precedence::Comparison,
    },
    // Identifier
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // String
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Number
    ParseRule {
        prefix: Some(number),
        infix: None,
        precedence: Precedence::None,
    },
    // And
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Class
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Else
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // False
    ParseRule {
        prefix: Some(literal),
        infix: None,
        precedence: Precedence::None,
    },
    // For
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Fun
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // If
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Nil
    ParseRule {
        prefix: Some(literal),
        infix: None,
        precedence: Precedence::None,
    },
    // Or
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Print
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Return
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Super
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // This
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // True
    ParseRule {
        prefix: Some(literal),
        infix: None,
        precedence: Precedence::None,
    },
    // Var
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // While
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Error
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
    // Eof
    ParseRule {
        prefix: None,
        infix: None,
        precedence: Precedence::None,
    },
];

fn current_chunk<'a>(chunk: &'a mut Chunk) -> &'a mut Chunk {
    chunk
}

fn error_at(parser: &mut Parser, token: &Token, message: &str) {
    if parser.panic_mode {
        return;
    }
    parser.panic_mode = true;

    eprint!("[line {}] Error", token.line);

    match token.token_type {
        TokenType::Eof => eprint!(" at end"),
        TokenType::Error => {}
        _ => eprint!(" at '{}'", &token.start[..token.length]),
    }

    eprintln!(": {}", message);
    parser.had_error = true;
}

fn error(parser: &mut Parser, message: &str) {
    let token = parser.previous;
    error_at(parser, &token, message);
}

fn error_at_current(parser: &mut Parser, message: &str) {
    let token = parser.current;
    error_at(parser, &token, message);
}

pub fn compile(source: &str, chunk: &mut Chunk) -> bool {
    let mut scanner = Scanner::new(source);
    let dummy = Token {
        token_type: TokenType::Eof,
        start: "",
        length: 0,
        line: 0,
    };
    let mut parser = Parser {
        current: dummy,
        previous: dummy,
        had_error: false,
        panic_mode: false,
    };

    advance(&mut parser, &mut scanner);
    expression(&mut parser, &mut scanner, chunk);
    consume(
        &mut parser,
        &mut scanner,
        TokenType::Eof,
        "Expected end of expression.",
    );

    end_compiler(&parser, chunk);
    !parser.had_error
}

fn advance<'a>(parser: &mut Parser<'a>, scanner: &mut Scanner<'a>) {
    parser.previous = parser.current;

    loop {
        parser.current = scanner.scan_token();
        if parser.current.token_type != TokenType::Error {
            break;
        }

        error_at_current(parser, parser.current.start);
    }
}

fn expression<'a>(parser: &mut Parser<'a>, scanner: &mut Scanner<'a>, chunk: &mut Chunk) {
    parse_precedence(parser, scanner, chunk, Precedence::Assignment);
}

fn consume<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    token_type: TokenType,
    message: &str,
) {
    if parser.current.token_type == token_type {
        advance(parser, scanner);
        return;
    }

    error_at_current(parser, message);
}

fn emit_byte(parser: &Parser, chunk: &mut Chunk, byte: u8) {
    chunk.write(byte, parser.previous.line);
}

fn emit_bytes(parser: &Parser, chunk: &mut Chunk, byte1: u8, byte2: u8) {
    emit_byte(parser, chunk, byte1);
    emit_byte(parser, chunk, byte2);
}

fn emit_return(parser: &Parser, chunk: &mut Chunk) {
    emit_byte(parser, chunk, OpCode::Return as u8);
}

fn make_constant(parser: &mut Parser, chunk: &mut Chunk, value: Value) -> u8 {
    let constant = chunk.add_constant(value);
    if constant > u8::MAX as usize {
        error(parser, "Too many constants in one chunk.");
        return 0;
    }
    constant as u8
}

fn emit_constant(parser: &mut Parser, chunk: &mut Chunk, value: Value) {
    let constant = make_constant(parser, chunk, value);
    emit_bytes(parser, chunk, OpCode::Constant as u8, constant);
}

fn end_compiler(parser: &Parser, chunk: &mut Chunk) {
    emit_return(parser, chunk);

    #[cfg(feature = "debug_print_code")]
    {
        if !parser.had_error {
            disassemble_chunk(chunk, "code");
        }
    }
}

fn binary<'a>(parser: &mut Parser<'a>, scanner: &mut Scanner<'a>, chunk: &mut Chunk) {
    let operator_type = parser.previous.token_type;

    let rule = get_rule(operator_type);
    let next_precedence =
        unsafe { std::mem::transmute::<u8, Precedence>(rule.precedence as u8 + 1) };
    parse_precedence(parser, scanner, chunk, next_precedence);

    match operator_type {
        TokenType::BangEqual => emit_bytes(parser, chunk, OpCode::Equal as u8, OpCode::Not as u8),
        TokenType::EqualEqual => emit_byte(parser, chunk, OpCode::Equal as u8),
        TokenType::Greater => emit_byte(parser, chunk, OpCode::Greater as u8),
        TokenType::GreaterEqual => emit_bytes(parser, chunk, OpCode::Less as u8, OpCode::Not as u8),
        TokenType::Less => emit_byte(parser, chunk, OpCode::Less as u8),
        TokenType::LessEqual => emit_bytes(parser, chunk, OpCode::Greater as u8, OpCode::Not as u8),
        TokenType::Plus => emit_byte(parser, chunk, OpCode::Add as u8),
        TokenType::Minus => emit_byte(parser, chunk, OpCode::Subtract as u8),
        TokenType::Star => emit_byte(parser, chunk, OpCode::Multiply as u8),
        TokenType::Slash => emit_byte(parser, chunk, OpCode::Divide as u8),
        _ => unreachable!(),
    }
}

fn literal<'a>(parser: &mut Parser<'a>, _: &mut Scanner<'a>, chunk: &mut Chunk) {
    match parser.previous.token_type {
        TokenType::False => emit_byte(parser, chunk, OpCode::False as u8),
        TokenType::Nil => emit_byte(parser, chunk, OpCode::Nil as u8),
        TokenType::True => emit_byte(parser, chunk, OpCode::True as u8),
        _ => unreachable!(),
    }
}

fn grouping<'a>(parser: &mut Parser<'a>, scanner: &mut Scanner<'a>, chunk: &mut Chunk) {
    expression(parser, scanner, chunk);
    consume(
        parser,
        scanner,
        TokenType::RightParen,
        "Expect ')' after expression.",
    );
}

fn number<'a>(parser: &mut Parser<'a>, _: &mut Scanner<'a>, chunk: &mut Chunk) {
    let value: f64 = parser.previous.start[..parser.previous.length]
        .parse()
        .unwrap();
    emit_constant(parser, chunk, Value::Number(value));
}

fn unary<'a>(parser: &mut Parser<'a>, scanner: &mut Scanner<'a>, chunk: &mut Chunk) {
    let operator_type = parser.previous.token_type;

    parse_precedence(parser, scanner, chunk, Precedence::Unary);

    match operator_type {
        TokenType::Bang => emit_byte(parser, chunk, OpCode::Not as u8),
        TokenType::Minus => emit_byte(parser, chunk, OpCode::Negate as u8),
        _ => unreachable!(),
    }
}

fn parse_precedence<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    precedence: Precedence,
) {
    advance(parser, scanner);

    let prefix_rule = get_rule(parser.previous.token_type).prefix;
    match prefix_rule {
        None => {
            error(parser, "Expect expression.");
            return;
        }
        Some(prefix_fn) => prefix_fn(parser, scanner, chunk),
    }

    while precedence <= get_rule(parser.current.token_type).precedence {
        advance(parser, scanner);
        let infix_rule = get_rule(parser.previous.token_type).infix;
        if let Some(infix_fn) = infix_rule {
            infix_fn(parser, scanner, chunk);
        }
    }
}

fn get_rule(token_type: TokenType) -> &'static ParseRule {
    &RULES[token_type as usize]
}
