use std::collections::HashMap;
use std::process::exit;

use crate::chunk::{Chunk, OpCode};
#[cfg(feature = "debug_print_code")]
use crate::debug::disassemble_chunk;
use crate::object::{Obj, ObjFunction, ObjString, allocate_function, copy_string};
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

#[derive(Copy, Clone, PartialEq)]
pub enum FunctionType {
    Function,
    Script,
}

struct Compiler<'a> {
    enclosing: Option<Box<Compiler<'a>>>,
    function: *mut ObjFunction,
    function_type: FunctionType,

    locals: Vec<Local<'a>>,
    local_count: usize,
    scope_depth: i32,
    locals_map: HashMap<&'a str, Vec<usize>>,
}

fn init_compiler<'a>(parser: &mut Parser<'a>, vm: &mut VM, func_type: FunctionType) {
    let func_ptr = allocate_function(vm);

    if func_type != FunctionType::Script {
        let name_str = &parser.previous.start[..parser.previous.length];
        let name_obj = copy_string(vm, name_str);
        unsafe { (*func_ptr).name = name_obj };
    }

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

    let mut new_compiler = Compiler {
        enclosing: None,
        function: func_ptr,
        function_type: func_type,
        locals: vec![dummy_local; u16::MAX as usize],
        local_count: 1,
        scope_depth: 0,
        locals_map: HashMap::new(),
    };
    new_compiler.locals[0].depth = 0;
    new_compiler.locals[0].name.start = "";
    new_compiler.locals[0].name.length = 0;

    let old_compiler = std::mem::replace(&mut parser.compiler, new_compiler);
    parser.compiler.enclosing = Some(Box::new(old_compiler));
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

pub fn compile(source: &str, vm: &mut VM) -> Option<*mut ObjFunction> {
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

    let function = allocate_function(vm);

    let mut compiler = Compiler {
        enclosing: None,
        function,
        function_type: FunctionType::Script,
        locals: vec![dummy_local; u16::MAX as usize],
        local_count: 1,
        scope_depth: 0,
        locals_map: HashMap::new(),
    };

    compiler.locals[0].depth = 0;
    compiler.locals[0].name.start = "";
    compiler.locals[0].name.length = 0;

    let mut parser = Parser {
        current: dummy,
        previous: dummy,
        had_error: false,
        panic_mode: false,
        compiler,
    };

    advance(&mut parser, &mut scanner);

    let chunk = unsafe { &mut (*function).chunk };

    while !match_token(&mut parser, &mut scanner, TokenType::Eof) {
        declaration(&mut parser, &mut scanner, chunk, vm);
    }

    let compiled_fn = end_compiler(&mut parser, chunk);
    if parser.had_error {
        None
    } else {
        Some(compiled_fn)
    }
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

fn function<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    func_type: FunctionType,
) {
    init_compiler(parser, vm, func_type);
    begin_scope(parser);

    consume(
        parser,
        scanner,
        TokenType::LeftParen,
        "Expect '(' after function name.",
    );

    let new_chunk = unsafe { &mut (*parser.compiler.function).chunk };

    if !check(parser, TokenType::RightParen) {
        loop {
            unsafe {
                (*parser.compiler.function).arity += 1;
                if (*parser.compiler.function).arity > 255 {
                    error_at_current(parser, "Cannot have more than 255 parameters.");
                }
            }

            let constant = parse_variable(parser, scanner, new_chunk, vm, "Expect parameter name.");
            define_variable(parser, chunk, constant);

            if !match_token(parser, scanner, TokenType::Comma) {
                break;
            }
        }
    }

    consume(
        parser,
        scanner,
        TokenType::RightParen,
        "Expect ')' after parameters.",
    );
    consume(
        parser,
        scanner,
        TokenType::LeftBrace,
        "Expect '{' before function body.",
    );

    block(parser, scanner, new_chunk, vm);

    let function_ptr = end_compiler(parser, new_chunk);
    emit_constant(parser, chunk, Value::Obj(function_ptr as *mut Obj));
}

fn fun_declaration<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    let global = parse_variable(parser, scanner, chunk, vm, "Expect function name.");
    mark_initialized(parser);
    function(parser, scanner, chunk, vm, FunctionType::Function);
    define_variable(parser, chunk, global);
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
        // Remove variable from map
        let index = parser.compiler.local_count - 1;
        let local = parser.compiler.locals[index];
        let name_str = &local.name.start[..local.name.length];

        if let Some(stack) = parser.compiler.locals_map.get_mut(name_str) {
            stack.pop();
            if stack.is_empty() {
                parser.compiler.locals_map.remove(name_str);
            }
        }

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

fn for_statement<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    begin_scope(parser);
    consume(
        parser,
        scanner,
        TokenType::LeftParen,
        "Expect '(' after 'for'.",
    );
    // Initializer
    if match_token(parser, scanner, TokenType::Semicolon) {
        // No initializer
    } else if match_token(parser, scanner, TokenType::Var) {
        var_declaration(parser, scanner, chunk, vm);
    } else {
        expression_statement(parser, scanner, chunk, vm);
    }
    let mut loop_start = chunk.code.len();
    let mut exit_jump: Option<usize> = None;
    // Condition
    if !match_token(parser, scanner, TokenType::Semicolon) {
        expression(parser, scanner, chunk, vm);
        consume(
            parser,
            scanner,
            TokenType::Semicolon,
            "Expect ';' after loop condition.",
        );

        // Jump out of the loop if the condition is false.
        exit_jump = Some(emit_jump(parser, chunk, OpCode::JumpIfFalse as u8));
        emit_byte(parser, chunk, OpCode::Pop as u8);
    }
    // Increment
    if !match_token(parser, scanner, TokenType::RightParen) {
        let body_jump = emit_jump(parser, chunk, OpCode::Jump as u8);
        let increment_start = chunk.code.len();
        expression(parser, scanner, chunk, vm);
        emit_byte(parser, chunk, OpCode::Pop as u8);
        consume(
            parser,
            scanner,
            TokenType::RightParen,
            "Expect ')' after for clauses.",
        );

        emit_loop(parser, chunk, loop_start);
        loop_start = increment_start;
        patch_jump(parser, chunk, body_jump);
    }
    // Body
    statement(parser, scanner, chunk, vm);
    emit_loop(parser, chunk, loop_start);

    if let Some(jump) = exit_jump {
        patch_jump(parser, chunk, jump);
        emit_byte(parser, chunk, OpCode::Pop as u8);
    }

    end_scope(parser, chunk);
}

