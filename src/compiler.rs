use crate::chunk::{Chunk, OpCode};
#[cfg(feature = "debug_print_code")]
use crate::debug::disassemble_chunk;
use crate::object::{Obj, copy_string};
use crate::scanner::{Scanner, Token, TokenType};
use crate::value::Value;
use crate::vm::VM;

struct Parser<'a> {
    current: Token<'a>,
    previous: Token<'a>,
    had_error: bool,
    panic_mode: bool,
    compiler: Compiler<'a>,
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

type ParseFn = for<'a> fn(&mut Parser<'a>, &mut Scanner<'a>, &mut Chunk, &mut VM, bool);

struct ParseRule {
    prefix: Option<ParseFn>,
    infix: Option<ParseFn>,
    precedence: Precedence,
}

#[derive(Clone, Copy)]
struct Local<'a> {
    name: Token<'a>,
    depth: i32,
}

struct Compiler<'a> {
    locals: [Local<'a>; 256],
    local_count: usize,
    scope_depth: i32,
}

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

pub fn compile(source: &str, chunk: &mut Chunk, vm: &mut VM) -> bool {
    let mut scanner = Scanner::new(source);
    let dummy = Token {
        token_type: TokenType::Eof,
        start: "",
        length: 0,
        line: 0,
    };
    let dummy_local = Local {
        name: dummy,
        depth: -1,
    };
    let compiler = Compiler {
        locals: [dummy_local; 256],
        local_count: 0,
        scope_depth: 0,
    };
    let mut parser = Parser {
        current: dummy,
        previous: dummy,
        had_error: false,
        panic_mode: false,
        compiler,
    };

    advance(&mut parser, &mut scanner);

    while !match_token(&mut parser, &mut scanner, TokenType::Eof) {
        declaration(&mut parser, &mut scanner, chunk, vm);
    }

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

fn expression<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    parse_precedence(parser, scanner, chunk, Precedence::Assignment, vm);
}

fn block<'a>(parser: &mut Parser<'a>, scanner: &mut Scanner<'a>, chunk: &mut Chunk, vm: &mut VM) {
    while !check(parser, TokenType::RightBrace) && !check(parser, TokenType::Eof) {
        declaration(parser, scanner, chunk, vm);
    }

    consume(
        parser,
        scanner,
        TokenType::RightBrace,
        "Expect '}' after block.",
    );
}

fn begin_scope(parser: &mut Parser) {
    parser.compiler.scope_depth += 1;
}

fn end_scope(parser: &mut Parser, chunk: &mut Chunk) {
    parser.compiler.scope_depth -= 1;

    // While there are still locals, and the top local is declared,
    // inside the scope we are leaving,
    while parser.compiler.local_count > 0
        && parser.compiler.locals[parser.compiler.local_count - 1].depth
            > parser.compiler.scope_depth
    {
        // Clean off the stack at runtime
        emit_byte(parser, chunk, OpCode::Pop as u8);
        parser.compiler.local_count -= 1;
    }
}

fn var_declaration<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    let global = parse_variable(parser, scanner, chunk, vm, "Expect variable name.");

    if match_token(parser, scanner, TokenType::Equal) {
        expression(parser, scanner, chunk, vm);
    } else {
        emit_byte(parser, chunk, OpCode::Nil as u8);
    }

    consume(
        parser,
        scanner,
        TokenType::Semicolon,
        "Expect ';' after variable declaration.",
    );

    define_variable(parser, chunk, global);
}

fn expression_statement<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    expression(parser, scanner, chunk, vm);
    consume(
        parser,
        scanner,
        TokenType::Semicolon,
        "Expect ';' after expression.",
    );
    emit_byte(parser, chunk, OpCode::Pop as u8);
}

fn print_statement<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    expression(parser, scanner, chunk, vm);
    consume(
        parser,
        scanner,
        TokenType::Semicolon,
        "Expect ';' after value.",
    );
    emit_byte(parser, chunk, OpCode::Print as u8);
}

fn synchronize<'a>(parser: &mut Parser<'a>, scanner: &mut Scanner<'a>) {
    parser.panic_mode = false;

    while parser.current.token_type != TokenType::Eof {
        if parser.previous.token_type == TokenType::Semicolon {
            return;
        }

        match parser.current.token_type {
            TokenType::Class
            | TokenType::Fun
            | TokenType::Var
            | TokenType::For
            | TokenType::If
            | TokenType::While
            | TokenType::Print
            | TokenType::Return => return,
            _ => (),
        }

        advance(parser, scanner);
    }
}

fn declaration<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    if match_token(parser, scanner, TokenType::Var) {
        var_declaration(parser, scanner, chunk, vm);
    } else {
        statement(parser, scanner, chunk, vm);
    }

    if parser.panic_mode {
        synchronize(parser, scanner);
    }
}

