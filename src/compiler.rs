use std::collections::HashMap;

use crate::chunk::{Chunk, OpCode};
#[cfg(feature = "debug_print_code")]
use crate::debug::disassemble_chunk;
use crate::debug_info::{
    BindingDebugInfo, BindingId, BindingKind, DebugPoint, DebugPointId, DebugPointKind,
    FunctionDebugInfo, UpvalueDebugInfo,
};
use crate::object::{Obj, ObjFunction, ObjString, allocate_function, copy_string};
use crate::scanner::{Scanner, Token, TokenType};
use crate::value::Value;
use crate::vm::VM;
use crate::{
    Diagnostic, DiagnosticPhase, DiagnosticSeverity, RevisionId, RuntimeHost, SourceDocument,
    SourceId, SourceSpan,
};

const LOCALS_MAX: usize = u8::MAX as usize + 1;

struct Parser<'a> {
    current: Token<'a>,
    previous: Token<'a>,
    had_error: bool,
    panic_mode: bool,
    source_id: SourceId,
    revision: RevisionId,
    diagnostics: Vec<Diagnostic>,
    next_binding_id: u64,
    next_debug_point_id: u64,
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

#[derive(Clone)]
struct Local<'a> {
    name: Token<'a>,
    owned_name: String,
    depth: i32,
    is_captured: bool,
    binding_id: Option<BindingId>,
    declaration: SourceSpan,
    metadata_index: Option<usize>,
}

#[derive(Clone, PartialEq)]
struct Upvalue {
    index: u8,
    is_local: bool,
    binding_id: BindingId,
    name: String,
    declaration: SourceSpan,
    metadata_index: usize,
}

struct PendingPoint {
    id: DebugPointId,
    kind: DebugPointKind,
    span: SourceSpan,
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
    upvalues: Vec<Upvalue>,
    pending_points: Vec<PendingPoint>,
}

fn empty_span(source_id: SourceId, revision: RevisionId) -> SourceSpan {
    let position = crate::TextPosition {
        byte_offset: 0,
        line: 1,
        column: 1,
    };
    SourceSpan {
        source_id,
        revision,
        start: position,
        end: position,
    }
}

fn dummy_local<'a>(source_id: SourceId, revision: RevisionId) -> Local<'a> {
    Local {
        name: Token {
            token_type: TokenType::Eof,
            start: "",
            length: 0,
            line: 0,
            column: 0,
            start_position: crate::TextPosition {
                byte_offset: 0,
                line: 0,
                column: 0,
            },
            end_position: crate::TextPosition {
                byte_offset: 0,
                line: 0,
                column: 0,
            },
            error_message: None,
        },
        owned_name: String::new(),
        depth: 0,
        is_captured: false,
        binding_id: None,
        declaration: empty_span(source_id, revision),
        metadata_index: None,
    }
}

fn initialize_function_debug_info(
    function: *mut ObjFunction,
    source_id: SourceId,
    revision: RevisionId,
    declaration: SourceSpan,
    entry_id: DebugPointId,
) {
    unsafe {
        (*function).debug_info = FunctionDebugInfo {
            source_id,
            revision,
            declaration,
            points: vec![DebugPoint {
                id: entry_id,
                offset: 0,
                kind: DebugPointKind::FunctionEntry,
                span: declaration,
            }],
            bindings: Vec::new(),
            upvalues: Vec::new(),
        };
    }
}

fn next_debug_point_id(parser: &mut Parser<'_>) -> DebugPointId {
    let id = DebugPointId(parser.next_debug_point_id);
    parser.next_debug_point_id += 1;
    id
}

fn schedule_point(parser: &mut Parser<'_>, kind: DebugPointKind, span: SourceSpan) -> DebugPointId {
    let id = next_debug_point_id(parser);
    parser
        .compiler
        .pending_points
        .push(PendingPoint { id, kind, span });
    id
}

fn finish_point(parser: &mut Parser<'_>, id: DebugPointId, end: crate::TextPosition) {
    if let Some(point) = parser
        .compiler
        .pending_points
        .iter_mut()
        .find(|point| point.id == id)
    {
        point.span.end = end;
        return;
    }

    unsafe {
        if let Some(point) = (*parser.compiler.function)
            .debug_info
            .points
            .iter_mut()
            .find(|point| point.id == id)
        {
            point.span.end = end;
        }
    }
}

fn finish_previous_point(parser: &mut Parser<'_>, id: DebugPointId) {
    finish_point(parser, id, parser.previous.end_position);
}

fn init_compiler<'a>(parser: &mut Parser<'a>, vm: &mut VM, func_type: FunctionType) {
    let declaration = token_span(parser, parser.previous);
    let entry_id = next_debug_point_id(parser);
    let func_ptr = allocate_function(vm);
    vm.compiler_roots.push(func_ptr);
    initialize_function_debug_info(
        func_ptr,
        parser.source_id,
        parser.revision,
        declaration,
        entry_id,
    );

    if func_type != FunctionType::Script {
        let name_str = &parser.previous.start[..parser.previous.length];
        let name_obj = copy_string(vm, name_str);
        unsafe { (*func_ptr).name = name_obj };
    }

    let dummy_local = dummy_local(parser.source_id, parser.revision);

    let new_compiler = Compiler {
        enclosing: None,
        function: func_ptr,
        function_type: func_type,
        locals: vec![dummy_local; LOCALS_MAX],
        local_count: 1,
        scope_depth: 0,
        locals_map: HashMap::new(),
        upvalues: Vec::with_capacity(100),
        pending_points: Vec::new(),
    };

    let old_compiler = std::mem::replace(&mut parser.compiler, new_compiler);
    parser.compiler.enclosing = Some(Box::new(old_compiler));
}

fn current_chunk<'a>(chunk: &'a mut Chunk) -> &'a mut Chunk {
    chunk
}

fn error_at(parser: &mut Parser, token: &Token, message: &str) {
    let scanner_error = token.token_type == TokenType::Error;
    let (phase, code) = if scanner_error {
        (DiagnosticPhase::Scanner, "scanner.error")
    } else {
        (DiagnosticPhase::Parser, "parser.error")
    };
    report_at(parser, token, message, phase, code, scanner_error);
}