fn if_statement<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    consume(
        parser,
        scanner,
        TokenType::LeftParen,
        "Expect '(' after 'if'.",
    );
    expression(parser, scanner, chunk, vm);
    consume(
        parser,
        scanner,
        TokenType::RightParen,
        "Expect '(' after condition.",
    );

    let then_jump = emit_jump(parser, chunk, OpCode::JumpIfFalse as u8);
    emit_byte(parser, chunk, OpCode::Pop as u8);
    statement(parser, scanner, chunk, vm);

    let else_jump = emit_jump(parser, chunk, OpCode::Jump as u8);

    patch_jump(parser, chunk, then_jump);
    emit_byte(parser, chunk, OpCode::Pop as u8);

    if match_token(parser, scanner, TokenType::Else) {
        statement(parser, scanner, chunk, vm);
    }
    patch_jump(parser, chunk, else_jump);
}

fn switch_statement<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    consume(
        parser,
        scanner,
        TokenType::LeftParen,
        "Expect '(' after 'switch'",
    );
    expression(parser, scanner, chunk, vm);
    consume(
        parser,
        scanner,
        TokenType::RightParen,
        "Expect ')' after condition.",
    );
    consume(
        parser,
        scanner,
        TokenType::LeftBrace,
        "Expect '{' before switch case.",
    );

    begin_scope(parser);

    let mut exit_jumps = Vec::new();
    let mut has_default = false;

    while !check(parser, TokenType::RightBrace) && !check(parser, TokenType::Eof) {
        if match_token(parser, scanner, TokenType::Case) {
            if has_default {
                error(parser, "Cannot have 'case' after 'default'.");
            }

            emit_byte(parser, chunk, OpCode::Dup as u8);
            expression(parser, scanner, chunk, vm);
            consume(
                parser,
                scanner,
                TokenType::Colon,
                "Expect ':' after 'case' value.",
            );

            emit_byte(parser, chunk, OpCode::Equal as u8);
            let next_case_jump = emit_jump(parser, chunk, OpCode::JumpIfFalse as u8);

            emit_byte(parser, chunk, OpCode::Pop as u8);

            while !check(parser, TokenType::Case)
                && !check(parser, TokenType::Default)
                && !check(parser, TokenType::RightBrace)
                && !check(parser, TokenType::Eof)
            {
                declaration(parser, scanner, chunk, vm);
            }

            let exit_jump = emit_jump(parser, chunk, OpCode::Jump as u8);
            exit_jumps.push(exit_jump);

            patch_jump(parser, chunk, next_case_jump);

            emit_byte(parser, chunk, OpCode::Pop as u8);
        } else if match_token(parser, scanner, TokenType::Default) {
            if has_default {
                error(parser, "Cannot have more than one 'default' case.");
            }
            has_default = true;
            consume(
                parser,
                scanner,
                TokenType::Colon,
                "Expect ':' after 'default'.",
            );

            while !check(parser, TokenType::Case)
                && !check(parser, TokenType::Default)
                && !check(parser, TokenType::RightBrace)
                && !check(parser, TokenType::Eof)
            {
                declaration(parser, scanner, chunk, vm);
            }
        } else {
            error_at_current(parser, "Expect 'case' or 'default'.");
            break;
        }
    }

    consume(
        parser,
        scanner,
        TokenType::RightBrace,
        "Expect '}' after switch case.",
    );

    for jump in exit_jumps {
        patch_jump(parser, chunk, jump);
    }

    emit_byte(parser, chunk, OpCode::Pop as u8);

    end_scope(parser, chunk);
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

