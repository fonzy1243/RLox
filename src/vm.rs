use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunk::{Chunk, OpCode};
use crate::compiler::compile;
use crate::object::{
    NativeFn, Obj, ObjFunction, ObjString, ObjType, allocate_list, allocate_native,
    allocate_string, copy_string, free_object, take_string,
};
use crate::table::Table;
use crate::value::{Value, values_equal};

#[cfg(feature = "debug_trace_execution")]
use crate::debug::disassemble_instruction;

const FRAMES_MAX: usize = 256;
const STACK_MAX: usize = FRAMES_MAX * 256;

macro_rules! read_byte {
    ($ip:expr, $chunk:expr) => {{
        unsafe {
            let b = *$chunk.code.get_unchecked($ip);
            $ip += 1;
            b
        }
    }};
}

macro_rules! read_short {
    ($ip:expr, $chunk:expr) => {{
        unsafe {
            let b1 = *$chunk.code.get_unchecked($ip);
            let b2 = *$chunk.code.get_unchecked($ip + 1);
            $ip += 2;
            u16::from_le_bytes([b1, b2])
        }
    }};
}

macro_rules! read_constant {
    ($ip:expr, $chunk:expr) => {
        unsafe {
            *$chunk
                .constants
                .get_unchecked(read_byte!($ip, $chunk) as usize)
        }
    };
}

macro_rules! read_string {
    ($ip:expr, $chunk:expr) => {{
        let constant = read_constant!($ip, $chunk);
        constant.as_obj() as *mut ObjString
    }};
}

macro_rules! binary_op {
    ($vm:expr, $ip:expr, $wrap:expr, $op:tt) => {{
        if !$vm.peek(0).is_number() || !$vm.peek(1).is_number() {
            $vm.frames[$vm.frame_count - 1].ip = $ip;
            $vm.runtime_error("Operands must be numbers.");
            return InterpretResult::RuntimeError;
        }
        let b = $vm.pop().as_number();
        unsafe {
            let top_ptr = $vm.stack_top.sub(1);
            *top_ptr = $wrap((*top_ptr).as_number() $op b);
        }
    }};
}

#[derive(Clone, Copy)]
pub struct CallFrame {
    pub function: *mut ObjFunction,
    pub ip: usize,
    pub slots: *mut Value,
}

pub struct VM {
    pub frames: [CallFrame; FRAMES_MAX],
    pub frame_count: usize,
    pub stack: Box<[Value; STACK_MAX]>,
    pub stack_top: *mut Value,
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
        let dummy_frame = CallFrame {
            function: std::ptr::null_mut(),
            ip: 0,
            slots: std::ptr::null_mut(),
        };

        let mut stack: Box<[Value; STACK_MAX]> = vec![Value::Nil; STACK_MAX].try_into().unwrap();
        let stack_top = stack.as_mut_ptr();

        let mut vm = VM {
            frames: [dummy_frame; FRAMES_MAX],
            frame_count: 0,
            stack: stack,
            stack_top: stack_top,
            objects: std::ptr::null_mut(),
            strings: Table::new(),
            globals: Table::new(),
        };