fn report_at(
    parser: &mut Parser,
    token: &Token,
    message: &str,
    phase: DiagnosticPhase,
    code: &str,
    bypass_panic_mode: bool,
) {
    if parser.panic_mode && !bypass_panic_mode {
        return;
    }
    parser.panic_mode = true;

    parser.diagnostics.push(Diagnostic {
        phase,
        severity: DiagnosticSeverity::Error,
        code: code.to_string(),
        message: message.to_string(),
        span: token.span(parser.source_id, parser.revision),
        frames: Vec::new(),
    });
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

fn compiler_error(parser: &mut Parser, message: &str) {
    let token = parser.previous;
    report_at(
        parser,
        &token,
        message,
        DiagnosticPhase::Compiler,
        "compiler.error",
        false,
    );
}

fn compiler_error_at_current(parser: &mut Parser, message: &str) {
    let token = parser.current;
    report_at(
        parser,
        &token,
        message,
        DiagnosticPhase::Compiler,
        "compiler.error",
        false,
    );
}

pub fn compile(
    document: &SourceDocument,
    vm: &mut VM,
    host: &mut dyn RuntimeHost,
) -> Option<*mut ObjFunction> {
    let mut scanner = Scanner::new(&document.text);
    let dummy = Token {
        token_type: TokenType::Eof,
        start: "",
        length: 0,
        line: 0,
        column: 0,
        start_position: crate::TextPosition {
            byte_offset: 0,
            line: 0,
            column: 0,
        },
        end_position: crate::TextPosition {
            byte_offset: 0,
            line: 0,
            column: 0,
        },
        error_message: None,
    };
    let dummy_local = dummy_local(document.id, document.revision);

    let function = allocate_function(vm);
    let script_declaration = SourceSpan {
        source_id: document.id,
        revision: document.revision,
        start: empty_span(document.id, document.revision).start,
        end: document.eof_span().end,
    };
    initialize_function_debug_info(
        function,
        document.id,
        document.revision,
        script_declaration,
        DebugPointId(0),
    );

    let compiler = Compiler {
        enclosing: None,
        function,
        function_type: FunctionType::Script,
        locals: vec![dummy_local; LOCALS_MAX],
        local_count: 1,
        scope_depth: 0,
        locals_map: HashMap::new(),
        upvalues: Vec::with_capacity(100),
        pending_points: Vec::new(),
    };

    let mut parser = Parser {
        current: dummy,
        previous: dummy,
        had_error: false,
        panic_mode: false,
        source_id: document.id,
        revision: document.revision,
        diagnostics: Vec::new(),
        next_binding_id: 0,
        next_debug_point_id: 1,
        compiler,
    };

    advance(&mut parser, &mut scanner);

    let chunk = unsafe { &mut (*function).chunk };

    while !match_token(&mut parser, &mut scanner, TokenType::Eof) {
        declaration(&mut parser, &mut scanner, chunk, vm);
    }

    let (compiled_fn, _) = end_compiler(&mut parser, chunk, vm);

    let had_error = parser.had_error;
    for diagnostic in parser.diagnostics.drain(..) {
        host.diagnostic(diagnostic);
    }

    if had_error { None } else { Some(compiled_fn) }
}

fn advance<'a>(parser: &mut Parser<'a>, scanner: &mut Scanner<'a>) {
    parser.previous = parser.current;

    loop {
        parser.current = scanner.scan_token();

        #[cfg(feature = "debug_print_tokens")]
        {
            if parser.current.line != parser.previous.line {
                eprint!("{:4} | ", parser.current.line);
            } else {
                eprint!("   | | ");
            }

            eprintln!(
                "{:3} | {:<20?} | '{}'",
                parser.current.column,
                parser.current.token_type,
                &parser.current.start[..parser.current.length]
            );
        }

        if parser.current.token_type != TokenType::Error {
            break;
        }

        error_at_current(
            parser,
            parser
                .current
                .error_message
                .unwrap_or("Unexpected scanner error."),
        );
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
) -> *mut ObjFunction {
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
                    compiler_error_at_current(parser, "Cannot have more than 255 parameters.");
                }
            }

            let constant = parse_variable(
                parser,
                scanner,
                new_chunk,
                vm,
                "Expect parameter name.",
                BindingKind::Parameter,
            );
            define_variable(parser, new_chunk, constant);

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

    let (function_ptr, upvalues) = end_compiler(parser, new_chunk, vm);
    let constant = make_constant(parser, chunk, Value::Obj(function_ptr as *mut Obj));
    if constant <= u8::MAX as usize {
        emit_opcode_with_byte(parser, chunk, OpCode::Closure, constant as u8);
    } else if constant <= 0x00ff_ffff {
        emit_opcode(parser, chunk, OpCode::ClosureLong);
        emit_byte_operand(parser, chunk, (constant & 0xff) as u8);
        emit_byte_operand(parser, chunk, ((constant >> 8) & 0xff) as u8);
        emit_byte_operand(parser, chunk, ((constant >> 16) & 0xff) as u8);
    } else {
        compiler_error(parser, "Too many constants in one chunk.");
    }

    for upvalue in &upvalues {
        emit_byte_operand(parser, chunk, if upvalue.is_local { 1 } else { 0 });
        emit_byte_operand(parser, chunk, upvalue.index);
    }

    function_ptr
}

fn fun_declaration<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    construct_start: crate::TextPosition,
) {
    let global = parse_variable(
        parser,
        scanner,
        chunk,
        vm,
        "Expect function name.",
        BindingKind::Local,
    );
    mark_resolver_initialized(parser);
    let function_ptr = function(parser, scanner, chunk, vm, FunctionType::Function);
    let declaration = SourceSpan {
        source_id: parser.source_id,
        revision: parser.revision,
        start: construct_start,
        end: parser.previous.end_position,
    };
    unsafe {
        (*function_ptr).debug_info.declaration = declaration;
        if let Some(entry) = (*function_ptr)
            .debug_info
            .points
            .iter_mut()
            .find(|point| point.kind == DebugPointKind::FunctionEntry)
        {
            entry.span = declaration;
        }
    }
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
        let local = parser.compiler.locals[index].clone();
        let name_str = &local.name.start[..local.name.length];

        if let Some(stack) = parser.compiler.locals_map.get_mut(name_str) {
            stack.pop();
            if stack.is_empty() {
                parser.compiler.locals_map.remove(name_str);
            }
        }

        if let Some(metadata_index) = local.metadata_index {
            unsafe {
                (&mut (*parser.compiler.function).debug_info.bindings)[metadata_index].live_end =
                    chunk.code.len();
            }
        }

        if local.is_captured {
            emit_opcode(parser, chunk, OpCode::CloseUpvalue);
        } else {
            emit_opcode(parser, chunk, OpCode::Pop);
        }
        parser.compiler.local_count -= 1;
    }
}