fn return_statement<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    if parser.compiler.function_type == FunctionType::Script {
        error(parser, "Cannot return from top-level code.");
    }

    if match_token(parser, scanner, TokenType::Semicolon) {
        emit_return(parser, chunk);
    } else {
        expression(parser, scanner, chunk, vm);
        consume(
            parser,
            scanner,
            TokenType::Semicolon,
            "Expect ';' after return value.",
        );
        emit_byte(parser, chunk, OpCode::Return as u8);
    }
}

fn while_statement<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    let loop_start = chunk.code.len();
    consume(
        parser,
        scanner,
        TokenType::LeftParen,
        "Expect '(' after 'while'.",
    );
    expression(parser, scanner, chunk, vm);
    consume(
        parser,
        scanner,
        TokenType::RightParen,
        "Expect ')' after condition.",
    );

    let exit_jump = emit_jump(parser, chunk, OpCode::JumpIfFalse as u8);
    emit_byte(parser, chunk, OpCode::Pop as u8);
    statement(parser, scanner, chunk, vm);
    emit_loop(parser, chunk, loop_start);

    patch_jump(parser, chunk, exit_jump);
    emit_byte(parser, chunk, OpCode::Pop as u8);
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
    if match_token(parser, scanner, TokenType::Fun) {
        fun_declaration(parser, scanner, chunk, vm);
    } else if match_token(parser, scanner, TokenType::Var) {
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
    } else if match_token(parser, scanner, TokenType::Return) {
        return_statement(parser, scanner, chunk, vm);
    } else if match_token(parser, scanner, TokenType::For) {
        for_statement(parser, scanner, chunk, vm);
    } else if match_token(parser, scanner, TokenType::If) {
        if_statement(parser, scanner, chunk, vm);
    } else if match_token(parser, scanner, TokenType::Switch) {
        switch_statement(parser, scanner, chunk, vm);
    } else if match_token(parser, scanner, TokenType::While) {
        while_statement(parser, scanner, chunk, vm);
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

fn emit_loop(parser: &mut Parser, chunk: &mut Chunk, loop_start: usize) {
    emit_byte(parser, chunk, OpCode::Loop as u8);

    let offset = chunk.code.len() - loop_start + 2;
    if offset > u16::MAX as usize {
        error(parser, "Loop body too large");
    }

    emit_byte(parser, chunk, (offset & 0xff) as u8);
    emit_byte(parser, chunk, ((offset >> 8) & 0xff) as u8);
}

fn emit_jump(parser: &mut Parser, chunk: &mut Chunk, instruction: u8) -> usize {
    emit_byte(parser, chunk, instruction);
    // Placeholder offset
    emit_byte(parser, chunk, 0xff);
    emit_byte(parser, chunk, 0xff);

    chunk.code.len() - 2
}

fn emit_return(parser: &Parser, chunk: &mut Chunk) {
    emit_byte(parser, chunk, OpCode::Nil as u8);
    emit_byte(parser, chunk, OpCode::Return as u8);
}

fn make_constant(parser: &mut Parser, chunk: &mut Chunk, value: Value) -> usize {
    chunk.add_constant(value)
}

fn emit_constant(parser: &mut Parser, chunk: &mut Chunk, value: Value) {
    let constant = make_constant(parser, chunk, value);

    if constant <= 255 {
        emit_bytes(parser, chunk, OpCode::Constant as u8, constant as u8);
    } else {
        emit_byte(parser, chunk, OpCode::ConstantLong as u8);
        emit_byte(parser, chunk, (constant & 0xFF) as u8);
        emit_byte(parser, chunk, ((constant >> 8) & 0xFF) as u8);
        emit_byte(parser, chunk, ((constant >> 16) & 0xFF) as u8);
    }
}

fn patch_jump(parser: &mut Parser, chunk: &mut Chunk, offset: usize) {
    // -2 to adjust for the bytecode for the jump offset itself
    let jump = chunk.code.len() - offset - 2;

    if jump > u16::MAX as usize {
        error(parser, "Too much code to jump over.");
    }

    chunk.code[offset] = (jump & 0xff) as u8;
    chunk.code[offset + 1] = ((jump >> 8) & 0xff) as u8;
}

fn end_compiler(parser: &mut Parser, chunk: &mut Chunk) -> *mut ObjFunction {
    emit_return(parser, chunk);

    let function = parser.compiler.function;

    #[cfg(feature = "debug_print_code")]
    {
        if !parser.had_error {
            let name = if unsafe { (*function).name.is_null() } {
                "<script>"
            } else {
                unsafe { ObjString::as_str((*function).name) }
            };

            disassemble_chunk(chunk, name);
        }
    }

    if let Some(enclosing) = parser.compiler.enclosing.take() {
        parser.compiler = *enclosing;
    }

    function
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
        TokenType::Backslash => emit_byte(parser, chunk, OpCode::IntDivide as u8),
        _ => unreachable!(),
    }
}

