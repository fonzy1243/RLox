use crate::chunk::{Chunk, OpCode};
use crate::compiler::compile;
use crate::object::{Obj, ObjString, ObjType, allocate_string, free_object};
use crate::table::Table;
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
    pub objects: *mut Obj,
    pub strings: Table,
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
            objects: std::ptr::null_mut(),
            strings: Table::new(),
        }
    }

    pub fn interpret(&mut self, source: &str) -> InterpretResult {
        let mut chunk = Chunk::new();

        if !compile(source, &mut chunk, self) {
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

    fn concatenate(&mut self) {
        let b_val = self.pop();
        let a_val = self.pop();

        let b = b_val.as_cstring();
        let a = a_val.as_cstring();
        let len = a.len() + b.len();

        let mut hash = 2166136261u32;
        for byte in a.bytes().chain(b.bytes()) {
            hash ^= byte as u32;
            hash = hash.wrapping_mul(16777619);
        }

        let layout = std::alloc::Layout::from_size_align(
            std::mem::size_of::<crate::object::ObjString>() + len,
            std::mem::align_of::<crate::object::ObjString>(),
        )
        .unwrap();

        let result_ptr = unsafe {
            let ptr = std::alloc::alloc(layout) as *mut crate::object::ObjString;
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }

            (*ptr).obj = Obj {
                obj_type: ObjType::String,
                next: self.objects,
            };
            (*ptr).length = len;
            (*ptr).hash = hash;
            self.objects = ptr as *mut Obj;

            let chars_ptr = (ptr as *mut u8).add(std::mem::size_of::<ObjString>());
            std::ptr::copy_nonoverlapping(a.as_ptr(), chars_ptr, a.len());
            std::ptr::copy_nonoverlapping(b.as_ptr(), chars_ptr.add(a.len()), b.len());

            ptr
        };

        self.push(Value::Obj(result_ptr as *mut Obj));
    }

    fn runtime_error(&mut self, message: &str) {
        eprintln!("{}", message);

        let instruction = self.ip - 1;
        let line = self.chunk.as_ref().unwrap().get_line(instruction);
        eprintln!("[line {}] in script", line);

        self.stack.clear();
    }
}

impl Drop for VM {
    fn drop(&mut self) {
        let mut object = self.objects;
        while !object.is_null() {
            unsafe {
                let next = (*object).next;
                free_object(object);
                object = next;
            }
        }
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
            x if x == OpCode::Add as u8 => {
                if vm.peek(0).is_string() && vm.peek(1).is_string() {
                    vm.concatenate();
                } else if vm.peek(0).is_number() && vm.peek(1).is_number() {
                    let b = vm.pop().as_number();
                    let a = vm.pop().as_number();
                    vm.push(Value::Number(a + b));
                } else {
                    vm.runtime_error("Operands must be two numbers or two strings.");
                    return InterpretResult::RuntimeError;
                }
            }
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