        vm.define_native("clock", clock_native);
        vm
    }

    pub fn interpret(&mut self, source: &str) -> InterpretResult {
        let function = match compile(source, self) {
            Some(func) => func,
            None => return InterpretResult::CompileError,
        };

        self.push(Value::Obj(function as *mut Obj));

        self.call(function, 0);

        run(self)
    }

    pub fn push(&mut self, value: Value) {
        unsafe {
            *self.stack_top = value;
            self.stack_top = self.stack_top.add(1);
        }
    }

    pub fn pop(&mut self) -> Value {
        unsafe {
            self.stack_top = self.stack_top.sub(1);
            *self.stack_top
        }
    }

    pub fn peek(&self, distance: usize) -> Value {
        unsafe { *self.stack_top.sub(1 + distance) }
    }

    fn call(&mut self, function: *mut ObjFunction, arg_count: usize) -> bool {
        let arity = unsafe { (*function).arity };
        if arg_count != arity {
            self.runtime_error(&format!(
                "Expected {} arguments but got {}.",
                arity, arg_count
            ));
            return false;
        }

        if self.frame_count == FRAMES_MAX {
            self.runtime_error("Stack overflow.");
            return false;
        }

        self.frames[self.frame_count] = CallFrame {
            function,
            ip: 0,
            slots: unsafe { self.stack_top.sub(arg_count + 1) },
        };

        self.frame_count += 1;
        true
    }

    fn call_value(&mut self, callee: Value, arg_count: usize) -> bool {
        if callee.is_function() {
            return self.call(callee.as_function(), arg_count);
        } else if callee.is_native() {
            let native = callee.as_native();
            unsafe {
                let args_start = self.stack_top.sub(arg_count);
                let args_slice = std::slice::from_raw_parts(args_start, arg_count);
                let result = ((*native).function)(arg_count, args_slice);
                self.stack_top = args_start.sub(1);

                self.push(result);
                return true;
            }
        }

        self.runtime_error("Can only call functions and classes.");
        false
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

        for i in (0..self.frame_count).rev() {
            let frame = &self.frames[i];
            let instruction = frame.ip - 1;
            let line = unsafe { (*frame.function).chunk.get_line(instruction) };

            if unsafe { (*frame.function).name.is_null() } {
                eprintln!("[line {}] in script", line);
            } else {
                let name = unsafe { ObjString::as_str((*frame.function).name) };
                eprintln!("[line {}] in {}()", line, name);
            }
        }

        self.stack_top = self.frames[self.frame_count].slots;
        self.frame_count = 0;
    }

    fn define_native(&mut self, name: &str, function: NativeFn) {
        let name_obj = copy_string(self, name);
        let native_obj = allocate_native(self, function);

        self.push(Value::Obj(name_obj as *mut Obj));
        self.push(Value::Obj(native_obj as *mut Obj));

        self.globals.set(name_obj, self.stack[1]);

        self.pop();
        self.pop();
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

// Native functions
fn clock_native(_: usize, _: &[Value]) -> Value {
    let start = SystemTime::now();
    let since_the_epoch = start
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    Value::Number(since_the_epoch.as_secs_f64())
}
// ----------------

fn run(vm: &mut VM) -> InterpretResult {
    let mut ip = vm.frames[vm.frame_count - 1].ip;
    let mut chunk = unsafe { &(*vm.frames[vm.frame_count - 1].function).chunk };

    loop {
        #[cfg(feature = "debug_trace_execution")]
        {
            print!("          ");
            unsafe {
                let stack_len = vm.stack_top.offset_from(vm.stack.as_ptr()) as usize;
                let stack_slice = std::slice::from_raw_parts(vm.stack.as_ptr(), stack_len);

                for value in stack_slice {
                    print!("[ {} ]", value);
                }
            }
            println!();

            unsafe {
                disassemble_instruction(chunk, ip);
            }
        }

        let instruction = unsafe { *chunk.code.get_unchecked(ip) };
        ip += 1;

        match instruction {
            x if x == OpCode::Constant as u8 => {
                let constant = read_constant!(ip, chunk);
                vm.push(constant);
            }
            x if x == OpCode::ConstantLong as u8 => {
                let lo = read_byte!(ip, chunk) as usize;
                let mi = read_byte!(ip, chunk) as usize;
                let hi = read_byte!(ip, chunk) as usize;

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
            x if x == OpCode::Greater as u8 => binary_op!(vm, ip, Value::Bool, >),
            x if x == OpCode::Less as u8 => binary_op!(vm, ip, Value::Bool, <),
            x if x == OpCode::Add as u8 => {
                if vm.peek(0).is_string() && vm.peek(1).is_string() {
                    vm.concatenate();
                } else if vm.peek(0).is_number() && vm.peek(1).is_number() {
                    let b = vm.pop().as_number();
                    let a = vm.pop().as_number();
                    vm.push(Value::Number(a + b));
                } else {
                    vm.frames[vm.frame_count - 1].ip = ip;
                    vm.runtime_error("Operands must be two numbers or two strings.");
                    return InterpretResult::RuntimeError;
                }
            }
            x if x == OpCode::Subtract as u8 => binary_op!(vm, ip, Value::Number, -),
            x if x == OpCode::Multiply as u8 => binary_op!(vm, ip, Value::Number, *),
            x if x == OpCode::Divide as u8 => binary_op!(vm, ip, Value::Number, /),
            x if x == OpCode::IntDivide as u8 => {
                if !vm.peek(0).is_number() || !vm.peek(1).is_number() {
                    vm.frames[vm.frame_count - 1].ip = ip;
                    vm.runtime_error("Operands must be numbers.");
                    return InterpretResult::RuntimeError;
                }

                let b = vm.pop().as_number();
                unsafe {
                    let top_ptr = vm.stack_top.sub(1);
                    *top_ptr = Value::Number(((*top_ptr).as_number() / b).trunc());
                }
            }
            x if x == OpCode::Not as u8 => unsafe {
                let top_ptr = vm.stack_top.sub(1);
                *top_ptr = Value::Bool((*top_ptr).is_falsy());
            },
            x if x == OpCode::Negate as u8 => {
                if !vm.peek(0).is_number() {
                    vm.frames[vm.frame_count - 1].ip = ip;
                    vm.runtime_error("Operand must be a number.");
                    return InterpretResult::RuntimeError;
                }

                unsafe {
                    let top_ptr = vm.stack_top.sub(1);
                    *top_ptr = Value::Number(-(*top_ptr).as_number());
                }
            }
            x if x == OpCode::Pop as u8 => {
                vm.pop();
            }
            x if x == OpCode::Dup as u8 => {
                let value = vm.peek(0);
                vm.push(value);
            }
            x if x == OpCode::GetLocal as u8 => {
                let slot = read_byte!(ip, chunk) as usize;
                unsafe {
                    let frame = vm.frames.get_unchecked(vm.frame_count - 1);
                    let value = *frame.slots.add(slot);
                    vm.push(value);
                }
            }
            x if x == OpCode::SetLocal as u8 => {
                let slot = read_byte!(ip, chunk) as usize;
                unsafe {
                    let frame = vm.frames.get_unchecked(vm.frame_count - 1);
                    *frame.slots.add(slot) = vm.peek(0);
                }
            }
            x if x == OpCode::GetLocalLong as u8 => {
                let slot = read_short!(ip, chunk) as usize;
                unsafe {
                    let frame = vm.frames.get_unchecked(vm.frame_count - 1);
                    let value = *frame.slots.add(slot);
                    vm.push(value);
                }
            }
            x if x == OpCode::SetLocalLong as u8 => {
                let slot = read_short!(ip, chunk) as usize;
                unsafe {
                    let frame = vm.frames.get_unchecked(vm.frame_count - 1);
                    *frame.slots.add(slot) = vm.peek(0);
                }
            }
            x if x == OpCode::GetGlobal as u8 => {
                let name = read_string!(ip, chunk);

                if let Some(value) = vm.globals.get(name) {
                    vm.push(value);
                } else {
                    let name_str = ObjString::as_str(name);
                    vm.frames[vm.frame_count - 1].ip = ip;
                    vm.runtime_error(&format!("Undefined variable '{}'.", name_str));
                    return InterpretResult::RuntimeError;
                }
            }
            x if x == OpCode::DefineGlobal as u8 => {
                let name = read_string!(ip, chunk);
                let value = vm.peek(0);

                vm.globals.set(name, value);

                vm.pop();
            }
            x if x == OpCode::SetGlobal as u8 => {
                let name = read_string!(ip, chunk);
                let value = vm.peek(0);

                if vm.globals.set(name, value) {
                    vm.globals.delete(name);

                    let name_str = ObjString::as_str(name);
                    vm.frames[vm.frame_count - 1].ip = ip;
                    vm.runtime_error(&format!("Undefined variable '{}'.", name_str));
                    return InterpretResult::RuntimeError;
                }
            }
            x if x == OpCode::BuildList as u8 => {
                let item_count = read_byte!(ip, chunk) as usize;
                let mut items = Vec::with_capacity(item_count);

                for _ in 0..item_count {
                    items.push(vm.pop());
                }
                items.reverse();

                let list_ptr = crate::object::allocate_list(vm, items);
                vm.push(Value::Obj(list_ptr as *mut Obj));
            }
            x if x == OpCode::BuildListLong as u8 => {
                let item_count = read_short!(ip, chunk) as usize;
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
                    vm.frames[vm.frame_count - 1].ip = ip;
                    vm.runtime_error("Only lists can be subscripted.");
                    return InterpretResult::RuntimeError;
                }
                if !index_val.is_number() {
                    vm.frames[vm.frame_count - 1].ip = ip;
                    vm.runtime_error("List index must be a number.");
                    return InterpretResult::RuntimeError;
                }

                let list = unsafe { &*list_val.as_list() };
                let index = index_val.as_number() as usize;

                if index >= list.items.len() {
                    vm.frames[vm.frame_count - 1].ip = ip;
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
                    vm.frames[vm.frame_count - 1].ip = ip;
                    vm.runtime_error("Only lists can be subscripted.");
                    return InterpretResult::RuntimeError;
                }
                if !index_val.is_number() {
                    vm.frames[vm.frame_count - 1].ip = ip;
                    vm.runtime_error("List index must be a number.");
                    return InterpretResult::RuntimeError;
                }

                let list = unsafe { &mut *list_val.as_list() };
                let index = index_val.as_number() as usize;

                if index >= list.items.len() {
                    vm.frames[vm.frame_count - 1].ip = ip;
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
                let offset = read_short!(ip, chunk) as usize;
                ip += offset;
            }
            x if x == OpCode::JumpIfFalse as u8 => {
                let offset = read_short!(ip, chunk) as usize;
                if vm.peek(0).is_falsy() {
                    ip += offset;
                }
            }
            x if x == OpCode::Loop as u8 => {
                let offset = read_short!(ip, chunk) as usize;
                ip -= offset;
            }
            x if x == OpCode::Call as u8 => {
                let arg_count = read_byte!(ip, chunk) as usize;

                // Sync the current frame ip back to the VM state before calling another function
                vm.frames[vm.frame_count - 1].ip = ip;

                if !vm.call_value(vm.peek(arg_count), arg_count) {
                    return InterpretResult::RuntimeError;
                }

                // Reload context for the new frame
                ip = vm.frames[vm.frame_count - 1].ip;
                chunk = unsafe { &(*vm.frames[vm.frame_count - 1].function).chunk };
            }
            x if x == OpCode::Return as u8 => {
                let result = vm.pop();
                vm.frame_count -= 1;

                if vm.frame_count == 0 {
                    vm.pop();
                    return InterpretResult::Ok;
                }

                let slots = vm.frames[vm.frame_count].slots;
                vm.stack_top = slots;
                vm.push(result);

                // Reload context for the parent frame
                ip = vm.frames[vm.frame_count - 1].ip;
                chunk = unsafe { &(*vm.frames[vm.frame_count - 1].function).chunk };
            }
            _ => {
                println!("Unknown opcode {}", instruction);
                return InterpretResult::RuntimeError;
            }
        }
    }
}
