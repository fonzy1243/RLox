mod chunk;
mod compiler;
mod debug;
mod scanner;
mod value;
mod vm;

use crate::chunk::{Chunk, OpCode};
use crate::debug::disassemble_chunk;
use crate::vm::{InterpretResult, VM};

use std::env;
use std::fs;
use std::io::{self, Write};

fn repl(vm: &mut VM) {
    loop {
        print!("> ");
        io::stdout().flush().unwrap();

        let mut line = String::new();

        if io::stdin().read_line(&mut line).unwrap() == 0 {
            println!();
            break;
        }

        vm.interpret(&line);
    }
}

fn run_file(vm: &mut VM, path: &str) {
    let source = fs::read_to_string(path).unwrap_or_else(|_| {
        eprintln!("Could not read file \"{}\".", path);
        std::process::exit(74);
    });

    match vm.interpret(&source) {
        InterpretResult::CompileError => std::process::exit(65),
        InterpretResult::RuntimeError => std::process::exit(70),
        InterpretResult::Ok => {}
    };
}

fn main() {
    let mut vm = VM::new();
    let args: Vec<String> = env::args().collect();

    match args.len() {
        1 => repl(&mut vm),
        2 => run_file(&mut vm, &args[1]),
        _ => {
            eprintln!("Usage: lox [path]");
            std::process::exit(64);
        }
    }
}
