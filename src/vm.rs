use crate::chunk::{Chunk, OpCode};
use crate::compiler::compile;
use crate::value::Value;

#[cfg(feature = "debug_trace_execution")]
use crate::debug::disassemble_instruction;

macro_rules! read_byte {
    ($vm:expr, $chunk:expr) => {{
        let byte = $chunk.code[$vm.ip];
        $vm.ip += 1;
        byte
    }};
}

macro_rules! read_constant {
    ($vm:expr, $chunk:expr) => {
        $chunk.constants[read_byte!($vm, $chunk) as usize]
    };
}

macro_rules! binary_op {
    ($vm:expr, $op:tt) => {{
        let b = $vm.pop();
        let top = $vm.stack.last_mut().expect("Stack underflow");
        *top = *top $op b;
    }};
}

pub struct VM<'a> {
    chunk: Option<&'a Chunk>,
    ip: usize,
    stack: Vec<Value>,
}

pub enum InterpretResult {
    Ok,
    CompileError,
    RuntimeError,
}

impl<'a> VM<'a> {
    pub fn new() -> Self {
        VM {
            chunk: None,
            ip: 0,
            stack: Vec::new(),
        }
    }

    pub fn interpret(&mut self, source: &str) -> InterpretResult {
        compile(source);
        InterpretResult::Ok
    }

    pub fn push(&mut self, value: Value) {
        self.stack.push(value);
    }

    pub fn pop(&mut self) -> Value {
        self.stack.pop().expect("Stack underflow")
    }
}

fn run(vm: &mut VM) -> InterpretResult {
    let chunk = vm.chunk.unwrap();

    loop {
        #[cfg(feature = "debug_trace_execution")]
        {
            print!("          ");
            for value in &vm.stack {
                print!("[ {} ]", value);
            }
            println!();
            disassemble_instruction(chunk, vm.ip);
        }

        let instruction = read_byte!(vm, chunk);

        match instruction {
            x if x == OpCode::Constant as u8 => {
                let constant = read_constant!(vm, chunk);
                vm.push(constant);
            }
            x if x == OpCode::Add as u8 => binary_op!(vm, +),
            x if x == OpCode::Subtract as u8 => binary_op!(vm, -),
            x if x == OpCode::Multiply as u8 => binary_op!(vm, *),
            x if x == OpCode::Divide as u8 => binary_op!(vm, /),
            x if x == OpCode::Negate as u8 => {
                let top = vm.stack.last_mut().expect("Stack underflow");
                *top = -*top;
            }
            x if x == OpCode::Return as u8 => {
                println!("{}", vm.pop());
                return InterpretResult::Ok;
            }
            _ => {
                println!("Unknown opcode {}", instruction);
                return InterpretResult::RuntimeError;
            }
        }
    }
}
