use std::env;
use std::fs;
use std::io::{self, Write};

use rlox::{InterpretResult, Interpreter};

fn repl(interpreter: &mut Interpreter) {
    loop {
        print!("> ");
        io::stdout().flush().unwrap();

        let mut line = String::new();

        if io::stdin().read_line(&mut line).unwrap() == 0 {
            println!();
            break;
        }

        interpreter.interpret(&line);
    }
}

fn run_file(interpreter: &mut Interpreter, path: &str) {
    let source = fs::read_to_string(path).unwrap_or_else(|_| {
        eprintln!("Could not read file \"{}\".", path);
        std::process::exit(74);
    });

    match interpreter.interpret(&source) {
        InterpretResult::CompileError => std::process::exit(65),
        InterpretResult::RuntimeError => std::process::exit(70),
        InterpretResult::Ok => {}
    };
}

fn main() {
    let mut interpreter = Interpreter::new();
    let args: Vec<String> = env::args().collect();

    match args.len() {
        1 => repl(&mut interpreter),
        2 => run_file(&mut interpreter, &args[1]),
        _ => {
            eprintln!("Usage: lox [path]");
            std::process::exit(64);
        }
    }
}
