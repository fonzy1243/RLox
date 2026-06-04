use crate::chunk::{Chunk, OpCode};
use crate::compiler::compile;
use crate::object::{
    Obj, ObjString, ObjType, allocate_list, allocate_string, free_object, take_string,
};
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

macro_rules! read_short {
    ($vm:expr, $chunk:expr) => {{
        let lo = read_byte!($vm, $chunk) as u16;
        let hi = read_byte!($vm, $chunk) as u16;
        (hi << 8) | lo
    }};
}

macro_rules! read_constant {
    ($vm:expr, $chunk:expr) => {
        $chunk.constants[read_byte!($vm, $chunk) as usize]
    };
}

macro_rules! read_string {
    ($vm:expr, $chunk:expr) => {{
        let constant = read_constant!($vm, $chunk);
        constant.as_obj() as *mut ObjString
    }};
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
    pub globals: Table,
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
            globals: Table::new(),
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

        let chars = format!("{}{}", a, b);

        let result = take_string(self, chars);

        self.push(Value::Obj(result as *mut Obj));
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
            x if x == OpCode::ConstantLong as u8 => {
                let lo = read_byte!(vm, chunk) as usize;
                let mi = read_byte!(vm, chunk) as usize;
                let hi = read_byte!(vm, chunk) as usize;

                let index = lo | (mi << 8) | (hi << 16);

                let constant = chunk.constants[index];
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
            x if x == OpCode::IntDivide as u8 => {
                if !vm.peek(0).is_number() || !vm.peek(1).is_number() {
                    vm.runtime_error("Operands must be numbers.");
                    return InterpretResult::RuntimeError;
                }

                let b = vm.pop().as_number();
                let top = vm.stack.last_mut().expect("Stack underflow");
                let a = top.as_number();

                *top = Value::Number((a / b).trunc());
            }
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
            x if x == OpCode::Pop as u8 => {
                vm.pop();
            }
            x if x == OpCode::GetLocal as u8 => {
                let slot = read_byte!(vm, chunk) as usize;
                let value = vm.stack[slot];
                vm.push(value);
            }
            x if x == OpCode::SetLocal as u8 => {
                let slot = read_byte!(vm, chunk) as usize;
                vm.stack[slot] = vm.peek(0);
            }
            x if x == OpCode::GetLocalLong as u8 => {
                let slot = read_short!(vm, chunk) as usize;
                let value = vm.stack[slot];
                vm.push(value);
            }
            x if x == OpCode::SetLocalLong as u8 => {
                let slot = read_short!(vm, chunk) as usize;
                vm.stack[slot] = vm.peek(0);
            }
            x if x == OpCode::GetGlobal as u8 => {
                let name = read_string!(vm, chunk);

                if let Some(value) = vm.globals.get(name) {
                    vm.push(value);
                } else {
                    let name_str = ObjString::as_str(name);
                    vm.runtime_error(&format!("Undefined variable '{}'.", name_str));
                    return InterpretResult::RuntimeError;
                }
            }
            x if x == OpCode::DefineGlobal as u8 => {
                let name = read_string!(vm, chunk);
                let value = vm.peek(0);

                vm.globals.set(name, value);

                vm.pop();
            }
            x if x == OpCode::SetGlobal as u8 => {
                let name = read_string!(vm, chunk);
                let value = vm.peek(0);

                if vm.globals.set(name, value) {
                    vm.globals.delete(name);

                    let name_str = ObjString::as_str(name);
                    vm.runtime_error(&format!("Undefined variable '{}'.", name_str));
                    return InterpretResult::RuntimeError;
                }
            }
            x if x == OpCode::BuildList as u8 => {
                let item_count = read_byte!(vm, chunk) as usize;
                let mut items = Vec::with_capacity(item_count);

                for _ in 0..item_count {
                    items.push(vm.pop());
                }
                items.reverse();

                let list_ptr = crate::object::allocate_list(vm, items);
                vm.push(Value::Obj(list_ptr as *mut Obj));
            }
            x if x == OpCode::BuildListLong as u8 => {
                let item_count = read_short!(vm, chunk) as usize;
                let mut items = Vec::with_capacity(item_count);

                for _ in 0..item_count {
                    items.push(vm.pop());
                }
                items.reverse();

                let list_ptr = allocate_list(vm, items);
                vm.push(Value::Obj(list_ptr as *mut Obj));
            }
            x if x == OpCode::GetIndex as u8 => {
                let index_val = vm.pop();
                let list_val = vm.pop();

                if !list_val.is_list() {
                    vm.runtime_error("Only lists can be subscripted.");
                    return InterpretResult::RuntimeError;
                }
                if !index_val.is_number() {
                    vm.runtime_error("List index must be a number.");
                    return InterpretResult::RuntimeError;
                }

                let list = unsafe { &*list_val.as_list() };
                let index = index_val.as_number() as usize;

                if index >= list.items.len() {
                    vm.runtime_error("List index out of bounds.");
                    return InterpretResult::RuntimeError;
                }

                vm.push(list.items[index]);
            }
            x if x == OpCode::SetIndex as u8 => {
                let value = vm.pop();
                let index_val = vm.pop();
                let list_val = vm.pop();

                if !list_val.is_list() {
                    vm.runtime_error("Only lists can be subscripted.");
                    return InterpretResult::RuntimeError;
                }
                if !index_val.is_number() {
                    vm.runtime_error("List index must be a number.");
                    return InterpretResult::RuntimeError;
                }

                let list = unsafe { &mut *list_val.as_list() };
                let index = index_val.as_number() as usize;

                if index >= list.items.len() {
                    vm.runtime_error("List index out of bounds.");
                    return InterpretResult::RuntimeError;
                }

                list.items[index] = value;

                vm.push(value);
            }
            x if x == OpCode::Print as u8 => {
                println!("{}", vm.pop());
            }
            x if x == OpCode::Jump as u8 => {
                let offset = read_short!(vm, chunk) as usize;
                vm.ip += offset;
            }
            x if x == OpCode::JumpIfFalse as u8 => {
                let offset = read_short!(vm, chunk) as usize;
                if vm.peek(0).is_falsy() {
                    vm.ip += offset;
                }
            }
            x if x == OpCode::Loop as u8 => {
                let offset = read_short!(vm, chunk) as usize;
                vm.ip -= offset;
            }
            x if x == OpCode::Return as u8 => {
                return InterpretResult::Ok;
            }
            _ => {
                println!("Unknown opcode {}", instruction);
                return InterpretResult::RuntimeError;
            }
        }
    }
}