fn var_declaration<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    let global = parse_variable(
        parser,
        scanner,
        chunk,
        vm,
        "Expect variable name.",
        BindingKind::Local,
    );

    if match_token(parser, scanner, TokenType::Equal) {
        expression(parser, scanner, chunk, vm);
    } else {
        emit_opcode(parser, chunk, OpCode::Nil);
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
    emit_opcode(parser, chunk, OpCode::Pop);
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
        let span = token_span(parser, parser.previous);
        let point = schedule_point(parser, DebugPointKind::LoopInitializer, span);
        var_declaration(parser, scanner, chunk, vm);
        finish_previous_point(parser, point);
    } else {
        let span = token_span(parser, parser.current);
        let point = schedule_point(parser, DebugPointKind::LoopInitializer, span);
        expression_statement(parser, scanner, chunk, vm);
        finish_previous_point(parser, point);
    }
    let mut loop_start = chunk.code.len();
    let mut exit_jump: Option<usize> = None;
    // Condition
    if !match_token(parser, scanner, TokenType::Semicolon) {
        let span = token_span(parser, parser.current);
        let point = schedule_point(parser, DebugPointKind::LoopCondition, span);
        expression(parser, scanner, chunk, vm);
        consume(
            parser,
            scanner,
            TokenType::Semicolon,
            "Expect ';' after loop condition.",
        );
        finish_previous_point(parser, point);

        // Jump out of the loop if the condition is false.
        exit_jump = Some(emit_jump(parser, chunk, OpCode::JumpIfFalse));
        emit_opcode(parser, chunk, OpCode::Pop);
    }
    // Increment
    if !match_token(parser, scanner, TokenType::RightParen) {
        let body_jump = emit_jump(parser, chunk, OpCode::Jump);
        let increment_start = chunk.code.len();
        let span = token_span(parser, parser.current);
        let point = schedule_point(parser, DebugPointKind::LoopIncrement, span);
        expression(parser, scanner, chunk, vm);
        finish_previous_point(parser, point);
        emit_opcode(parser, chunk, OpCode::Pop);
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
        emit_opcode(parser, chunk, OpCode::Pop);
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
        "Expect ')' after condition.",
    );

    let then_jump = emit_jump(parser, chunk, OpCode::JumpIfFalse);
    emit_opcode(parser, chunk, OpCode::Pop);
    statement(parser, scanner, chunk, vm);

    let else_jump = emit_jump(parser, chunk, OpCode::Jump);

    patch_jump(parser, chunk, then_jump);
    emit_opcode(parser, chunk, OpCode::Pop);

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
                compiler_error(parser, "Cannot have 'case' after 'default'.");
            }

            emit_opcode(parser, chunk, OpCode::Dup);
            expression(parser, scanner, chunk, vm);
            consume(
                parser,
                scanner,
                TokenType::Colon,
                "Expect ':' after 'case' value.",
            );

            emit_opcode(parser, chunk, OpCode::Equal);
            let next_case_jump = emit_jump(parser, chunk, OpCode::JumpIfFalse);

            emit_opcode(parser, chunk, OpCode::Pop);

            while !check(parser, TokenType::Case)
                && !check(parser, TokenType::Default)
                && !check(parser, TokenType::RightBrace)
                && !check(parser, TokenType::Eof)
            {
                declaration(parser, scanner, chunk, vm);
            }

            let exit_jump = emit_jump(parser, chunk, OpCode::Jump);
            exit_jumps.push(exit_jump);

            patch_jump(parser, chunk, next_case_jump);

            emit_opcode(parser, chunk, OpCode::Pop);
        } else if match_token(parser, scanner, TokenType::Default) {
            if has_default {
                compiler_error(parser, "Cannot have more than one 'default' case.");
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

    emit_opcode(parser, chunk, OpCode::Pop);

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
    emit_opcode(parser, chunk, OpCode::Print);
}

fn return_statement<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
) {
    if parser.compiler.function_type == FunctionType::Script {
        compiler_error(parser, "Cannot return from top-level code.");
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
        emit_opcode(parser, chunk, OpCode::Return);
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

    let exit_jump = emit_jump(parser, chunk, OpCode::JumpIfFalse);
    emit_opcode(parser, chunk, OpCode::Pop);
    statement(parser, scanner, chunk, vm);
    emit_loop(parser, chunk, loop_start);

    patch_jump(parser, chunk, exit_jump);
    emit_opcode(parser, chunk, OpCode::Pop);
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
        let span = token_span(parser, parser.previous);
        let point = schedule_point(parser, DebugPointKind::Statement, span);
        fun_declaration(parser, scanner, chunk, vm, span.start);
        finish_previous_point(parser, point);
    } else if match_token(parser, scanner, TokenType::Var) {
        let span = token_span(parser, parser.previous);
        let point = schedule_point(parser, DebugPointKind::Statement, span);
        var_declaration(parser, scanner, chunk, vm);
        finish_previous_point(parser, point);
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
        let span = token_span(parser, parser.previous);
        let point = schedule_point(parser, DebugPointKind::Statement, span);
        print_statement(parser, scanner, chunk, vm);
        finish_previous_point(parser, point);
    } else if match_token(parser, scanner, TokenType::Return) {
        let span = token_span(parser, parser.previous);
        let point = schedule_point(parser, DebugPointKind::Statement, span);
        return_statement(parser, scanner, chunk, vm);
        finish_previous_point(parser, point);
    } else if match_token(parser, scanner, TokenType::For) {
        for_statement(parser, scanner, chunk, vm);
    } else if match_token(parser, scanner, TokenType::If) {
        let span = token_span(parser, parser.previous);
        let point = schedule_point(parser, DebugPointKind::Statement, span);
        if_statement(parser, scanner, chunk, vm);
        finish_previous_point(parser, point);
    } else if match_token(parser, scanner, TokenType::Switch) {
        let span = token_span(parser, parser.previous);
        let point = schedule_point(parser, DebugPointKind::Statement, span);
        switch_statement(parser, scanner, chunk, vm);
        finish_previous_point(parser, point);
    } else if match_token(parser, scanner, TokenType::While) {
        let span = token_span(parser, parser.previous);
        let point = schedule_point(parser, DebugPointKind::Statement, span);
        while_statement(parser, scanner, chunk, vm);
        finish_previous_point(parser, point);
    } else if match_token(parser, scanner, TokenType::LeftBrace) {
        begin_scope(parser);
        block(parser, scanner, chunk, vm);
        end_scope(parser, chunk);
    } else {
        let span = token_span(parser, parser.current);
        let point = schedule_point(parser, DebugPointKind::Statement, span);
        expression_statement(parser, scanner, chunk, vm);
        finish_previous_point(parser, point);
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

fn token_span(parser: &Parser, token: Token<'_>) -> SourceSpan {
    token.span(parser.source_id, parser.revision)
}

fn merged_span(parser: &Parser, start: Token<'_>, end: Token<'_>) -> SourceSpan {
    SourceSpan {
        source_id: parser.source_id,
        revision: parser.revision,
        start: start.start_position,
        end: end.end_position,
    }
}

fn emit_opcode(parser: &mut Parser, chunk: &mut Chunk, opcode: OpCode) {
    emit_opcode_at(parser, chunk, opcode, token_span(parser, parser.previous));
}

fn emit_opcode_at(parser: &mut Parser, chunk: &mut Chunk, opcode: OpCode, span: SourceSpan) {
    let offset = chunk.code.len();
    let pending = std::mem::take(&mut parser.compiler.pending_points);
    unsafe {
        (*parser.compiler.function)
            .debug_info
            .points
            .extend(pending.into_iter().map(|point| DebugPoint {
                id: point.id,
                offset,
                kind: point.kind,
                span: point.span,
            }));
    }
    chunk.write(opcode, span);
}

fn emit_byte_operand(parser: &Parser, chunk: &mut Chunk, byte: u8) {
    emit_byte_operand_at(chunk, byte, token_span(parser, parser.previous));
}

fn emit_byte_operand_at(chunk: &mut Chunk, byte: u8, span: SourceSpan) {
    chunk.write(byte, span);
}

fn emit_u16_operand(parser: &Parser, chunk: &mut Chunk, value: u16) {
    let span = token_span(parser, parser.previous);
    emit_u16_operand_at(chunk, value, span);
}

fn emit_u16_operand_at(chunk: &mut Chunk, value: u16, span: SourceSpan) {
    emit_byte_operand_at(chunk, (value & 0xff) as u8, span);
    emit_byte_operand_at(chunk, (value >> 8) as u8, span);
}

fn emit_opcode_with_byte(parser: &mut Parser, chunk: &mut Chunk, opcode: OpCode, operand: u8) {
    emit_opcode(parser, chunk, opcode);
    emit_byte_operand(parser, chunk, operand);
}

fn emit_opcode_with_byte_at(
    parser: &mut Parser,
    chunk: &mut Chunk,
    opcode: OpCode,
    operand: u8,
    span: SourceSpan,
) {
    emit_opcode_at(parser, chunk, opcode, span);
    emit_byte_operand_at(chunk, operand, span);
}

fn emit_loop(parser: &mut Parser, chunk: &mut Chunk, loop_start: usize) {
    emit_opcode(parser, chunk, OpCode::Loop);

    let offset = chunk.code.len() - loop_start + 2;
    if offset > u16::MAX as usize {
        compiler_error(parser, "Loop body too large");
    }

    emit_u16_operand(parser, chunk, offset as u16);
}

fn emit_jump(parser: &mut Parser, chunk: &mut Chunk, instruction: OpCode) -> usize {
    emit_opcode(parser, chunk, instruction);
    // Placeholder offset
    emit_u16_operand(parser, chunk, u16::MAX);

    chunk.code.len() - 2
}

fn emit_return(parser: &mut Parser, chunk: &mut Chunk) {
    emit_opcode(parser, chunk, OpCode::Nil);
    emit_opcode(parser, chunk, OpCode::Return);
}

fn make_constant(parser: &mut Parser, chunk: &mut Chunk, value: Value) -> usize {
    chunk.add_constant(value)
}

fn emit_constant(parser: &mut Parser, chunk: &mut Chunk, value: Value) {
    let constant = make_constant(parser, chunk, value);

    if constant <= 255 {
        emit_opcode_with_byte(parser, chunk, OpCode::Constant, constant as u8);
    } else {
        emit_opcode(parser, chunk, OpCode::ConstantLong);
        emit_byte_operand(parser, chunk, (constant & 0xFF) as u8);
        emit_byte_operand(parser, chunk, ((constant >> 8) & 0xFF) as u8);
        emit_byte_operand(parser, chunk, ((constant >> 16) & 0xFF) as u8);
    }
}

fn patch_jump(parser: &mut Parser, chunk: &mut Chunk, offset: usize) {
    // -2 to adjust for the bytecode for the jump offset itself
    let jump = chunk.code.len() - offset - 2;

    if jump > u16::MAX as usize {
        compiler_error(parser, "Too much code to jump over.");
    }

    chunk.code[offset] = (jump & 0xff) as u8;
    chunk.code[offset + 1] = ((jump >> 8) & 0xff) as u8;
}

fn end_compiler(
    parser: &mut Parser,
    chunk: &mut Chunk,
    vm: &mut VM,
) -> (*mut ObjFunction, Vec<Upvalue>) {
    // Function-entry debug points share offset zero with an empty body's
    // synthetic epilogue. Keep that entry inside the otherwise half-open
    // lifetime without extending ordinary bindings through the epilogue.
    let live_end = chunk.code.len().max(1);
    for local in parser.compiler.locals[1..parser.compiler.local_count].iter() {
        if let Some(metadata_index) = local.metadata_index {
            unsafe {
                let binding =
                    &mut (&mut (*parser.compiler.function).debug_info.bindings)[metadata_index];
                if binding.live_start != usize::MAX {
                    binding.live_end = live_end;
                }
            }
        }
    }
    parser.compiler.pending_points.clear();
    emit_return(parser, chunk);

    let function = parser.compiler.function;
    let upvalues = parser.compiler.upvalues.clone();

    #[cfg(feature = "debug_print_code")]
    {
        if !parser.had_error {
            let name = if unsafe { (*function).name.is_null() } {
                "<script>"
            } else {
                unsafe { ObjString::as_str((*function).name) }
            };

            unsafe {
                (*function).upvalue_count = 0;
            }
            disassemble_chunk(chunk, name);
            unsafe {
                (*function).upvalue_count = upvalues.len();
            }
        }
    }

    if let Some(enclosing) = parser.compiler.enclosing.take() {
        parser.compiler = *enclosing;
    }

    vm.compiler_roots.pop();

    (function, upvalues)
}

fn binary<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    _: bool,
) {
    let operator = parser.previous;
    let operator_type = operator.token_type;
    let span = token_span(parser, operator);

    let rule = get_rule(operator_type);
    let next_precedence =
        unsafe { std::mem::transmute::<u8, Precedence>(rule.precedence as u8 + 1) };
    parse_precedence(parser, scanner, chunk, next_precedence, vm);

    match operator_type {
        TokenType::BangEqual => {
            emit_opcode_at(parser, chunk, OpCode::Equal, span);
            emit_opcode_at(parser, chunk, OpCode::Not, span);
        }
        TokenType::EqualEqual => emit_opcode_at(parser, chunk, OpCode::Equal, span),
        TokenType::Greater => emit_opcode_at(parser, chunk, OpCode::Greater, span),
        TokenType::GreaterEqual => {
            emit_opcode_at(parser, chunk, OpCode::Less, span);
            emit_opcode_at(parser, chunk, OpCode::Not, span);
        }
        TokenType::Less => emit_opcode_at(parser, chunk, OpCode::Less, span),
        TokenType::LessEqual => {
            emit_opcode_at(parser, chunk, OpCode::Greater, span);
            emit_opcode_at(parser, chunk, OpCode::Not, span);
        }
        TokenType::Plus => emit_opcode_at(parser, chunk, OpCode::Add, span),
        TokenType::Minus => emit_opcode_at(parser, chunk, OpCode::Subtract, span),
        TokenType::Star => emit_opcode_at(parser, chunk, OpCode::Multiply, span),
        TokenType::Slash => emit_opcode_at(parser, chunk, OpCode::Divide, span),
        TokenType::Backslash => emit_opcode_at(parser, chunk, OpCode::IntDivide, span),
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
    let opening = parser.previous;
    let arg_count = argument_list(parser, scanner, chunk, vm);
    let span = merged_span(parser, opening, parser.previous);
    emit_opcode_with_byte_at(parser, chunk, OpCode::Call, arg_count, span);
}

fn literal<'a>(
    parser: &mut Parser<'a>,
    _: &mut Scanner<'a>,
    chunk: &mut Chunk,
    _: &mut VM,
    _: bool,
) {
    match parser.previous.token_type {
        TokenType::False => emit_opcode(parser, chunk, OpCode::False),
        TokenType::Nil => emit_opcode(parser, chunk, OpCode::Nil),
        TokenType::True => emit_opcode(parser, chunk, OpCode::True),
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
    let else_jump = emit_jump(parser, chunk, OpCode::JumpIfFalse);
    let end_jump = emit_jump(parser, chunk, OpCode::Jump);

    patch_jump(parser, chunk, else_jump);
    emit_opcode(parser, chunk, OpCode::Pop);

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
                compiler_error(parser, "Error processing list literal.");
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
        emit_opcode_with_byte(parser, chunk, OpCode::BuildList, item_count as u8);
    } else {
        emit_opcode(parser, chunk, OpCode::BuildListLong);
        emit_u16_operand(parser, chunk, item_count as u16);
    }
}

fn index<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    can_assign: bool,
) {
    let opening = parser.previous;
    expression(parser, scanner, chunk, vm);
    consume(
        parser,
        scanner,
        TokenType::RightBracket,
        "Expect ']' after index.",
    );

    let closing = parser.previous;
    let span = merged_span(parser, opening, closing);

    if can_assign && match_token(parser, scanner, TokenType::Equal) {
        expression(parser, scanner, chunk, vm);
        emit_opcode_at(parser, chunk, OpCode::SetIndex, span);
    } else {
        emit_opcode_at(parser, chunk, OpCode::GetIndex, span);
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
    let span = token_span(parser, name);
    let arg;
    let get_op;
    let set_op;

    if let Some(local_arg) = resolve_local(parser, &name) {
        arg = local_arg;
        if arg <= 255 {
            get_op = OpCode::GetLocal;
            set_op = OpCode::SetLocal;
        } else {
            get_op = OpCode::GetLocalLong;
            set_op = OpCode::SetLocalLong
        }
    } else if let Some(upvalue_arg) = resolve_upvalue(parser, &name) {
        arg = upvalue_arg;
        get_op = OpCode::GetUpvalue;
        set_op = OpCode::SetUpvalue;
    } else {
        arg = identifier_constant(parser, chunk, vm, name) as usize;
        get_op = OpCode::GetGlobal;
        set_op = OpCode::SetGlobal;
    }

    let is_assignment = can_assign && match_token(parser, scanner, TokenType::Equal);
    if is_assignment {
        expression(parser, scanner, chunk, vm);
    }

    let op = if is_assignment { set_op } else { get_op };

    if op == OpCode::GetLocalLong || op == OpCode::SetLocalLong {
        emit_opcode_at(parser, chunk, op, span);
        emit_u16_operand_at(chunk, arg as u16, span);
    } else {
        emit_opcode_with_byte_at(parser, chunk, op, arg as u8, span);
    }
}

fn unary<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    _: bool,
) {
    let operator = parser.previous;
    let operator_type = operator.token_type;
    let span = token_span(parser, operator);

    parse_precedence(parser, scanner, chunk, Precedence::Unary, vm);

    match operator_type {
        TokenType::Bang => emit_opcode_at(parser, chunk, OpCode::Not, span),
        TokenType::Minus => emit_opcode_at(parser, chunk, OpCode::Negate, span),
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
        compiler_error(parser, "Too many globals in one chunk.");
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
            let local = parser.compiler.locals[index].clone();
            if local.depth == -1 {
                compiler_error(parser, "Can't read local variable in its own initializer.");
            }
            return Some(index);
        }
    }

    None
}

