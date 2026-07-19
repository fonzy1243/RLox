use std::env;
use std::fs;
use std::io::{self, Write};
use std::sync::Arc;

use rlox::{
    Diagnostic, DiagnosticPhase, InterpretResult, Interpreter, RevisionId, RuntimeHost,
    SourceDocument, SourceId,
};
use rlox_protocol::WorkerSessionId;

mod debug_worker;

struct ConsoleHost {
    source: Arc<str>,
}

impl ConsoleHost {
    fn new(source: Arc<str>) -> Self {
        Self { source }
    }
}

impl RuntimeHost for ConsoleHost {
    fn output(&mut self, text: String) {
        println!("{text}");
    }

    fn diagnostic(&mut self, value: Diagnostic) {
        if value.phase == DiagnosticPhase::Runtime {
            eprintln!("{}", value.message);
            for frame in value.frames {
                if frame.function == "<script>" {
                    eprintln!("[line {}] in script", frame.span.start.line);
                } else {
                    eprintln!("[line {}] in {}()", frame.span.start.line, frame.function);
                }
            }
            return;
        }

        eprint!("[line {}] Error", value.span.start.line);
        if value.phase != DiagnosticPhase::Scanner {
            if value.span.start == value.span.end
                && value.span.start.byte_offset == self.source.len()
            {
                eprint!(" at end");
            } else if let Some(lexeme) = self
                .source
                .get(value.span.start.byte_offset..value.span.end.byte_offset)
            {
                eprint!(" at '{lexeme}'");
            }
        }
        eprintln!(": {}", value.message);
    }
}

fn repl(interpreter: &mut Interpreter) {
    let mut revision = 1;
    loop {
        print!("> ");
        io::stdout().flush().unwrap();

        let mut line = String::new();

        if io::stdin().read_line(&mut line).unwrap() == 0 {
            println!();
            break;
        }

        let document = SourceDocument::new(SourceId(1), RevisionId(revision), "<repl>", &line);
        revision += 1;
        let mut host = ConsoleHost::new(document.text.clone());
        interpreter.run(document, &mut host);
    }
}

fn run_file(interpreter: &mut Interpreter, path: &str) {
    let source = fs::read_to_string(path).unwrap_or_else(|_| {
        eprintln!("Could not read file \"{}\".", path);
        std::process::exit(74);
    });

    let document = SourceDocument::new(SourceId(1), RevisionId(1), path, source);
    let mut host = ConsoleHost::new(document.text.clone());

    match interpreter.run(document, &mut host) {
        InterpretResult::CompileError => std::process::exit(65),
        InterpretResult::RuntimeError => std::process::exit(70),
        InterpretResult::Ok => {}
    };
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args
        .get(1)
        .is_some_and(|argument| argument == "--debug-worker")
    {
        let session = match args.as_slice() {
            [_, mode, session_flag, session]
                if mode == "--debug-worker" && session_flag == "--worker-session" =>
            {
                parse_worker_session(session)
            }
            _ => None,
        };
        let Some(session) = session else {
            eprintln!("Usage: lox [path]");
            std::process::exit(64);
        };
        if let Err(error) = debug_worker::run_worker(std::io::stdin(), std::io::stdout(), session) {
            eprintln!("Oxide IDE worker failed: {error:?}");
            std::process::exit(70);
        }
        return;
    }

    let mut interpreter = Interpreter::new();

    match args.len() {
        1 => repl(&mut interpreter),
        2 => run_file(&mut interpreter, &args[1]),
        _ => {
            eprintln!("Usage: lox [path]");
            std::process::exit(64);
        }
    }
}

fn parse_worker_session(value: &str) -> Option<WorkerSessionId> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let parsed = value.parse::<u64>().ok()?;
    (parsed.to_string() == value).then_some(WorkerSessionId(parsed))
}