fn call<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    _: bool,
) {
    let arg_count = argument_list(parser, scanner, chunk, vm);
    emit_bytes(parser, chunk, OpCode::Call as u8, arg_count);
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

fn or<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    _: bool,
) {
    let else_jump = emit_jump(parser, chunk, OpCode::JumpIfFalse as u8);
    let end_jump = emit_jump(parser, chunk, OpCode::Jump as u8);

    patch_jump(parser, chunk, else_jump);
    emit_byte(parser, chunk, OpCode::Pop as u8);

    parse_precedence(parser, scanner, chunk, Precedence::Or, vm);
    patch_jump(parser, chunk, end_jump);
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

fn list<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    _: bool,
) {
    let mut item_count = 0;

    if !check(parser, TokenType::RightBracket) {
        loop {
            expression(parser, scanner, chunk, vm);
            item_count += 1;

            if item_count > 255 {
                error(parser, "Error processing list literal.");
            }
            if !match_token(parser, scanner, TokenType::Comma) {
                break;
            }
        }
    }

    consume(
        parser,
        scanner,
        TokenType::RightBracket,
        "Expect ']' after list elements.",
    );

    if item_count <= 255 {
        emit_bytes(parser, chunk, OpCode::BuildList as u8, item_count as u8);
    } else {
        emit_byte(parser, chunk, OpCode::BuildListLong as u8);
        emit_byte(parser, chunk, (item_count & 0xFF) as u8);
        emit_byte(parser, chunk, ((item_count >> 8) & 0xFF) as u8);
    }
}

fn index<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    can_assign: bool,
) {
    expression(parser, scanner, chunk, vm);
    consume(
        parser,
        scanner,
        TokenType::RightBracket,
        "Expect ']' after index.",
    );

    if can_assign && match_token(parser, scanner, TokenType::Equal) {
        expression(parser, scanner, chunk, vm);
        emit_byte(parser, chunk, OpCode::SetIndex as u8);
    } else {
        emit_byte(parser, chunk, OpCode::GetIndex as u8);
    }
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
    let mut is_local = false;
    let mut arg = 0;

    if let Some(local_arg) = resolve_local(parser, &name) {
        is_local = true;
        arg = local_arg;
    } else {
        arg = identifier_constant(parser, chunk, vm, name) as usize;
    }

    let is_assignment = can_assign && match_token(parser, scanner, TokenType::Equal);
    if is_assignment {
        expression(parser, scanner, chunk, vm);
    }

    if is_local {
        if arg <= 255 {
            let op = if is_assignment {
                OpCode::SetLocal
            } else {
                OpCode::GetLocal
            };
            emit_bytes(parser, chunk, op as u8, arg as u8);
        } else {
            let op = if is_assignment {
                OpCode::SetLocalLong
            } else {
                OpCode::GetLocalLong
            };
            emit_byte(parser, chunk, op as u8);
            // Emit the 16-bit int as two little-endian bytes
            emit_byte(parser, chunk, (arg & 0xFF) as u8);
            emit_byte(parser, chunk, ((arg >> 8) & 0xFF) as u8);
        }
    } else {
        let op = if is_assignment {
            OpCode::SetGlobal
        } else {
            OpCode::GetGlobal
        };
        emit_bytes(parser, chunk, op as u8, arg as u8);
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
    let constant = make_constant(parser, chunk, Value::Obj(ptr as *mut Obj));

    if constant > u8::MAX as usize {
        error(parser, "Too many globals in one chunk.");
        return 0;
    }

    constant as u8
}

fn identifiers_equal(a: &Token, b: &Token) -> bool {
    if a.length != b.length {
        return false;
    }
    &a.start[..a.length] == &b.start[..b.length]
}

fn resolve_local<'a>(parser: &mut Parser<'a>, name: &Token<'a>) -> Option<usize> {
    let name_str = &name.start[..name.length];

    if let Some(stack) = parser.compiler.locals_map.get(name_str) {
        if let Some(&index) = stack.last() {
            let local = parser.compiler.locals[index];
            if local.depth == -1 {
                error(parser, "Can't read local variable in its own initializer.");
            }
            return Some(index);
        }
    }

    None
}