fn statement<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    if match_token(parser, scanner, TokenType::Print) {
        print_statement(parser, scanner, chunk, vm);
    } else if match_token(parser, scanner, TokenType::LeftBrace) {
        begin_scope(parser);
        block(parser, scanner, chunk, vm);
        end_scope(parser, chunk);
    } else {
        expression_statement(parser, scanner, chunk, vm);
    }
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

fn match_token<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    token_type: TokenType,
) -> bool {
    if !check(parser, token_type) {
        return false;
    }
    advance(parser, scanner);
    true
}

fn check(parser: &Parser, token_type: TokenType) -> bool {
    parser.current.token_type == token_type
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

fn binary<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    _: bool,
) {
    let operator_type = parser.previous.token_type;

    let rule = get_rule(operator_type);
    let next_precedence =
        unsafe { std::mem::transmute::<u8, Precedence>(rule.precedence as u8 + 1) };
    parse_precedence(parser, scanner, chunk, next_precedence, vm);

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

fn literal<'a>(
    parser: &mut Parser<'a>,
    _: &mut Scanner<'a>,
    chunk: &mut Chunk,
    _: &mut VM,
    _: bool,
) {
    match parser.previous.token_type {
        TokenType::False => emit_byte(parser, chunk, OpCode::False as u8),
        TokenType::Nil => emit_byte(parser, chunk, OpCode::Nil as u8),
        TokenType::True => emit_byte(parser, chunk, OpCode::True as u8),
        _ => unreachable!(),
    }
}

fn grouping<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    _: bool,
) {
    expression(parser, scanner, chunk, vm);
    consume(
        parser,
        scanner,
        TokenType::RightParen,
        "Expect ')' after expression.",
    );
}

fn number<'a>(
    parser: &mut Parser<'a>,
    _: &mut Scanner<'a>,
    chunk: &mut Chunk,
    _: &mut VM,
    _: bool,
) {
    let value: f64 = parser.previous.start[..parser.previous.length]
        .parse()
        .unwrap();
    emit_constant(parser, chunk, Value::Number(value));
}

fn string<'a>(
    parser: &mut Parser<'a>,
    _: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    _: bool,
) {
    let s = &parser.previous.start[1..parser.previous.length - 1];
    let ptr = copy_string(vm, s);
    emit_constant(parser, chunk, Value::Obj(ptr as *mut Obj));
}

fn variable<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    can_assign: bool,
) {
    let name = parser.previous;
    named_variable(parser, scanner, chunk, vm, name, can_assign);
}

fn named_variable<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    name: Token<'a>,
    can_assign: bool,
) {
    let (arg, get_op, set_op) = if let Some(local_arg) = resolve_local(parser, &name) {
        (local_arg, OpCode::GetLocal as u8, OpCode::SetLocal as u8)
    } else {
        let global_arg = identifier_constant(parser, chunk, vm, name);
        (global_arg, OpCode::GetGlobal as u8, OpCode::SetGlobal as u8)
    };

    if can_assign && match_token(parser, scanner, TokenType::Equal) {
        expression(parser, scanner, chunk, vm);
        emit_bytes(parser, chunk, OpCode::SetGlobal as u8, arg);
    } else {
        emit_bytes(parser, chunk, OpCode::GetGlobal as u8, arg);
    }
}

fn unary<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    _: bool,
) {
    let operator_type = parser.previous.token_type;

    parse_precedence(parser, scanner, chunk, Precedence::Unary, vm);

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
    vm: &mut VM,
) {
    advance(parser, scanner);

    let prefix_rule = get_rule(parser.previous.token_type).prefix;
    let prefix_fn = match prefix_rule {
        Some(func) => func,
        None => {
            error(parser, "Expect expression.");
            return;
        }
    };

    let can_assign = precedence <= Precedence::Assignment;

    prefix_fn(parser, scanner, chunk, vm, can_assign);

    while precedence <= get_rule(parser.current.token_type).precedence {
        advance(parser, scanner);
        let infix_rule = get_rule(parser.previous.token_type).infix;
        if let Some(infix_fn) = infix_rule {
            infix_fn(parser, scanner, chunk, vm, can_assign);
        }
    }

    if can_assign && match_token(parser, scanner, TokenType::Equal) {
        error(parser, "Invalid assignment target.");
    }
}

fn identifier_constant<'a>(
    parser: &mut Parser,
    chunk: &mut Chunk,
    vm: &mut VM,
    name: Token<'a>,
) -> u8 {
    let s = &name.start[..name.length];
    let ptr = copy_string(vm, s);
    make_constant(parser, chunk, Value::Obj(ptr as *mut Obj))
}

fn identifiers_equal(a: &Token, b: &Token) -> bool {
    if a.length != b.length {
        return false;
    }
    &a.start[..a.length] == &b.start[..b.length]
}

fn resolve_local<'a>(parser: &mut Parser<'a>, name: &Token<'a>) -> Option<u8> {
    for i in (0..parser.compiler.local_count).rev() {
        let local = parser.compiler.locals[i];

        if identifiers_equal(name, &local.name) {
            if local.depth == -1 {
                error(parser, "Can't read local variable in its own initializer.");
            }
            return Some(i as u8);
        }
    }

    None
}

