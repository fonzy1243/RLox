use crate::chunk::{Chunk, OpCode};
use crate::compiler::compile;
use crate::value::{Value, values_equal};

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
    ($vm:expr, $wrap:expr, $op:tt) => {{
        if !$vm.peek(0).is_number() || !$vm.peek(1).is_number() {
            $vm.runtime_error("Operands must be numbers.");
            return InterpretResult::RuntimeError;
        }
        let b = $vm.pop().as_number();
        let top = $vm.stack.last_mut().expect("Stack underflow");
        *top = $wrap(top.as_number() $op b);
    }};
}

pub struct VM {
    chunk: Option<Chunk>,
    ip: usize,
    stack: Vec<Value>,
}

pub enum InterpretResult {
    Ok,
    CompileError,
    RuntimeError,
}

impl VM {
    pub fn new() -> Self {
        VM {
            chunk: None,
            ip: 0,
            stack: Vec::new(),
        }
    }

    pub fn interpret(&mut self, source: &str) -> InterpretResult {
        let mut chunk = Chunk::new();

        if !compile(source, &mut chunk) {
            return InterpretResult::CompileError;
        }

        self.chunk = Some(chunk);
        self.ip = 0;

        run(self)
    }

    pub fn push(&mut self, value: Value) {
        self.stack.push(value);
    }

    pub fn pop(&mut self) -> Value {
        self.stack.pop().expect("Stack underflow")
    }

    pub fn peek(&self, distance: usize) -> Value {
        self.stack[self.stack.len() - 1 - distance]
    }

    fn runtime_error(&mut self, message: &str) {
        eprintln!("{}", message);

        let instruction = self.ip - 1;
        let line = self.chunk.as_ref().unwrap().get_line(instruction);
        eprintln!("[line {}] in script", line);

        self.stack.clear();
    }
}

fn run(vm: &mut VM) -> InterpretResult {
    loop {
        #[cfg(feature = "debug_trace_execution")]
        {
            print!("          ");
            for value in &vm.stack {
                print!("[ {} ]", value);
            }
            println!();
            disassemble_instruction(vm.chunk.as_ref().unwrap(), vm.ip);
        }

        let chunk = vm.chunk.as_ref().unwrap();
        let instruction = read_byte!(vm, chunk);

        match instruction {
            x if x == OpCode::Constant as u8 => {
                let constant = read_constant!(vm, chunk);
                vm.push(constant);
            }
            x if x == OpCode::Nil as u8 => vm.push(Value::Nil),
            x if x == OpCode::True as u8 => vm.push(Value::Bool(true)),
            x if x == OpCode::False as u8 => vm.push(Value::Bool(false)),
            x if x == OpCode::Equal as u8 => {
                let b = vm.pop();
                let a = vm.pop();
                vm.push(Value::Bool(values_equal(a, b)));
            }
            x if x == OpCode::Greater as u8 => binary_op!(vm, Value::Bool, >),
            x if x == OpCode::Less as u8 => binary_op!(vm, Value::Bool, <),
            x if x == OpCode::Add as u8 => binary_op!(vm, Value::Number, +),
            x if x == OpCode::Subtract as u8 => binary_op!(vm, Value::Number, -),
            x if x == OpCode::Multiply as u8 => binary_op!(vm, Value::Number, *),
            x if x == OpCode::Divide as u8 => binary_op!(vm, Value::Number, /),
            x if x == OpCode::Not as u8 => {
                let top = vm.stack.last_mut().expect("Stack underflow");
                *top = Value::Bool(top.is_falsy());
            }
            x if x == OpCode::Negate as u8 => {
                if !vm.peek(0).is_number() {
                    vm.runtime_error("Operand must be a number.");
                    return InterpretResult::RuntimeError;
                }
                let top = vm.stack.last_mut().expect("Stack underflow");
                *top = Value::Number(-top.as_number())
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
