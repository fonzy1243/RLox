use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunk::OpCode;
use crate::compiler::compile;
use crate::object::{
    NativeFn, Obj, ObjClosure, ObjFunction, ObjString, ObjType, ObjUpvalue, allocate_closure,
    allocate_list, allocate_native, allocate_string, capture_upvalue, close_upvalues, copy_string,
    free_object, take_string,
};
use crate::table::Table;
use crate::value::{Value, values_equal};
use crate::{
    Diagnostic, DiagnosticPhase, DiagnosticSeverity, RuntimeFrame, RuntimeHost, SourceDocument,
    SourceSpan,
};

#[cfg(feature = "debug_trace_execution")]
use crate::debug::disassemble_instruction;

const FRAMES_MAX: usize = 256;
const STACK_MAX: usize = FRAMES_MAX * 256;

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
    ($vm:expr, $host:expr, $fallback:expr, $ip:expr, $chunk:expr, $wrap:expr, $op:tt) => {{
        if !$vm.peek(0).is_number() || !$vm.peek(1).is_number() {
            let offset = unsafe { $ip.offset_from($chunk.code.as_ptr()) } as usize;
            $vm.frames[$vm.frame_count - 1].ip = offset;
            $vm.runtime_error($host, "Operands must be numbers.", Some(offset - 1), $fallback);
            return InterpretResult::RuntimeError;
        }
        let b = $vm.pop().as_number();
        unsafe {
            let top_ptr = $vm.stack_top.sub(1);
            *top_ptr = $wrap((*top_ptr).as_number() $op b);
        }
    }};
}

