use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunk::{Chunk, OpCode};
use crate::compiler::compile;
use crate::object::{
    NativeFn, Obj, ObjBoundMethod, ObjClass, ObjClosure, ObjFunction, ObjInstance, ObjList,
    ObjString, ObjType, ObjUpvalue, allocate_bound_method, allocate_class, allocate_closure,
    allocate_instance, allocate_list, allocate_native, allocate_string, capture_upvalue,
    close_upvalues, copy_string, free_object, take_string,
};
use crate::table::Table;
use crate::value::{Value, values_equal};

#[cfg(feature = "debug_trace_execution")]
use crate::debug::disassemble_instruction;

const FRAMES_MAX: usize = 256;
const STACK_MAX: usize = FRAMES_MAX * 256;
const GC_HEAP_GROW_FACTOR: usize = 2;

macro_rules! read_byte {
    ($ip:expr) => {{
        unsafe {
            let b = *$ip;
            $ip = $ip.add(1);
            b
        }
    }};
}

macro_rules! read_short {
    ($ip:expr) => {{
        unsafe {
            let b1 = *$ip;
            let b2 = *$ip.add(1);
            $ip = $ip.add(2);
            u16::from_le_bytes([b1, b2])
        }
    }};
}

macro_rules! read_constant {
    ($ip:expr, $chunk:expr) => {{
        let index = read_byte!($ip) as usize;
        unsafe { *$chunk.constants.get_unchecked(index) }
    }};
}

macro_rules! read_string {
    ($ip:expr, $chunk:expr) => {{
        let constant = read_constant!($ip, $chunk);
        constant.as_obj() as *mut ObjString
    }};
}