fn add_local<'a>(parser: &mut Parser<'a>, name: Token<'a>) {
    if parser.compiler.local_count == 256 {
        error(parser, "Too many local variables in function.");
        return;
    }

    let local = &mut parser.compiler.locals[parser.compiler.local_count];
    local.name = name;
    local.depth = -1;
    parser.compiler.local_count += 1;
}

fn declare_variable<'a>(parser: &mut Parser<'a>) {
    if parser.compiler.scope_depth == 0 {
        return;
    }

    let name = parser.previous;

    for i in (0..parser.compiler.local_count).rev() {
        let local = parser.compiler.locals[i];

        if local.depth != -1 && local.depth < parser.compiler.scope_depth {
            break;
        }

        if identifiers_equal(&name, &local.name) {
            error(parser, "Already a variable with this name in this scope.");
        }
    }

    add_local(parser, name);
}

fn parse_variable<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    error_message: &str,
) -> u8 {
    consume(parser, scanner, TokenType::Identifier, error_message);

    declare_variable(parser);

    if parser.compiler.scope_depth > 0 {
        return 0;
    }

    let name = parser.previous;
    identifier_constant(parser, chunk, vm, name)
}

fn mark_initialized(parser: &mut Parser) {
    //if parser.compiler.scope_depth == 0 {
    //   return;
    //}

    let local_count = parser.compiler.local_count;
    parser.compiler.locals[local_count - 1].depth = parser.compiler.scope_depth;
}

fn define_variable(parser: &mut Parser, chunk: &mut Chunk, global: u8) {
    if parser.compiler.scope_depth > 0 {
        mark_initialized(parser);
        return;
    }

    emit_bytes(parser, chunk, OpCode::DefineGlobal as u8, global);
}

fn get_rule(token_type: TokenType) -> ParseRule {
    match token_type {
        TokenType::LeftParen => ParseRule {
            prefix: Some(grouping),
            infix: None,
            precedence: Precedence::None,
        },
        TokenType::Minus => ParseRule {
            prefix: Some(unary),
            infix: Some(binary),
            precedence: Precedence::Term,
        },
        TokenType::Plus => ParseRule {
            prefix: None,
            infix: Some(binary),
            precedence: Precedence::Term,
        },
        TokenType::Slash => ParseRule {
            prefix: None,
            infix: Some(binary),
            precedence: Precedence::Factor,
        },
        TokenType::Star => ParseRule {
            prefix: None,
            infix: Some(binary),
            precedence: Precedence::Factor,
        },
        TokenType::Bang => ParseRule {
            prefix: Some(unary),
            infix: None,
            precedence: Precedence::None,
        },
        TokenType::BangEqual => ParseRule {
            prefix: None,
            infix: Some(binary),
            precedence: Precedence::Equality,
        },
        TokenType::EqualEqual => ParseRule {
            prefix: None,
            infix: Some(binary),
            precedence: Precedence::Equality,
        },
        TokenType::Greater => ParseRule {
            prefix: None,
            infix: Some(binary),
            precedence: Precedence::Comparison,
        },
        TokenType::GreaterEqual => ParseRule {
            prefix: None,
            infix: Some(binary),
            precedence: Precedence::Comparison,
        },
        TokenType::Less => ParseRule {
            prefix: None,
            infix: Some(binary),
            precedence: Precedence::Comparison,
        },
        TokenType::LessEqual => ParseRule {
            prefix: None,
            infix: Some(binary),
            precedence: Precedence::Comparison,
        },
        TokenType::String => ParseRule {
            prefix: Some(string),
            infix: None,
            precedence: Precedence::None,
        },
        TokenType::Identifier => ParseRule {
            prefix: Some(variable),
            infix: None,
            precedence: Precedence::None,
        },
        TokenType::Number => ParseRule {
            prefix: Some(number),
            infix: None,
            precedence: Precedence::None,
        },
        TokenType::False | TokenType::Nil | TokenType::True => ParseRule {
            prefix: Some(literal),
            infix: None,
            precedence: Precedence::None,
        },

        // Group all currently inactive tokens here.
        TokenType::RightParen
        | TokenType::LeftBrace
        | TokenType::RightBrace
        | TokenType::Comma
        | TokenType::Dot
        | TokenType::Semicolon
        | TokenType::Equal
        | TokenType::And
        | TokenType::Class
        | TokenType::Else
        | TokenType::For
        | TokenType::Fun
        | TokenType::If
        | TokenType::Or
        | TokenType::Print
        | TokenType::Return
        | TokenType::Super
        | TokenType::This
        | TokenType::Var
        | TokenType::While
        | TokenType::Error
        | TokenType::Eof => ParseRule {
            prefix: None,
            infix: None,
            precedence: Precedence::None,
        },
    }
}