fn add_local<'a>(parser: &mut Parser<'a>, name: Token<'a>) {
    if parser.compiler.local_count == u16::MAX as usize {
        error(parser, "Too many local variables in function.");
        return;
    }

    let index = parser.compiler.local_count;
    let local = &mut parser.compiler.locals[parser.compiler.local_count];
    local.name = name;
    local.depth = -1;

    let name_str = &name.start[..name.length];
    parser
        .compiler
        .locals_map
        .entry(name_str)
        .or_default()
        .push(index);

    parser.compiler.local_count += 1;
}

fn declare_variable<'a>(parser: &mut Parser<'a>) {
    if parser.compiler.scope_depth == 0 {
        return;
    }

    let name = parser.previous;
    let name_str = &name.start[..name.length];

    if let Some(stack) = parser.compiler.locals_map.get(name_str) {
        if let Some(&index) = stack.last() {
            let local = parser.compiler.locals[index];
            if local.depth == -1 || local.depth == parser.compiler.scope_depth {
                error(parser, "Already a variable with this name in this scope.");
            }
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
    if parser.compiler.scope_depth == 0 {
        return;
    }

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

fn argument_list<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) -> u8 {
    let mut arg_count = 0;
    if !check(parser, TokenType::RightParen) {
        loop {
            expression(parser, scanner, chunk, vm);
            if arg_count == 255 {
                error(parser, "Cannot have more than 255 arguments.");
            }
            arg_count += 1;

            if !match_token(parser, scanner, TokenType::Comma) {
                break;
            }
        }
    }

    consume(
        parser,
        scanner,
        TokenType::RightParen,
        "Expect ')' after arguments.",
    );
    arg_count
}

fn and<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    _: bool,
) {
    let end_jump = emit_jump(parser, chunk, OpCode::JumpIfFalse as u8);

    emit_byte(parser, chunk, OpCode::Pop as u8);
    parse_precedence(parser, scanner, chunk, Precedence::And, vm);

    patch_jump(parser, chunk, end_jump);
}

fn get_rule(token_type: TokenType) -> ParseRule {
    match token_type {
        TokenType::LeftParen => ParseRule {
            prefix: Some(grouping),
            infix: Some(call),
            precedence: Precedence::Call,
        },
        TokenType::LeftBracket => ParseRule {
            prefix: Some(list),
            infix: Some(index),
            precedence: Precedence::Call,
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
        TokenType::Backslash => ParseRule {
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
        TokenType::And => ParseRule {
            prefix: None,
            infix: Some(and),
            precedence: Precedence::And,
        },
        TokenType::Or => ParseRule {
            prefix: None,
            infix: Some(or),
            precedence: Precedence::Or,
        },
        TokenType::False | TokenType::Nil | TokenType::True => ParseRule {
            prefix: Some(literal),
            infix: None,
            precedence: Precedence::None,
        },
        // For switch statement
        TokenType::Colon | TokenType::Switch | TokenType::Case | TokenType::Default => ParseRule {
            prefix: None,
            infix: None,
            precedence: Precedence::None,
        },

        // Group all currently inactive tokens here.
        TokenType::RightParen
        | TokenType::LeftBrace
        | TokenType::RightBrace
        | TokenType::RightBracket
        | TokenType::GreaterGreater
        | TokenType::GreaterGreaterGreater
        | TokenType::Comma
        | TokenType::Dot
        | TokenType::Semicolon
        | TokenType::Equal
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