fn resolve_enclosing_local<'a>(parser: &mut Parser<'a>, name: &Token<'a>) -> Option<usize> {
    let name_str = &name.start[..name.length];

    let (found_index, is_uninitialized) = {
        let enclosing = parser.compiler.enclosing.as_ref().unwrap();
        if let Some(stack) = enclosing.locals_map.get(name_str) {
            if let Some(&index) = stack.last() {
                (Some(index), enclosing.locals[index].depth == -1)
            } else {
                (None, false)
            }
        } else {
            (None, false)
        }
    };

    if is_uninitialized {
        compiler_error(parser, "Cannot read local variable in its own initializer.");
    }

    found_index
}

fn add_upvalue(
    compiler: &mut Compiler,
    index: usize,
    is_local: bool,
    binding_id: BindingId,
    name: String,
    declaration: SourceSpan,
) -> Result<usize, ()> {
    let index = u8::try_from(index).map_err(|_| ())?;
    for (i, upvalue) in compiler.upvalues.iter().enumerate() {
        if upvalue.index == index && upvalue.is_local == is_local {
            return Ok(i);
        }
    }

    let table_index = u8::try_from(compiler.upvalues.len()).map_err(|_| ())?;
    let metadata_index = unsafe { (*compiler.function).debug_info.upvalues.len() };

    unsafe {
        (*compiler.function)
            .debug_info
            .upvalues
            .push(UpvalueDebugInfo {
                binding_id,
                name: name.clone(),
                index: table_index,
                declaration,
            });
    }
    compiler.upvalues.push(Upvalue {
        index,
        is_local,
        binding_id,
        name,
        declaration,
        metadata_index,
    });

    unsafe {
        (*compiler.function).upvalue_count = compiler.upvalues.len();
    }

    Ok(compiler.upvalues.len() - 1)
}

fn resolve_upvalue_in_compiler<'a>(
    compiler: &mut Compiler<'a>,
    name: &Token<'a>,
) -> Result<Option<usize>, ()> {
    let Some(enclosing) = compiler.enclosing.as_mut() else {
        return Ok(None);
    };

    let name_str = &name.start[..name.length];
    let local = if let Some(stack) = enclosing.locals_map.get(name_str) {
        stack
            .last()
            .copied()
            .filter(|&i| enclosing.locals[i].depth != -1)
    } else {
        None
    };

    if let Some(local_idx) = local {
        let local = enclosing.locals[local_idx].clone();
        enclosing.locals[local_idx].is_captured = true;
        return add_upvalue(
            compiler,
            local_idx,
            true,
            local.binding_id.expect("captured locals have binding IDs"),
            local.owned_name,
            local.declaration,
        )
        .map(Some);
    }

    let Some(upvalue) = resolve_upvalue_in_compiler(enclosing, name)? else {
        return Ok(None);
    };
    let provenance = enclosing.upvalues[upvalue].clone();
    add_upvalue(
        compiler,
        upvalue,
        false,
        provenance.binding_id,
        provenance.name,
        provenance.declaration,
    )
    .map(Some)
}