macro_rules! binary_op {
    ($vm:expr, $ip:expr, $chunk:expr, $wrap:expr, $op:tt) => {{
        if !$vm.peek(0).is_number() || !$vm.peek(1).is_number() {
            let offset = unsafe { $ip.offset_from($chunk.code.as_ptr()) } as usize;
            $vm.frames[$vm.frame_count - 1].ip = offset;
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
    pub closure: *mut ObjClosure,
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
    pub open_upvalues: *mut ObjUpvalue,
    pub compiler_roots: Vec<*mut ObjFunction>,
    pub gray_stack: Vec<*mut Obj>,
    pub bytes_allocated: usize,
    pub next_gc: usize,
    pub init_string: *mut ObjString,
}

pub enum InterpretResult {
    Ok,
    CompileError,
    RuntimeError,
}

impl VM {
    pub fn new() -> Self {
        let dummy_frame = CallFrame {
            closure: std::ptr::null_mut(),
            ip: 0,
            slots: std::ptr::null_mut(),
        };

        let mut vm = VM {
            frames: [dummy_frame; FRAMES_MAX],
            frame_count: 0,
            stack: vec![Value::Nil; STACK_MAX].try_into().unwrap(),
            stack_top: std::ptr::null_mut(),
            objects: std::ptr::null_mut(),
            strings: Table::new(),
            globals: Table::new(),
            open_upvalues: std::ptr::null_mut(),
            compiler_roots: Vec::new(),
            gray_stack: Vec::new(),
            bytes_allocated: 0,
            next_gc: 1024 * 1024,
            init_string: std::ptr::null_mut(),
        };

        vm.stack_top = vm.stack.as_mut_ptr();

        vm.define_native("clock", clock_native);
        vm.init_string = copy_string(&mut vm, "init");

        vm
    }

    pub fn interpret(&mut self, source: &str) -> InterpretResult {
        let function = match compile(source, self) {
            Some(func) => func,
            None => return InterpretResult::CompileError,
        };

        self.push(Value::Obj(function as *mut Obj));
        let closure = allocate_closure(self, function);
        self.pop();
        self.push(Value::Obj(closure as *mut Obj));

        self.call(closure, 0);

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

    fn call(&mut self, closure: *mut ObjClosure, arg_count: usize) -> bool {
        let arity = unsafe { (*(*closure).function).arity };
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
            closure,
            ip: 0,
            slots: unsafe { self.stack_top.sub(arg_count + 1) },
        };

        self.frame_count += 1;
        true
    }

    fn call_value(&mut self, callee: Value, arg_count: usize) -> bool {
        if callee.is_bound_method() {
            let bound = unsafe { &*callee.as_bound_method() };
            let receiver = bound.receiver;
            let method = bound.method;
            unsafe {
                *self.stack_top.sub(arg_count + 1) = receiver;
            }
            return self.call(method, arg_count);
        } else if callee.is_class() {
            let class = callee.as_class();
            let instance = allocate_instance(self, class);
            unsafe {
                *self.stack_top.sub(arg_count + 1) = Value::Obj(instance as *mut Obj);
            }

            let init_string = self.init_string;
            if let Some(initializer) = unsafe { (*class).methods.get(init_string) } {
                return self.call(initializer.as_closure(), arg_count);
            } else if arg_count != 0 {
                self.runtime_error(&format!("Expected 0 arguments but got {}.", arg_count));
                return false;
            }

            return true;
        } else if callee.is_closure() {
            return self.call(callee.as_closure(), arg_count);
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
        let b = self.peek(0).as_cstring().to_owned();
        let a = self.peek(1).as_cstring().to_owned();

        let chars = format!("{}{}", a, b);
        let result = take_string(self, chars);

        self.pop();
        self.pop();
        self.push(Value::Obj(result as *mut Obj));
    }

    fn runtime_error(&mut self, message: &str) {
        eprintln!("{}", message);

        for i in (0..self.frame_count).rev() {
            let frame = &self.frames[i];
            let instruction = frame.ip - 1;
            let line = unsafe { (*(*frame.closure).function).chunk.get_line(instruction) };

            if unsafe { (*(*frame.closure).function).name.is_null() } {
                eprintln!("[line {}] in script", line);
            } else {
                let name = unsafe { ObjString::as_str((*(*frame.closure).function).name) };
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
        self.init_string = std::ptr::null_mut();
        let mut object = self.objects;
        while !object.is_null() {
            unsafe {
                let next = (*object).next;
                free_object(self, object);
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

// Garbage Collection
pub fn collect_garbage(vm: &mut VM) {
    #[cfg(feature = "debug_log_gc")]
    let before = vm.bytes_allocated;

    #[cfg(feature = "debug_log_gc")]
    println!("-- gc begin");

    mark_roots(vm);
    trace_references(vm);
    vm.strings.remove_white();
    sweep(vm);

    vm.next_gc = vm.bytes_allocated * GC_HEAP_GROW_FACTOR;

    #[cfg(feature = "debug_log_gc")]
    println!(
        "-- gc end. collected {} bytes (from {} to {}), next at {}",
        before - vm.bytes_allocated,
        before,
        vm.bytes_allocated,
        vm.next_gc
    );
}

pub fn mark_object(vm: &mut VM, object: *mut Obj) {
    if object.is_null() {
        return;
    }

    unsafe {
        if (*object).is_marked {
            return;
        }

        #[cfg(feature = "debug_log_gc")]
        println!("{:p} mark {}", object, Value::Obj(object));

        (*object).is_marked = true;
    }

    vm.gray_stack.push(object);
}

pub fn mark_value(vm: &mut VM, value: Value) {
    if let Value::Obj(ptr) = value {
        mark_object(vm, ptr);
    }
}

fn blacken_object(vm: &mut VM, object: *mut Obj) {
    #[cfg(feature = "debug_log_gc")]
    println!("{:p} blacken {}", object, Value::Obj(object));

    unsafe {
        match (*object).obj_type {
            ObjType::Native | ObjType::String => {}
            ObjType::Upvalue => {
                let upvalue = object as *mut ObjUpvalue;
                mark_value(vm, (*upvalue).closed);
            }
            ObjType::Function => {
                let function = object as *mut ObjFunction;
                mark_object(vm, (*function).name as *mut Obj);
                let constants: Vec<Value> = (*function).chunk.constants.clone();
                for value in constants {
                    mark_value(vm, value);
                }
            }
            ObjType::Instance => {
                let instance = object as *mut ObjInstance;
                unsafe {
                    mark_object(vm, (*instance).class as *mut Obj);
                    let fields_ptr = &(*instance).fields as *const Table;
                    mark_table(vm, &*fields_ptr);
                }
            }
            ObjType::BoundMethod => {
                let bound = object as *mut ObjBoundMethod;
                unsafe {
                    mark_value(vm, (*bound).receiver);
                    mark_object(vm, (*bound).method as *mut Obj);
                }
            }
            ObjType::Class => {
                let class = object as *mut ObjClass;
                unsafe {
                    mark_object(vm, (*class).name as *mut Obj);
                    let methods_ptr = &(*class).methods as *const Table;
                    mark_table(vm, &*methods_ptr);
                }
            }
            ObjType::Closure => {
                let closure = object as *mut ObjClosure;
                mark_object(vm, (*closure).function as *mut Obj);
                for i in 0..(*closure).upvalue_count {
                    mark_object(vm, (*closure).upvalues[i] as *mut Obj);
                }
            }
            ObjType::List => {
                let list = object as *mut ObjList;
                let items: Vec<Value> = (*list).items.clone();
                for item in items {
                    mark_value(vm, item);
                }
            }
        }
    }
}

fn mark_table(vm: &mut VM, table: &Table) {
    for i in 0..table.capacity {
        unsafe {
            let entry = table.entries.add(i);
            mark_object(vm, (*entry).key as *mut Obj);
            mark_value(vm, (*entry).value);
        }
    }
}

fn mark_compiler_roots(vm: &mut VM) {
    let roots: Vec<*mut ObjFunction> = vm.compiler_roots.clone();
    for function in roots {
        mark_object(vm, function as *mut Obj);
    }
}

fn mark_roots(vm: &mut VM) {
    // Mark stack
    let mut slot = vm.stack.as_ptr() as *const Value;
    while slot < vm.stack_top as *const Value {
        unsafe {
            mark_value(vm, *slot);
        }
        slot = unsafe { slot.add(1) }
    }

    // Mark call frame closures
    for i in 0..vm.frame_count {
        mark_object(vm, vm.frames[i].closure as *mut Obj);
    }

    // Mark open upvalues
    let mut upvalue = vm.open_upvalues;
    while !upvalue.is_null() {
        mark_object(vm, upvalue as *mut Obj);
        upvalue = unsafe { (*upvalue).next };
    }

    // Mark globals
    let globals_ptr = &vm.globals as *const Table;
    mark_table(vm, unsafe { &*globals_ptr });

    // Mark compiler roots
    mark_compiler_roots(vm);

    // Mark init string
    mark_object(vm, vm.init_string as *mut Obj);
}

fn trace_references(vm: &mut VM) {
    while let Some(object) = vm.gray_stack.pop() {
        blacken_object(vm, object);
    }
}

fn sweep(vm: &mut VM) {
    let mut previous: *mut Obj = std::ptr::null_mut();
    let mut object = vm.objects;

    while !object.is_null() {
        unsafe {
            if (*object).is_marked {
                (*object).is_marked = false;
                previous = object;
                object = (*object).next;
            } else {
                let unreached = object;
                object = (*object).next;

                if !previous.is_null() {
                    (*previous).next = object;
                } else {
                    vm.objects = object;
                }

                free_object(vm, unreached);
            }
        }
    }
}
// -----------------

// Methods ---------
fn define_method(vm: &mut VM, name: *mut ObjString) {
    let method = vm.peek(0);
    let class = unsafe { &mut *vm.peek(1).as_class() };
    class.methods.set(name, method);
    vm.pop();
}

pub fn bind_method(vm: &mut VM, class: *mut ObjClass, name: *mut ObjString) -> bool {
    let method = unsafe { (*class).methods.get(name) };

    match method {
        None => {
            let name_str = unsafe { ObjString::as_str(name) };
            vm.runtime_error(&format!("Undefined property '{}'.", name_str));
            false
        }
        Some(method) => {
            let receiver = vm.peek(0);
            let bound = allocate_bound_method(vm, receiver, method.as_closure());
            vm.pop();
            vm.push(Value::Obj(bound as *mut Obj));
            true
        }
    }
}

fn invoke_from_class(
    vm: &mut VM,
    class: *mut ObjClass,
    name: *mut ObjString,
    arg_count: usize,
) -> bool {
    let method = unsafe { (*class).methods.get(name) };
    match method {
        None => {
            let name_str = unsafe { ObjString::as_str(name) };
            vm.runtime_error(&format!("Undefined property '{}'.", name_str));
            false
        }
        Some(method) => vm.call(method.as_closure(), arg_count),
    }
}

fn invoke(vm: &mut VM, name: *mut ObjString, arg_count: usize) -> bool {
    let receiver = vm.peek(arg_count);

    if !receiver.is_instance() {
        vm.runtime_error("Only instances have methods.");
        return false;
    }

    let instance = receiver.as_instance();

    if let Some(value) = unsafe { (*instance).fields.get(name) } {
        unsafe {
            *vm.stack_top.sub(arg_count + 1) = value;
        }
        return vm.call_value(value, arg_count);
    }

    invoke_from_class(vm, unsafe { (*instance).class }, name, arg_count)
}

fn call_list_method(vm: &mut VM, list_ptr: *mut ObjList, name: &str, arg_count: usize) -> bool {
    match name {
        "append" => {
            if arg_count != 1 {
                vm.runtime_error("append() takes 1 argument.");
                return false;
            }
            let value = vm.pop();
            vm.pop();
            unsafe {
                (*list_ptr).items.push(value);
            }
            vm.push(Value::Nil);
            true
        }
        "pop" => {
            if arg_count != 0 {
                vm.runtime_error("pop() takes 0 arguments.");
                return false;
            }
            vm.pop();
            let result = unsafe { (*list_ptr).items.pop().unwrap_or(Value::Nil) };
            vm.push(result);
            true
        }
        "len" => {
            if arg_count != 0 {
                vm.runtime_error("len() takes 0 arguments.");
                return false;
            }
            vm.pop();
            let len = unsafe { (*list_ptr).items.len() };
            vm.push(Value::Number(len as f64));
            true
        }
        "delete" => {
            if arg_count != 1 {
                vm.runtime_error("delete() takes 1 argument.");
                return false;
            }
            let index_val = vm.pop();
            vm.pop();
            if !index_val.is_number() {
                vm.runtime_error("delete() index must be a number.");
                return false;
            }
            let list = unsafe { &mut *list_ptr };
            let raw = index_val.as_number() as isize;
            let index = if raw < 0 {
                (list.items.len() as isize + raw) as usize
            } else {
                raw as usize
            };
            if index >= list.items.len() {
                vm.runtime_error("List index out of bounds.");
                return false;
            }
            list.items.remove(index);
            vm.push(Value::Nil);
            true
        }
        _ => {
            let msg = format!("List has no method '{}'.", name);
            vm.runtime_error(&msg);
            false
        }
    }
}
// ---------------

fn run(vm: &mut VM) -> InterpretResult {
    let mut chunk = unsafe { &(*(*vm.frames[vm.frame_count - 1].closure).function).chunk };
    let mut ip: *const u8 = unsafe { chunk.code.as_ptr().add(vm.frames[vm.frame_count - 1].ip) };
    let mut slots: *mut Value = vm.frames[vm.frame_count - 1].slots;

    loop {
        let current_offset = unsafe { ip.offset_from(chunk.code.as_ptr()) } as usize;

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
                disassemble_instruction(chunk, current_offset);
            }
        }

        let instruction = unsafe { *ip };
        ip = unsafe { ip.add(1) };

        match instruction {
            x if x == OpCode::Constant as u8 => {
                let constant = read_constant!(ip, chunk);
                vm.push(constant);
            }
            x if x == OpCode::ConstantLong as u8 => {
                let lo = read_byte!(ip) as usize;
                let mi = read_byte!(ip) as usize;
                let hi = read_byte!(ip) as usize;

                let index = lo | (mi << 8) | (hi << 16);

                let constant = chunk.constants[index];
                vm.push(constant);
            }
            x if x == OpCode::Nil as u8 => vm.push(Value::Nil),
            x if x == OpCode::True as u8 => vm.push(Value::Bool(true)),
            x if x == OpCode::False as u8 => vm.push(Value::Bool(false)),
            x if x == OpCode::GetUpvalue as u8 => {
                let slot = read_byte!(ip) as usize;
                let value = unsafe {
                    let upvalue = (*vm.frames[vm.frame_count - 1].closure).upvalues[slot];
                    *(*upvalue).location
                };
                vm.push(value);
            }
            x if x == OpCode::SetUpvalue as u8 => {
                let slot = read_byte!(ip) as usize;
                let value = vm.peek(0);
                unsafe {
                    let upvalue = (*vm.frames[vm.frame_count - 1].closure).upvalues[slot];
                    *(*upvalue).location = value;
                }
            }
            x if x == OpCode::GetProperty as u8 => {
                if !vm.peek(0).is_instance() {
                    vm.runtime_error("Only instances have properties.");
                    return InterpretResult::RuntimeError;
                }

                let instance_ptr = vm.peek(0).as_instance();
                let name = read_string!(ip, chunk);

                if let Some(value) = unsafe { (*instance_ptr).fields.get(name) } {
                    vm.pop();
                    vm.push(value);
                } else {
                    let class = unsafe { (*instance_ptr).class };
                    if !bind_method(vm, class, name) {
                        return InterpretResult::RuntimeError;
                    }
                }
            }
            x if x == OpCode::SetProperty as u8 => {
                if !vm.peek(1).is_instance() {
                    vm.runtime_error("Only instances have fields.");
                    return InterpretResult::RuntimeError;
                }

                let instance = unsafe { &mut *vm.peek(1).as_instance() };
                let name = read_string!(ip, chunk);
                let value = vm.peek(0);

                instance.fields.set(name, value);

                let value = vm.pop();
                vm.pop();
                vm.push(value);
            }
            x if x == OpCode::GetSuper as u8 => {
                let name = read_string!(ip, chunk);
                let superclass = vm.pop().as_class();

                if !bind_method(vm, superclass, name) {
                    return InterpretResult::RuntimeError;
                }
            }
            x if x == OpCode::Equal as u8 => {
                let b = vm.pop();
                let a = vm.pop();
                vm.push(Value::Bool(values_equal(a, b)));
            }
            x if x == OpCode::Greater as u8 => binary_op!(vm, ip, chunk, Value::Bool, >),
            x if x == OpCode::Less as u8 => binary_op!(vm, ip, chunk, Value::Bool, <),
            x if x == OpCode::Add as u8 => {
                if vm.peek(0).is_string() && vm.peek(1).is_string() {
                    vm.concatenate();
                } else if vm.peek(0).is_number() && vm.peek(1).is_number() {
                    let b = vm.pop().as_number();
                    let a = vm.pop().as_number();
                    vm.push(Value::Number(a + b));
                } else if vm.peek(0).is_list() && vm.peek(1).is_list() {
                    let b_items = unsafe { (*vm.peek(0).as_list()).items.clone() };
                    let a_items = unsafe { (*vm.peek(1).as_list()).items.clone() };
                    vm.pop();
                    vm.pop();
                    let mut combined = a_items;
                    combined.extend(b_items);
                    let result = allocate_list(vm, combined);
                    vm.push(Value::Obj(result as *mut Obj));
                } else {
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                    vm.runtime_error("Operands must be two numbers or two strings.");
                    return InterpretResult::RuntimeError;
                }
            }
            x if x == OpCode::Subtract as u8 => binary_op!(vm, ip, chunk, Value::Number, -),
            x if x == OpCode::Multiply as u8 => {
                if vm.peek(0).is_number() && vm.peek(1).is_list() {
                    let count = vm.pop().as_number() as usize;
                    let list_ptr = vm.pop().as_list();
                    let items = unsafe { (*list_ptr).items.clone() };
                    let mut repeated = Vec::with_capacity(items.len() * count);
                    for _ in 0..count {
                        repeated.extend_from_slice(&items);
                    }
                    let result = allocate_list(vm, repeated);
                    vm.push(Value::Obj(result as *mut Obj));
                } else {
                    binary_op!(vm, ip, chunk, Value::Number, *)
                }
            }
            x if x == OpCode::Divide as u8 => binary_op!(vm, ip, chunk, Value::Number, /),
            x if x == OpCode::IntDivide as u8 => {
                if !vm.peek(0).is_number() || !vm.peek(1).is_number() {
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
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
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
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
                let slot = read_byte!(ip) as usize;
                unsafe {
                    let value = *slots.add(slot);
                    vm.push(value);
                }
            }
            x if x == OpCode::SetLocal as u8 => {
                let slot = read_byte!(ip) as usize;
                unsafe {
                    *slots.add(slot) = vm.peek(0);
                }
            }
            x if x == OpCode::GetLocalLong as u8 => {
                let slot = read_short!(ip) as usize;
                unsafe {
                    let value = *slots.add(slot);
                    vm.push(value);
                }
            }
            x if x == OpCode::SetLocalLong as u8 => {
                let slot = read_short!(ip) as usize;
                unsafe {
                    *slots.add(slot) = vm.peek(0);
                }
            }
            x if x == OpCode::GetGlobal as u8 => {
                let name = read_string!(ip, chunk);

                if let Some(value) = vm.globals.get(name) {
                    vm.push(value);
                } else {
                    let name_str = ObjString::as_str(name);
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
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
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                    vm.runtime_error(&format!("Undefined variable '{}'.", name_str));
                    return InterpretResult::RuntimeError;
                }
            }
            x if x == OpCode::BuildList as u8 => {
                let item_count = read_byte!(ip) as usize;
                let mut items = Vec::with_capacity(item_count);

                for _ in 0..item_count {
                    items.push(vm.pop());
                }
                items.reverse();

                let list_ptr = crate::object::allocate_list(vm, items);
                vm.push(Value::Obj(list_ptr as *mut Obj));
            }
            x if x == OpCode::BuildListLong as u8 => {
                let item_count = read_short!(ip) as usize;
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
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                    vm.runtime_error("Only lists can be subscripted.");
                    return InterpretResult::RuntimeError;
                }
                if !index_val.is_number() {
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                    vm.runtime_error("List index must be a number.");
                    return InterpretResult::RuntimeError;
                }

                let list = unsafe { &*list_val.as_list() };
                let raw = index_val.as_number() as isize;
                let index = if raw < 0 {
                    (list.items.len() as isize + raw) as usize
                } else {
                    raw as usize
                };

                if index >= list.items.len() {
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
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
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                    vm.runtime_error("Only lists can be subscripted.");
                    return InterpretResult::RuntimeError;
                }
                if !index_val.is_number() {
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                    vm.runtime_error("List index must be a number.");
                    return InterpretResult::RuntimeError;
                }

                let list = unsafe { &mut *list_val.as_list() };
                let raw = index_val.as_number() as isize;
                let index = if raw < 0 {
                    (list.items.len() as isize + raw) as usize
                } else {
                    raw as usize
                };

                if index >= list.items.len() {
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
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
                let offset = read_short!(ip) as usize;
                unsafe {
                    ip = ip.add(offset);
                }
            }
            x if x == OpCode::JumpIfFalse as u8 => {
                let offset = read_short!(ip) as usize;
                if vm.peek(0).is_falsy() {
                    unsafe {
                        ip = ip.add(offset);
                    }
                }
            }
            x if x == OpCode::Loop as u8 => {
                let offset = read_short!(ip) as usize;
                unsafe {
                    ip = ip.sub(offset);
                }
            }
            x if x == OpCode::Call as u8 => {
                let arg_count = read_byte!(ip) as usize;

                // Synchronize raw pointer back to an integer offset inside the VM frame storage before switching context
                let final_offset = unsafe { ip.offset_from(chunk.code.as_ptr()) } as usize;
                vm.frames[vm.frame_count - 1].ip = final_offset;

                if !vm.call_value(vm.peek(arg_count), arg_count) {
                    return InterpretResult::RuntimeError;
                }

                // Swap out context boundaries to the newly initialized CallFrame
                chunk = unsafe { &(*(*vm.frames[vm.frame_count - 1].closure).function).chunk };
                ip = unsafe { chunk.code.as_ptr().add(vm.frames[vm.frame_count - 1].ip) };
                slots = vm.frames[vm.frame_count - 1].slots;
            }
            x if x == OpCode::Invoke as u8 => {
                let method = read_string!(ip, chunk);
                let arg_count = unsafe { *ip } as usize;
                ip = unsafe { ip.add(1) };

                let final_offset = unsafe { ip.offset_from(chunk.code.as_ptr()) } as usize;
                vm.frames[vm.frame_count - 1].ip = final_offset;

                let receiver = vm.peek(arg_count);
                if receiver.is_list() {
                    let list_ptr = receiver.as_list();
                    let name = unsafe { ObjString::as_str(method) };
                    if !call_list_method(vm, list_ptr, name, arg_count) {
                        return InterpretResult::RuntimeError;
                    }
                } else {
                    if !invoke(vm, method, arg_count) {
                        return InterpretResult::RuntimeError;
                    }

                    chunk = unsafe { &(*(*vm.frames[vm.frame_count - 1].closure).function).chunk };
                    ip = unsafe { chunk.code.as_ptr().add(vm.frames[vm.frame_count - 1].ip) };
                    slots = vm.frames[vm.frame_count - 1].slots;
                }
            }
            x if x == OpCode::SuperInvoke as u8 => {
                let method = read_string!(ip, chunk);
                let arg_count = read_byte!(ip) as usize;
                let superclass = vm.pop().as_class();

                let final_offset = unsafe { ip.offset_from(chunk.code.as_ptr()) } as usize;
                vm.frames[vm.frame_count - 1].ip = final_offset;

                if !invoke_from_class(vm, superclass, method, arg_count) {
                    return InterpretResult::RuntimeError;
                }

                chunk = unsafe { &(*(*vm.frames[vm.frame_count - 1].closure).function).chunk };
                ip = unsafe { chunk.code.as_ptr().add(vm.frames[vm.frame_count - 1].ip) };
                slots = vm.frames[vm.frame_count - 1].slots;
            }
            x if x == OpCode::Closure as u8 => {
                let function_val = read_constant!(ip, chunk);
                let function_ptr = function_val.as_function();
                let closure_ptr = allocate_closure(vm, function_ptr);

                let upvalue_count = unsafe { (*closure_ptr).upvalue_count };
                for i in 0..upvalue_count {
                    let is_local = read_byte!(ip);
                    let index = read_byte!(ip) as usize;

                    unsafe {
                        if is_local == 1 {
                            (*closure_ptr).upvalues[i] = capture_upvalue(vm, slots.add(index));
                        } else {
                            (*closure_ptr).upvalues[i] =
                                (*vm.frames[vm.frame_count - 1].closure).upvalues[index];
                        }
                    }
                }

                vm.push(Value::Obj(closure_ptr as *mut Obj));
            }
            x if x == OpCode::CloseUpvalue as u8 => {
                close_upvalues(vm, unsafe { vm.stack_top.sub(1) });
                vm.pop();
            }
            x if x == OpCode::Return as u8 => {
                let result = vm.pop();
                close_upvalues(vm, slots);
                vm.frame_count -= 1;

                if vm.frame_count == 0 {
                    vm.pop();
                    return InterpretResult::Ok;
                }

                let slots_to_restore = vm.frames[vm.frame_count].slots;
                vm.stack_top = slots_to_restore;
                vm.push(result);

                // Swap context elements to match the parent frame we returned back into
                chunk = unsafe { &(*(*vm.frames[vm.frame_count - 1].closure).function).chunk };
                ip = unsafe { chunk.code.as_ptr().add(vm.frames[vm.frame_count - 1].ip) };
                slots = vm.frames[vm.frame_count - 1].slots;
            }
            x if x == OpCode::Class as u8 => {
                let name = read_string!(ip, chunk);
                let class = allocate_class(vm, name);
                vm.push(Value::Obj(class as *mut Obj));
            }
            x if x == OpCode::Inherit as u8 => {
                let superclass = vm.peek(1);
                if !superclass.is_class() {
                    vm.runtime_error("Superclass must be a class.");
                    return InterpretResult::RuntimeError;
                }

                let subclass = unsafe { &mut *vm.peek(0).as_class() };
                let superclass_methods = unsafe { &(*superclass.as_class()).methods };
                subclass.methods.add_all(superclass_methods);
                vm.pop();
            }
            x if x == OpCode::Method as u8 => {
                let name = read_string!(ip, chunk);
                define_method(vm, name);
            }
            _ => {
                println!("Unknown opcode {}", instruction);
                return InterpretResult::RuntimeError;
            }
        }
    }
}