macro_rules! push_or_runtime_error {
    ($vm:expr, $host:expr, $fallback:expr, $value:expr, $offset:expr) => {{
        if !$vm.push($value) {
            $vm.frames[$vm.frame_count - 1].ip = $offset + 1;
            $vm.runtime_error($host, "Stack overflow.", Some($offset), $fallback);
            return InterpretResult::RuntimeError;
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        };

        vm.stack_top = vm.stack.as_mut_ptr();

        let native_defined = vm.define_native("clock", clock_native);
        debug_assert!(native_defined, "a new VM has capacity for native roots");
        vm
    }

    pub fn run(
        &mut self,
        document: &SourceDocument,
        host: &mut dyn RuntimeHost,
    ) -> InterpretResult {
        let fallback_span = document.eof_span();
        let function = match compile(document, self, host) {
            Some(func) => func,
            None => return InterpretResult::CompileError,
        };

        if !self.push(Value::Obj(function as *mut Obj)) {
            self.runtime_error(host, "Stack overflow.", None, fallback_span);
            return InterpretResult::RuntimeError;
        }
        let closure = allocate_closure(self, function);
        self.pop();
        if !self.push(Value::Obj(closure as *mut Obj)) {
            self.runtime_error(host, "Stack overflow.", None, fallback_span);
            return InterpretResult::RuntimeError;
        }

        if !self.call(closure, 0, host, fallback_span) {
            return InterpretResult::RuntimeError;
        }

        run(self, host, fallback_span)
    }

    #[must_use]
    fn push(&mut self, value: Value) -> bool {
        let stack_end = unsafe { self.stack.as_mut_ptr().add(STACK_MAX) };
        if self.stack_top >= stack_end {
            return false;
        }

        unsafe {
            *self.stack_top = value;
            self.stack_top = self.stack_top.add(1);
        }
        true
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

    fn call(
        &mut self,
        closure: *mut ObjClosure,
        arg_count: usize,
        host: &mut dyn RuntimeHost,
        fallback_span: SourceSpan,
    ) -> bool {
        let arity = unsafe { (*(*closure).function).arity };
        if arg_count != arity {
            self.runtime_error(
                host,
                &format!("Expected {} arguments but got {}.", arity, arg_count),
                None,
                fallback_span,
            );
            return false;
        }

        if self.frame_count == FRAMES_MAX {
            self.runtime_error(host, "Stack overflow.", None, fallback_span);
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

    fn call_value(
        &mut self,
        callee: Value,
        arg_count: usize,
        host: &mut dyn RuntimeHost,
        fallback_span: SourceSpan,
    ) -> bool {
        if callee.is_closure() {
            return self.call(callee.as_closure(), arg_count, host, fallback_span);
        } else if callee.is_native() {
            let native = callee.as_native();
            unsafe {
                let args_start = self.stack_top.sub(arg_count);
                let args_slice = std::slice::from_raw_parts(args_start, arg_count);
                let result = ((*native).function)(arg_count, args_slice);
                self.stack_top = args_start.sub(1);

                if !self.push(result) {
                    self.runtime_error(host, "Stack overflow.", None, fallback_span);
                    return false;
                }
                return true;
            }
        }

        self.runtime_error(
            host,
            "Can only call functions and classes.",
            None,
            fallback_span,
        );
        false
    }

    fn concatenate(&mut self) -> Value {
        let b_val = self.pop();
        let a_val = self.pop();

        let b = b_val.as_cstring();
        let a = a_val.as_cstring();

        let chars = format!("{}{}", a, b);

        let result = take_string(self, chars);

        Value::Obj(result as *mut Obj)
    }

    fn runtime_error(
        &mut self,
        host: &mut dyn RuntimeHost,
        message: &str,
        fault_offset: Option<usize>,
        fallback_span: SourceSpan,
    ) {
        let mut frames = Vec::with_capacity(self.frame_count);
        let mut primary_span = fallback_span;

        for (frame_index, i) in (0..self.frame_count).rev().enumerate() {
            let frame = &self.frames[i];
            let chunk = unsafe { &(*(*frame.closure).function).chunk };
            let instruction = if frame_index == 0 {
                fault_offset.unwrap_or_else(|| frame.ip.saturating_sub(1))
            } else {
                frame.ip.saturating_sub(1)
            };
            let span = chunk
                .spans
                .get(instruction)
                .copied()
                .unwrap_or(fallback_span);
            if frame_index == 0 {
                primary_span = span;
            }
            let function = if unsafe { (*(*frame.closure).function).name.is_null() } {
                "<script>".to_string()
            } else {
                unsafe { ObjString::as_str((*(*frame.closure).function).name) }.to_string()
            };
            frames.push(RuntimeFrame { function, span });
        }

        host.diagnostic(Diagnostic {
            phase: DiagnosticPhase::Runtime,
            severity: DiagnosticSeverity::Error,
            code: "runtime.error".to_string(),
            message: message.to_string(),
            span: primary_span,
            frames,
        });
        self.reset_execution_state();
    }

    fn reset_execution_state(&mut self) {
        let stack_start = self.stack.as_mut_ptr();
        unsafe {
            close_upvalues(self, stack_start);
        }
        self.open_upvalues = std::ptr::null_mut();
        self.frame_count = 0;
        self.stack_top = stack_start;
    }

    fn define_native(&mut self, name: &str, function: NativeFn) -> bool {
        let name_obj = copy_string(self, name);
        let native_obj = allocate_native(self, function);

        if !self.push(Value::Obj(name_obj as *mut Obj)) {
            return false;
        }
        if !self.push(Value::Obj(native_obj as *mut Obj)) {
            self.pop();
            return false;
        }

        self.globals.set(name_obj, self.stack[1]);

        self.pop();
        self.pop();
        true
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

// Garbage Collection
pub fn collect_garbage(vm: &mut VM) {
    #[cfg(feature = "debug_log_gc")]
    eprintln!("-- gc begin");

    #[cfg(feature = "debug_log_gc")]
    eprintln!("-- gc end");
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
        eprintln!("{:p} mark {}", object, unsafe { Value::Obj(object) });

        (*object).is_marked = true;
    }

    vm.gray_stack.push(object);
}

pub fn mark_value(vm: &mut VM, value: Value) {
    if let Value::Obj(ptr) = value {
        mark_object(vm, ptr);
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
}
// -----------------

fn checked_list_index(value: Value, len: usize) -> Result<usize, &'static str> {
    if !value.is_number() {
        return Err("List index must be a number.");
    }
    let number = value.as_number();
    if !number.is_finite() || number.fract() != 0.0 || number < 0.0 {
        return Err("List index must be a non-negative integer.");
    }
    let index = number as usize;
    if index >= len {
        return Err("List index out of bounds.");
    }
    Ok(index)
}

fn run(vm: &mut VM, host: &mut dyn RuntimeHost, fallback_span: SourceSpan) -> InterpretResult {
    let mut chunk = unsafe { &(*(*vm.frames[vm.frame_count - 1].closure).function).chunk };
    let mut ip: *const u8 = unsafe { chunk.code.as_ptr().add(vm.frames[vm.frame_count - 1].ip) };
    let mut slots: *mut Value = vm.frames[vm.frame_count - 1].slots;

    loop {
        let current_offset = unsafe { ip.offset_from(chunk.code.as_ptr()) } as usize;

        #[cfg(feature = "debug_trace_execution")]
        {
            eprint!("          ");
            unsafe {
                let stack_len = vm.stack_top.offset_from(vm.stack.as_ptr()) as usize;
                let stack_slice = std::slice::from_raw_parts(vm.stack.as_ptr(), stack_len);

                for value in stack_slice {
                    eprint!("[ {} ]", value);
                }
            }
            eprintln!();

            unsafe {
                disassemble_instruction(chunk, current_offset);
            }
        }

        let instruction = unsafe { *ip };
        ip = unsafe { ip.add(1) };

        match instruction {
            x if x == OpCode::Constant as u8 => {
                let constant = read_constant!(ip, chunk);
                push_or_runtime_error!(vm, host, fallback_span, constant, current_offset);
            }
            x if x == OpCode::ConstantLong as u8 => {
                let lo = read_byte!(ip) as usize;
                let mi = read_byte!(ip) as usize;
                let hi = read_byte!(ip) as usize;

                let index = lo | (mi << 8) | (hi << 16);

                let constant = chunk.constants[index];
                push_or_runtime_error!(vm, host, fallback_span, constant, current_offset);
            }
            x if x == OpCode::Nil as u8 => {
                push_or_runtime_error!(vm, host, fallback_span, Value::Nil, current_offset)
            }
            x if x == OpCode::True as u8 => {
                push_or_runtime_error!(vm, host, fallback_span, Value::Bool(true), current_offset)
            }
            x if x == OpCode::False as u8 => {
                push_or_runtime_error!(vm, host, fallback_span, Value::Bool(false), current_offset)
            }
            x if x == OpCode::GetUpvalue as u8 => {
                let slot = read_byte!(ip) as usize;
                let value = unsafe {
                    let upvalue = (*vm.frames[vm.frame_count - 1].closure).upvalues[slot];
                    *(*upvalue).location
                };
                push_or_runtime_error!(vm, host, fallback_span, value, current_offset);
            }
            x if x == OpCode::SetUpvalue as u8 => {
                let slot = read_byte!(ip) as usize;
                let value = vm.peek(0);
                unsafe {
                    let upvalue = (*vm.frames[vm.frame_count - 1].closure).upvalues[slot];
                    *(*upvalue).location = value;
                }
            }
            x if x == OpCode::Equal as u8 => {
                let b = vm.pop();
                let a = vm.pop();
                push_or_runtime_error!(
                    vm,
                    host,
                    fallback_span,
                    Value::Bool(values_equal(a, b)),
                    current_offset
                );
            }
            x if x == OpCode::Greater as u8 => {
                binary_op!(vm, host, fallback_span, ip, chunk, Value::Bool, >)
            }
            x if x == OpCode::Less as u8 => {
                binary_op!(vm, host, fallback_span, ip, chunk, Value::Bool, <)
            }
            x if x == OpCode::Add as u8 => {
                if vm.peek(0).is_string() && vm.peek(1).is_string() {
                    let result = vm.concatenate();
                    push_or_runtime_error!(vm, host, fallback_span, result, current_offset);
                } else if vm.peek(0).is_number() && vm.peek(1).is_number() {
                    let b = vm.pop().as_number();
                    let a = vm.pop().as_number();
                    push_or_runtime_error!(
                        vm,
                        host,
                        fallback_span,
                        Value::Number(a + b),
                        current_offset
                    );
                } else {
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                    vm.runtime_error(
                        host,
                        "Operands must be two numbers or two strings.",
                        Some(current_offset),
                        fallback_span,
                    );
                    return InterpretResult::RuntimeError;
                }
            }
            x if x == OpCode::Subtract as u8 => {
                binary_op!(vm, host, fallback_span, ip, chunk, Value::Number, -)
            }
            x if x == OpCode::Multiply as u8 => {
                binary_op!(vm, host, fallback_span, ip, chunk, Value::Number, *)
            }
            x if x == OpCode::Divide as u8 => {
                binary_op!(vm, host, fallback_span, ip, chunk, Value::Number, /)
            }
            x if x == OpCode::IntDivide as u8 => {
                if !vm.peek(0).is_number() || !vm.peek(1).is_number() {
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                    vm.runtime_error(
                        host,
                        "Operands must be numbers.",
                        Some(current_offset),
                        fallback_span,
                    );
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
                    vm.runtime_error(
                        host,
                        "Operand must be a number.",
                        Some(current_offset),
                        fallback_span,
                    );
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
                push_or_runtime_error!(vm, host, fallback_span, value, current_offset);
            }
            x if x == OpCode::GetLocal as u8 => {
                let slot = read_byte!(ip) as usize;
                let value = unsafe { *slots.add(slot) };
                push_or_runtime_error!(vm, host, fallback_span, value, current_offset);
            }
            x if x == OpCode::SetLocal as u8 => {
                let slot = read_byte!(ip) as usize;
                unsafe {
                    *slots.add(slot) = vm.peek(0);
                }
            }
            x if x == OpCode::GetLocalLong as u8 => {
                let slot = read_short!(ip) as usize;
                let value = unsafe { *slots.add(slot) };
                push_or_runtime_error!(vm, host, fallback_span, value, current_offset);
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
                    push_or_runtime_error!(vm, host, fallback_span, value, current_offset);
                } else {
                    let name_str = ObjString::as_str(name);
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                    vm.runtime_error(
                        host,
                        &format!("Undefined variable '{}'.", name_str),
                        Some(current_offset),
                        fallback_span,
                    );
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
                    vm.runtime_error(
                        host,
                        &format!("Undefined variable '{}'.", name_str),
                        Some(current_offset),
                        fallback_span,
                    );
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
                push_or_runtime_error!(
                    vm,
                    host,
                    fallback_span,
                    Value::Obj(list_ptr as *mut Obj),
                    current_offset
                );
            }
            x if x == OpCode::BuildListLong as u8 => {
                let item_count = read_short!(ip) as usize;
                let mut items = Vec::with_capacity(item_count);

                for _ in 0..item_count {
                    items.push(vm.pop());
                }
                items.reverse();

                let list_ptr = allocate_list(vm, items);
                push_or_runtime_error!(
                    vm,
                    host,
                    fallback_span,
                    Value::Obj(list_ptr as *mut Obj),
                    current_offset
                );
            }
            x if x == OpCode::GetIndex as u8 => {
                let index_val = vm.pop();
                let list_val = vm.pop();

                if !list_val.is_list() {
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                    vm.runtime_error(
                        host,
                        "Only lists can be subscripted.",
                        Some(current_offset),
                        fallback_span,
                    );
                    return InterpretResult::RuntimeError;
                }
                let list = unsafe { &*list_val.as_list() };
                let index = match checked_list_index(index_val, list.items.len()) {
                    Ok(index) => index,
                    Err(message) => {
                        vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                        vm.runtime_error(host, message, Some(current_offset), fallback_span);
                        return InterpretResult::RuntimeError;
                    }
                };

                push_or_runtime_error!(vm, host, fallback_span, list.items[index], current_offset);
            }
            x if x == OpCode::SetIndex as u8 => {
                let value = vm.pop();
                let index_val = vm.pop();
                let list_val = vm.pop();

                if !list_val.is_list() {
                    vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                    vm.runtime_error(
                        host,
                        "Only lists can be subscripted.",
                        Some(current_offset),
                        fallback_span,
                    );
                    return InterpretResult::RuntimeError;
                }
                let list = unsafe { &mut *list_val.as_list() };
                let index = match checked_list_index(index_val, list.items.len()) {
                    Ok(index) => index,
                    Err(message) => {
                        vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                        vm.runtime_error(host, message, Some(current_offset), fallback_span);
                        return InterpretResult::RuntimeError;
                    }
                };

                list.items[index] = value;

                push_or_runtime_error!(vm, host, fallback_span, value, current_offset);
            }
            x if x == OpCode::Print as u8 => {
                host.output(vm.pop().to_string());
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

                if !vm.call_value(vm.peek(arg_count), arg_count, host, fallback_span) {
                    return InterpretResult::RuntimeError;
                }

                // Swap out context boundaries to the newly initialized CallFrame
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

                push_or_runtime_error!(
                    vm,
                    host,
                    fallback_span,
                    Value::Obj(closure_ptr as *mut Obj),
                    current_offset
                );
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
                if !vm.push(result) {
                    vm.runtime_error(host, "Stack overflow.", Some(current_offset), fallback_span);
                    return InterpretResult::RuntimeError;
                }

                // Swap context elements to match the parent frame we returned back into
                chunk = unsafe { &(*(*vm.frames[vm.frame_count - 1].closure).function).chunk };
                ip = unsafe { chunk.code.as_ptr().add(vm.frames[vm.frame_count - 1].ip) };
                slots = vm.frames[vm.frame_count - 1].slots;
            }
            _ => {
                vm.frames[vm.frame_count - 1].ip = current_offset + 1;
                vm.runtime_error(
                    host,
                    &format!("Unknown opcode {}.", instruction),
                    Some(current_offset),
                    fallback_span,
                );
                return InterpretResult::RuntimeError;
            }
        }
    }
}