fn resolve_upvalue<'a>(parser: &mut Parser<'a>, name: &Token<'a>) -> Option<usize> {
    match resolve_upvalue_in_compiler(&mut parser.compiler, name) {
        Ok(upvalue) => upvalue,
        Err(()) => {
            compiler_error(parser, "Too many closure variables in function.");
            None
        }
    }
}

fn add_local<'a>(parser: &mut Parser<'a>, name: Token<'a>) {
    if parser.compiler.local_count == LOCALS_MAX {
        compiler_error(parser, "Too many local variables in function.");
        return;
    }

    let index = parser.compiler.local_count;
    let binding_id = BindingId(parser.next_binding_id);
    parser.next_binding_id += 1;
    let owned_name = name.start[..name.length].to_string();
    let declaration = token_span(parser, name);
    let metadata_index = unsafe { (*parser.compiler.function).debug_info.bindings.len() };
    unsafe {
        (*parser.compiler.function)
            .debug_info
            .bindings
            .push(BindingDebugInfo {
                id: binding_id,
                name: owned_name.clone(),
                kind: BindingKind::Local,
                slot: index as u16,
                scope_depth: parser.compiler.scope_depth,
                declaration,
                live_start: usize::MAX,
                live_end: usize::MAX,
            });
    }
    let local = &mut parser.compiler.locals[parser.compiler.local_count];
    local.name = name;
    local.owned_name = owned_name;
    local.depth = -1;
    local.is_captured = false;
    local.binding_id = Some(binding_id);
    local.declaration = declaration;
    local.metadata_index = Some(metadata_index);

    let name_str = &name.start[..name.length];
    parser
        .compiler
        .locals_map
        .entry(name_str)
        .or_default()
        .push(index);

    parser.compiler.local_count += 1;
}

fn declare_variable<'a>(parser: &mut Parser<'a>, binding_kind: BindingKind) {
    if parser.compiler.scope_depth == 0 {
        return;
    }

    let name = parser.previous;
    let name_str = &name.start[..name.length];

    if let Some(stack) = parser.compiler.locals_map.get(name_str) {
        if let Some(&index) = stack.last() {
            let local = parser.compiler.locals[index].clone();
            if local.depth == -1 || local.depth == parser.compiler.scope_depth {
                compiler_error(parser, "Already a variable with this name in this scope.");
            }
        }
    }

    add_local(parser, name);
    if let Some(metadata_index) =
        parser.compiler.locals[parser.compiler.local_count - 1].metadata_index
    {
        unsafe {
            (&mut (*parser.compiler.function).debug_info.bindings)[metadata_index].kind =
                binding_kind;
        }
    }
}

fn parse_variable<'a>(
    parser: &mut Parser<'a>,
    scanner: &mut Scanner<'a>,
    chunk: &mut Chunk,
    vm: &mut VM,
    error_message: &str,
    binding_kind: BindingKind,
) -> u8 {
    consume(parser, scanner, TokenType::Identifier, error_message);

    declare_variable(parser, binding_kind);

    if parser.compiler.scope_depth > 0 {
        return 0;
    }

    let name = parser.previous;
    identifier_constant(parser, chunk, vm, name)
}

fn mark_resolver_initialized(parser: &mut Parser) {
    if parser.compiler.scope_depth == 0 {
        return;
    }

    let local_count = parser.compiler.local_count;
    parser.compiler.locals[local_count - 1].depth = parser.compiler.scope_depth;
}

fn initialize_local_lifetime(parser: &mut Parser, live_start: usize) {
    mark_resolver_initialized(parser);
    let local_count = parser.compiler.local_count;
    if let Some(metadata_index) = parser.compiler.locals[local_count - 1].metadata_index {
        unsafe {
            (&mut (*parser.compiler.function).debug_info.bindings)[metadata_index].live_start =
                live_start;
        }
    }
}

fn define_variable(parser: &mut Parser, chunk: &mut Chunk, global: u8) {
    if parser.compiler.scope_depth > 0 {
        initialize_local_lifetime(parser, chunk.code.len());
        return;
    }

    emit_opcode_with_byte(parser, chunk, OpCode::DefineGlobal, global);
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
                compiler_error(parser, "Cannot have more than 255 arguments.");
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
    let end_jump = emit_jump(parser, chunk, OpCode::JumpIfFalse);

    emit_opcode(parser, chunk, OpCode::Pop);
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::compile;
    use crate::chunk::{Chunk, OpCode};
    use crate::debug_info::{BindingKind, DebugPointKind};
    use crate::object::{ObjFunction, ObjString};
    use crate::value::Value;
    use crate::vm::VM;
    use crate::{RecordingHost, RevisionId, SourceDocument, SourceId};

    fn compile_result(source: &str) -> (VM, Option<*mut ObjFunction>, RecordingHost) {
        let document = SourceDocument::new(SourceId(41), RevisionId(7), "test.lox", source);
        let mut vm = VM::new();
        let mut host = RecordingHost::default();
        let function = compile(&document, &mut vm, &mut host);
        (vm, function, host)
    }

    fn compile_success(source: &str) -> (VM, *mut ObjFunction) {
        let (vm, function, host) = compile_result(source);
        assert!(
            host.diagnostics().is_empty(),
            "unexpected diagnostics: {:?}",
            host.diagnostics()
        );
        (vm, function.expect("source should compile"))
    }

    fn function_name(function: *mut ObjFunction) -> &'static str {
        unsafe {
            if (*function).name.is_null() {
                "<script>"
            } else {
                ObjString::as_str((*function).name)
            }
        }
    }

    fn function_ref(function: *mut ObjFunction) -> &'static ObjFunction {
        unsafe { &*function }
    }

    fn direct_function(function: *mut ObjFunction, name: &str) -> *mut ObjFunction {
        unsafe {
            (*function)
                .chunk
                .constants
                .iter()
                .filter_map(|value| match value {
                    Value::Obj(object)
                        if (**object).obj_type == crate::object::ObjType::Function =>
                    {
                        Some(*object as *mut ObjFunction)
                    }
                    _ => None,
                })
                .find(|candidate| function_name(*candidate) == name)
                .unwrap_or_else(|| panic!("missing function {name}"))
        }
    }

    fn opcode_starts(function: *mut ObjFunction) -> HashSet<usize> {
        let chunk = unsafe { &(*function).chunk };
        chunk
            .opcode_starts()
            .unwrap_or_else(|error| panic!("invalid compiler bytecode: {error}"))
    }

    fn assert_points_are_opcode_starts(function: *mut ObjFunction) {
        let starts = opcode_starts(function);
        unsafe {
            for point in &(*function).debug_info.points {
                assert!(
                    starts.contains(&point.offset),
                    "point {:?} is not at an opcode start",
                    point
                );
            }
        }
    }

    fn assert_golden_code(function: *mut ObjFunction, expected: &[u8]) {
        let chunk = &function_ref(function).chunk;
        assert_eq!(chunk.code, expected);
        assert_eq!(chunk.spans.len(), chunk.code.len());
        chunk
            .opcode_starts()
            .unwrap_or_else(|error| panic!("invalid golden bytecode: {error}"));
    }

    fn position_at(source: &str, byte_offset: usize) -> crate::TextPosition {
        let mut line = 1;
        let mut column = 1;
        for scalar in source[..byte_offset].chars() {
            if scalar == '\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
        }
        crate::TextPosition {
            byte_offset,
            line,
            column,
        }
    }

    fn assert_point_span(
        function: *mut ObjFunction,
        source: &str,
        kind: DebugPointKind,
        expected: &str,
    ) {
        let start = source
            .find(expected)
            .unwrap_or_else(|| panic!("missing expected construct {expected:?}"));
        let end = start + expected.len();
        let point = &function_ref(function)
            .debug_info
            .points
            .iter()
            .find(|point| point.kind == kind && point.span.start.byte_offset == start)
            .unwrap_or_else(|| panic!("missing {kind:?} point at byte {start}"));
        assert_eq!(point.span.start, position_at(source, start));
        assert_eq!(point.span.end, position_at(source, end));
        assert_eq!(&source[start..end], expected);
    }

    #[test]
    fn debug_points_preserve_distinct_same_line_statement_spans() {
        let source = "var a=1; a=a+1; print a;";
        let (_vm, function) = compile_success(source);
        let points = unsafe { &(*function).debug_info.points };
        let statements: Vec<_> = points
            .iter()
            .filter(|point| point.kind == DebugPointKind::Statement)
            .collect();

        assert_eq!(statements.len(), 3);
        assert_eq!(
            statements
                .iter()
                .map(|point| &source[point.span.start.byte_offset..point.span.end.byte_offset])
                .collect::<Vec<_>>(),
            ["var a=1;", "a=a+1;", "print a;"]
        );
        assert!(statements.iter().all(|point| point.span.start.line == 1));
        assert_eq!(
            points
                .iter()
                .filter(|point| point.kind == DebugPointKind::FunctionEntry)
                .count(),
            1
        );
        assert_eq!(points[0].offset, 0);
        assert_eq!(points[1].offset, 0);
        assert_ne!(points[0].id, points[1].id);
        assert!(points.iter().all(|point| {
            point.span.source_id == SourceId(41) && point.span.revision == RevisionId(7)
        }));
        assert_points_are_opcode_starts(function);
    }

    #[test]
    fn empty_function_has_only_an_entry_point() {
        let (_vm, script) = compile_success("fun empty() {}\n");
        let empty = direct_function(script, "empty");

        assert_eq!(function_ref(empty).debug_info.points.len(), 1);
        assert_eq!(
            function_ref(empty).debug_info.points[0].kind,
            DebugPointKind::FunctionEntry
        );
        assert_eq!(function_ref(empty).debug_info.points[0].offset, 0);
    }

    #[test]
    fn multiline_expression_has_one_statement_point() {
        let source = "\n// lead\nprint (1 +\n  2);\n";
        let (_vm, function) = compile_success(source);
        let points = unsafe { &(*function).debug_info.points };
        let statements: Vec<_> = points
            .iter()
            .filter(|point| point.kind == DebugPointKind::Statement)
            .collect();

        assert_eq!(statements.len(), 1);
        assert_point_span(
            function,
            source,
            DebugPointKind::Statement,
            "print (1 +\n  2);",
        );
        assert_points_are_opcode_starts(function);
    }

    #[test]
    fn declarations_functions_and_returns_use_full_construct_spans() {
        let source = "var global =\n  1;\nfun f(p) {\n  var local;\n  return\n    p;\n}\n";
        let (_vm, script) = compile_success(source);
        let function = direct_function(script, "f");

        assert_point_span(
            script,
            source,
            DebugPointKind::Statement,
            "var global =\n  1;",
        );
        let function_construct = "fun f(p) {\n  var local;\n  return\n    p;\n}";
        assert_point_span(
            script,
            source,
            DebugPointKind::Statement,
            function_construct,
        );
        assert_point_span(
            function,
            source,
            DebugPointKind::FunctionEntry,
            function_construct,
        );
        assert_point_span(function, source, DebugPointKind::Statement, "var local;");
        assert_point_span(
            function,
            source,
            DebugPointKind::Statement,
            "return\n    p;",
        );
    }

    #[test]
    fn assignment_if_else_and_nested_statement_spans_are_complete() {
        let source = "var a=0;\na = a +\n  1;\nif (a)\n  print 1;\nelse\n  print 2;\n";
        let (_vm, function) = compile_success(source);

        assert_point_span(function, source, DebugPointKind::Statement, "a = a +\n  1;");
        assert_point_span(
            function,
            source,
            DebugPointKind::Statement,
            "if (a)\n  print 1;\nelse\n  print 2;",
        );
        assert_point_span(function, source, DebugPointKind::Statement, "print 1;");
        assert_point_span(function, source, DebugPointKind::Statement, "print 2;");
    }

    #[test]
    fn while_switch_and_nested_block_spans_exclude_comments_and_braces() {
        let source = "while (false) {\n  // loop comment\n  print 1;\n}\nswitch (1) {\n  case 1: print 2;\n  default: {\n    // empty\n  }\n}\n{\n  // blank block\n}\n";
        let (_vm, function) = compile_success(source);

        assert_point_span(
            function,
            source,
            DebugPointKind::Statement,
            "while (false) {\n  // loop comment\n  print 1;\n}",
        );
        assert_point_span(function, source, DebugPointKind::Statement, "print 1;");
        assert_point_span(
            function,
            source,
            DebugPointKind::Statement,
            "switch (1) {\n  case 1: print 2;\n  default: {\n    // empty\n  }\n}",
        );
        assert_point_span(function, source, DebugPointKind::Statement, "print 2;");
        assert_eq!(
            function_ref(function)
                .debug_info
                .points
                .iter()
                .filter(|point| point.kind == DebugPointKind::Statement)
                .count(),
            4
        );
    }

    #[test]
    fn for_clause_and_body_spans_cover_only_present_executable_constructs() {
        let source = "for (var i = 0;\n     i < 2;\n     i = i + 1)\n  print i;\nfor (;;) {\n  // omitted\n}\n";
        let (_vm, function) = compile_success(source);

        assert_point_span(
            function,
            source,
            DebugPointKind::LoopInitializer,
            "var i = 0;",
        );
        assert_point_span(function, source, DebugPointKind::LoopCondition, "i < 2;");
        assert_point_span(function, source, DebugPointKind::LoopIncrement, "i = i + 1");
        assert_point_span(function, source, DebugPointKind::Statement, "print i;");
        assert_eq!(
            function_ref(function)
                .debug_info
                .points
                .iter()
                .filter(|point| matches!(
                    point.kind,
                    DebugPointKind::LoopInitializer
                        | DebugPointKind::LoopCondition
                        | DebugPointKind::LoopIncrement
                ))
                .count(),
            3
        );
    }

    #[test]
    fn for_clauses_and_body_have_distinct_semantic_points() {
        let (_vm, function) = compile_success("for (var i=0; i<2; i=i+1) print i;\n");
        let points = unsafe { &(*function).debug_info.points };
        let kinds: Vec<_> = points.iter().map(|point| point.kind).collect();

        assert_eq!(
            kinds,
            [
                DebugPointKind::FunctionEntry,
                DebugPointKind::LoopInitializer,
                DebugPointKind::LoopCondition,
                DebugPointKind::LoopIncrement,
                DebugPointKind::Statement,
            ]
        );
        for point in points.iter().skip(1) {
            let opcode = function_ref(function).chunk.code[point.offset];
            assert!(!matches!(
                opcode,
                x if x == OpCode::Jump as u8
                    || x == OpCode::JumpIfFalse as u8
                    || x == OpCode::Loop as u8
                    || x == OpCode::Pop as u8
                    || x == OpCode::CloseUpvalue as u8
            ));
        }
        assert_points_are_opcode_starts(function);
    }

    #[test]
    fn omitted_for_clauses_do_not_create_points() {
        let (_vm, function) = compile_success("for (;;) print 1;\n");
        let kinds: Vec<_> = unsafe { &(*function).debug_info.points }
            .iter()
            .map(|point| point.kind)
            .collect();

        assert_eq!(
            kinds,
            [DebugPointKind::FunctionEntry, DebugPointKind::Statement]
        );
    }

    #[test]
    fn debug_points_decode_across_long_and_variable_width_instructions() {
        let mut source = String::from("fun outer(p) { fun inner() { print p; } return inner; }\n");
        for value in 0..260 {
            source.push_str(&format!("print {};\n", value + 1000));
        }
        let (_vm, script) = compile_success(&source);
        let outer = direct_function(script, "outer");
        let inner = direct_function(outer, "inner");

        assert!(unsafe { (*script).chunk.code.contains(&(OpCode::ConstantLong as u8)) });
        assert_points_are_opcode_starts(script);
        assert_points_are_opcode_starts(outer);
        assert_points_are_opcode_starts(inner);
    }

    #[test]
    fn shadowed_and_reused_slots_have_distinct_binding_ids_and_exact_lifetimes() {
        let (_vm, script) =
            compile_success("fun f(p) { { var a=1; print a; } { var a=2; print a; } }\n");
        let function = direct_function(script, "f");
        let info = unsafe { &(*function).debug_info };
        let parameter = info
            .bindings
            .iter()
            .find(|binding| binding.name == "p")
            .unwrap();
        let locals: Vec<_> = info
            .bindings
            .iter()
            .filter(|binding| binding.name == "a")
            .collect();

        assert_eq!(parameter.kind, BindingKind::Parameter);
        assert_eq!(parameter.live_start, 0);
        assert_eq!(locals.len(), 2);
        assert_eq!(locals[0].slot, locals[1].slot);
        assert_ne!(locals[0].id, locals[1].id);
        assert!(
            locals
                .iter()
                .all(|binding| binding.kind == BindingKind::Local)
        );
        assert!(locals.iter().all(|binding| {
            function_ref(function).chunk.code[binding.live_end] == OpCode::Pop as u8
        }));
        assert_eq!(
            function_ref(function).chunk.code[parameter.live_end],
            OpCode::Nil as u8
        );
    }

    #[test]
    fn empty_function_parameters_include_the_entry_offset_only() {
        let (_vm, script) = compile_success("fun empty(parameter) {}\n");
        let function = direct_function(script, "empty");
        let parameter = function_ref(function)
            .debug_info
            .bindings
            .iter()
            .find(|binding| binding.name == "parameter")
            .unwrap();

        assert_eq!(parameter.live_start, 0);
        assert_eq!(parameter.live_end, 1);
        assert_eq!(function_ref(function).chunk.code[0], OpCode::Nil as u8);
        assert_eq!(function_ref(function).chunk.code[1], OpCode::Return as u8);
    }

    #[test]
    fn recursive_local_starts_after_complete_closure_and_keeps_capture_provenance() {
        let source = "fun outer() { var seed=1; fun recurse(n) { print seed; if (n>0) recurse(n-1); } recurse(1); }\n";
        let (_vm, script) = compile_success(source);
        let outer = direct_function(script, "outer");
        let recurse = direct_function(outer, "recurse");
        let binding = unsafe { &(*outer).debug_info.bindings }
            .iter()
            .find(|binding| binding.name == "recurse")
            .unwrap();
        let closure_offset = opcode_starts(outer)
            .into_iter()
            .find(|offset| function_ref(outer).chunk.code[*offset] == OpCode::Closure as u8)
            .unwrap();

        assert_eq!(unsafe { (*recurse).upvalue_count }, 2);
        assert_eq!(binding.live_start, closure_offset + 6);
        let local_function_point = unsafe { &(*outer).debug_info.points }
            .iter()
            .find(|point| point.kind == DebugPointKind::Statement && point.offset == closure_offset)
            .expect("local function point should survive nested compilation");
        assert_eq!(
            &source[local_function_point.span.start.byte_offset
                ..local_function_point.span.end.byte_offset],
            "fun recurse(n) { print seed; if (n>0) recurse(n-1); }"
        );
        let upvalue = unsafe { &(*recurse).debug_info.upvalues }
            .iter()
            .find(|upvalue| upvalue.name == "recurse")
            .unwrap();
        assert_eq!(upvalue.binding_id, binding.id);
        assert_eq!(upvalue.name, "recurse");
        assert_eq!(upvalue.declaration, binding.declaration);
    }

    #[test]
    fn transitive_open_and_closed_captures_keep_original_binding_metadata() {
        let (_vm, script) = compile_success(
            "fun outer(p) { var a=1; fun middle() { fun inner() { print p; print a; } return inner; } return middle; }\n",
        );
        let outer = direct_function(script, "outer");
        let middle = direct_function(outer, "middle");
        let inner = direct_function(middle, "inner");
        let outer_bindings = unsafe { &(*outer).debug_info.bindings };
        let inner_upvalues = unsafe { &(*inner).debug_info.upvalues };

        for name in ["p", "a"] {
            let binding = outer_bindings
                .iter()
                .find(|binding| binding.name == name)
                .unwrap();
            let middle_capture = unsafe { &(*middle).debug_info.upvalues }
                .iter()
                .find(|upvalue| upvalue.name == name)
                .unwrap();
            let inner_capture = inner_upvalues
                .iter()
                .find(|upvalue| upvalue.name == name)
                .unwrap();
            assert_eq!(middle_capture.binding_id, binding.id);
            assert_eq!(inner_capture.binding_id, binding.id);
            assert_eq!(inner_capture.declaration, binding.declaration);
        }
    }

    fn capture_boundary_source(grandparent_bindings: usize) -> String {
        let mut source = String::from("fun grand() {");
        for index in 0..grandparent_bindings {
            source.push_str(&format!("var g{index}={index};"));
        }
        source.push_str("fun parent() {");
        for index in 0..254 {
            source.push_str(&format!("var p{index}={index};"));
        }
        source.push_str("fun child() {");
        for index in 0..grandparent_bindings {
            source.push_str(&format!("print g{index};"));
        }
        for index in 0..254 {
            source.push_str(&format!("print p{index};"));
        }
        source.push_str("print child;");
        source.push_str("} return child; } return parent; }");
        source
    }

    #[test]
    fn capture_table_accepts_256_entries_and_slot_255_without_wrapping() {
        let source = capture_boundary_source(1);
        let (_vm, script) = compile_success(&source);
        let grand = direct_function(script, "grand");
        let parent = direct_function(grand, "parent");
        let child = direct_function(parent, "child");

        assert_eq!(unsafe { (*child).upvalue_count }, 256);
        assert_eq!(unsafe { (*child).debug_info.upvalues.len() }, 256);
        assert_eq!(function_ref(child).debug_info.upvalues[255].index, 255);
        let closure_offset = opcode_starts(parent)
            .into_iter()
            .find(|offset| function_ref(parent).chunk.code[*offset] == OpCode::Closure as u8)
            .unwrap();
        assert_eq!(
            function_ref(parent).chunk.code[closure_offset + 2 + 255 * 2],
            1
        );
        assert_eq!(
            function_ref(parent).chunk.code[closure_offset + 3 + 255 * 2],
            255
        );
    }

    #[test]
    fn duplicate_capture_at_the_limit_reuses_the_existing_entry() {
        let mut source = capture_boundary_source(1);
        source = source.replacen("} return child;", "print p253; } return child;", 1);
        let (_vm, script) = compile_success(&source);
        let grand = direct_function(script, "grand");
        let parent = direct_function(grand, "parent");
        let child = direct_function(parent, "child");

        assert_eq!(unsafe { (*child).upvalue_count }, 256);
        assert_eq!(unsafe { (*child).debug_info.upvalues.len() }, 256);
    }

    #[test]
    fn captured_local_ends_immediately_before_close_upvalue() {
        let (_vm, script) =
            compile_success("fun outer() { { var captured=1; fun inner(){ print captured; } } }\n");
        let outer = direct_function(script, "outer");
        let binding = unsafe { &(*outer).debug_info.bindings }
            .iter()
            .find(|binding| binding.name == "captured")
            .unwrap();

        assert_eq!(
            function_ref(outer).chunk.code[binding.live_end],
            OpCode::CloseUpvalue as u8
        );
    }

    #[test]
    fn capture_table_rejects_the_257th_entry_before_u8_conversion() {
        let source = capture_boundary_source(2);
        let (_vm, function, host) = compile_result(&source);

        assert!(function.is_none());
        assert!(
            host.diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message == "Too many closure variables in function.")
        );
    }

    #[test]
    fn captured_slot_rejects_first_index_beyond_u8() {
        let mut source = String::from("fun outer() {");
        for index in 0..256 {
            source.push_str(&format!("var v{index}={index};"));
        }
        source.push_str("fun inner() { print v255; }}");
        let (_vm, function, host) = compile_result(&source);

        assert!(function.is_none());
        assert!(
            host.diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message == "Too many local variables in function.")
        );
    }

    #[test]
    fn metadata_does_not_change_bytecode_or_runtime_output() {
        let source = "var a=1; a=a+1; print a;";
        let (_vm, function) = compile_success(source);
        assert_eq!(
            unsafe { &(*function).chunk.code },
            &[
                OpCode::Constant as u8,
                1,
                OpCode::DefineGlobal as u8,
                0,
                OpCode::GetGlobal as u8,
                0,
                OpCode::Constant as u8,
                1,
                OpCode::Add as u8,
                OpCode::SetGlobal as u8,
                0,
                OpCode::Pop as u8,
                OpCode::GetGlobal as u8,
                0,
                OpCode::Print as u8,
                OpCode::Nil as u8,
                OpCode::Return as u8,
            ]
        );

        let document = SourceDocument::new(SourceId(41), RevisionId(7), "test.lox", source);
        let mut vm = VM::new();
        let mut host = RecordingHost::default();
        assert_eq!(vm.run(&document, &mut host), crate::InterpretResult::Ok);
        assert_eq!(host.output(), ["2"]);
        assert!(host.diagnostics().is_empty());
    }

    #[test]
    fn golden_jumps_loops_and_logical_operators_preserve_exact_bytes() {
        let (_vm, while_function) = compile_success("while (false) print 1;");
        assert_golden_code(
            while_function,
            &[
                OpCode::False as u8,
                OpCode::JumpIfFalse as u8,
                7,
                0,
                OpCode::Pop as u8,
                OpCode::Constant as u8,
                0,
                OpCode::Print as u8,
                OpCode::Loop as u8,
                11,
                0,
                OpCode::Pop as u8,
                OpCode::Nil as u8,
                OpCode::Return as u8,
            ],
        );

        let (_vm, if_function) = compile_success("if (true) print 1; else print 2;");
        assert_golden_code(
            if_function,
            &[
                OpCode::True as u8,
                OpCode::JumpIfFalse as u8,
                7,
                0,
                OpCode::Pop as u8,
                OpCode::Constant as u8,
                0,
                OpCode::Print as u8,
                OpCode::Jump as u8,
                4,
                0,
                OpCode::Pop as u8,
                OpCode::Constant as u8,
                1,
                OpCode::Print as u8,
                OpCode::Nil as u8,
                OpCode::Return as u8,
            ],
        );

        let (_vm, logical_function) = compile_success("print true and false or true;");
        assert_golden_code(
            logical_function,
            &[
                OpCode::True as u8,
                OpCode::JumpIfFalse as u8,
                2,
                0,
                OpCode::Pop as u8,
                OpCode::False as u8,
                OpCode::JumpIfFalse as u8,
                3,
                0,
                OpCode::Jump as u8,
                2,
                0,
                OpCode::Pop as u8,
                OpCode::True as u8,
                OpCode::Print as u8,
                OpCode::Nil as u8,
                OpCode::Return as u8,
            ],
        );
    }

    #[test]
    fn golden_calls_lists_indexing_and_returns_preserve_exact_bytes() {
        let source = "fun id(x){ return x; } var a=[1,2]; a[0]=id(a[1]); print a[0];";
        let (_vm, script) = compile_success(source);
        let identity = direct_function(script, "id");

        assert_golden_code(
            script,
            &[
                OpCode::Closure as u8,
                1,
                OpCode::DefineGlobal as u8,
                0,
                OpCode::Constant as u8,
                3,
                OpCode::Constant as u8,
                4,
                OpCode::BuildList as u8,
                2,
                OpCode::DefineGlobal as u8,
                2,
                OpCode::GetGlobal as u8,
                2,
                OpCode::Constant as u8,
                5,
                OpCode::GetGlobal as u8,
                0,
                OpCode::GetGlobal as u8,
                2,
                OpCode::Constant as u8,
                3,
                OpCode::GetIndex as u8,
                OpCode::Call as u8,
                1,
                OpCode::SetIndex as u8,
                OpCode::Pop as u8,
                OpCode::GetGlobal as u8,
                2,
                OpCode::Constant as u8,
                5,
                OpCode::GetIndex as u8,
                OpCode::Print as u8,
                OpCode::Nil as u8,
                OpCode::Return as u8,
            ],
        );
        assert_golden_code(
            identity,
            &[
                OpCode::GetLocal as u8,
                1,
                OpCode::Return as u8,
                OpCode::Nil as u8,
                OpCode::Return as u8,
            ],
        );
    }

    #[test]
    fn golden_long_constants_and_lists_preserve_exact_bytes() {
        let mut constants_source = String::new();
        let mut constants_expected = Vec::new();
        for index in 0..=256usize {
            constants_source.push_str(&format!("print {index};"));
            if index <= u8::MAX as usize {
                constants_expected.extend([OpCode::Constant as u8, index as u8]);
            } else {
                constants_expected.extend([OpCode::ConstantLong as u8, 0, 1, 0]);
            }
            constants_expected.push(OpCode::Print as u8);
        }
        constants_expected.extend([OpCode::Nil as u8, OpCode::Return as u8]);
        let (_vm, constants_function) = compile_success(&constants_source);
        assert_golden_code(constants_function, &constants_expected);

        let list_source = format!("print [{}];", vec!["nil"; 255].join(","));
        let mut list_expected = vec![OpCode::Nil as u8; 255];
        list_expected.extend([
            OpCode::BuildList as u8,
            255,
            OpCode::Print as u8,
            OpCode::Nil as u8,
            OpCode::Return as u8,
        ]);
        let (_vm, list_function) = compile_success(&list_source);
        assert_golden_code(list_function, &list_expected);

        let long_list_source = format!("print [{}];", vec!["nil"; 256].join(","));
        let (_vm, function, host) = compile_result(&long_list_source);
        assert!(function.is_none());
        assert!(
            host.diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message == "Error processing list literal.")
        );

        let span = crate::SourceSpan {
            source_id: SourceId(41),
            revision: RevisionId(7),
            start: position_at("", 0),
            end: position_at("", 0),
        };
        let mut encoded_long_list = Chunk::new();
        encoded_long_list.code = vec![
            OpCode::BuildListLong as u8,
            0,
            1,
            OpCode::Nil as u8,
            OpCode::Return as u8,
        ];
        encoded_long_list.spans = vec![span; encoded_long_list.code.len()];
        assert_eq!(
            encoded_long_list.opcode_starts().unwrap(),
            HashSet::from([0, 3, 4])
        );
        assert_eq!(encoded_long_list.spans.len(), encoded_long_list.code.len());
    }

    #[test]
    fn golden_long_closure_uses_a_three_byte_constant_operand() {
        let mut source = String::from("fun outer() { var captured; fun first() {}");
        for value in 0..=254 {
            source.push_str(&format!("{value};"));
        }
        source.push_str("fun later() { print captured; } later(); } outer();");

        let (_vm, script) = compile_success(&source);
        let outer = direct_function(script, "outer");
        let offset = opcode_starts(outer)
            .into_iter()
            .find(|offset| function_ref(outer).chunk.code[*offset] == OpCode::ClosureLong as u8)
            .expect("the second nested function should use a long closure operand");

        assert_eq!(
            &function_ref(outer).chunk.code[offset..offset + 6],
            &[OpCode::ClosureLong as u8, 0, 1, 0, 1, 1]
        );
        assert_points_are_opcode_starts(outer);
        let Value::Obj(later) = function_ref(outer).chunk.constants[256] else {
            panic!("long closure constant should contain a function");
        };
        assert_eq!(
            unsafe { ObjString::as_str((*(later as *mut ObjFunction)).name) },
            "later"
        );
    }

    #[test]
    fn golden_maximum_local_slot_uses_the_exact_byte_operand() {
        let mut source = String::from("fun f(){");
        let mut expected = Vec::new();
        for index in 0..255usize {
            source.push_str(&format!("var v{index}={index};"));
            expected.extend([OpCode::Constant as u8, index as u8]);
        }
        source.push_str("print v254;}");
        expected.extend([
            OpCode::GetLocal as u8,
            255,
            OpCode::Print as u8,
            OpCode::Nil as u8,
            OpCode::Return as u8,
        ]);

        let (_vm, script) = compile_success(&source);
        let function = direct_function(script, "f");
        assert_golden_code(function, &expected);
        let starts = opcode_starts(function);
        assert!(starts.iter().all(|offset| {
            function_ref(function).chunk.code[*offset] != OpCode::GetLocalLong as u8
        }));
    }

    #[test]
    fn golden_closure_descriptors_and_cleanup_preserve_exact_bytes() {
        let source = "fun outer(){ {var x=1; fun inner(){print x;} } }";
        let (_vm, script) = compile_success(source);
        let outer = direct_function(script, "outer");
        let inner = direct_function(outer, "inner");

        assert_golden_code(
            script,
            &[
                OpCode::Closure as u8,
                1,
                OpCode::DefineGlobal as u8,
                0,
                OpCode::Nil as u8,
                OpCode::Return as u8,
            ],
        );
        assert_golden_code(
            outer,
            &[
                OpCode::Constant as u8,
                0,
                OpCode::Closure as u8,
                1,
                1,
                1,
                OpCode::Pop as u8,
                OpCode::CloseUpvalue as u8,
                OpCode::Nil as u8,
                OpCode::Return as u8,
            ],
        );
        assert_golden_code(
            inner,
            &[
                OpCode::GetUpvalue as u8,
                0,
                OpCode::Print as u8,
                OpCode::Nil as u8,
                OpCode::Return as u8,
            ],
        );
    }
}
