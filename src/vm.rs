use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunk::OpCode;
use crate::compiler::compile;
use crate::object::{
    NativeFn, Obj, ObjClosure, ObjFunction, ObjString, ObjUpvalue, allocate_closure, allocate_list,
    allocate_native, capture_upvalue, close_upvalues, copy_string, free_object, take_string,
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

#[derive(Clone, Copy)]
pub struct CallFrame {
    pub closure: *mut ObjClosure,
    pub ip: usize,
    pub slots: *mut Value,
    pub activation_id: crate::session::ActivationId,
    pub call_site_offset: Option<usize>,
    pub call_site: Option<SourceSpan>,
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
    next_activation_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VmFault {
    pub message: String,
    pub offset: usize,
}

impl VmFault {
    pub(crate) fn new(message: impl Into<String>, offset: usize) -> Self {
        Self {
            message: message.into(),
            offset,
        }
    }
}

pub(crate) enum StartError {
    Compile,
    Runtime(VmFault),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DispatchResult {
    Continue,
    Complete,
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
            activation_id: crate::session::ActivationId(0),
            call_site_offset: None,
            call_site: None,
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
            next_activation_id: 1,
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
        match self.prepare(document, host) {
            Ok(()) => {}
            Err(StartError::Compile) => return InterpretResult::CompileError,
            Err(StartError::Runtime(fault)) => {
                let diagnostic = self.diagnostic_for_fault(fault, document.eof_span());
                host.diagnostic(diagnostic);
                self.cleanup_execution();
                return InterpretResult::RuntimeError;
            }
        }

        loop {
            match self.dispatch_one(host) {
                Ok(DispatchResult::Continue) => {}
                Ok(DispatchResult::Complete) => return InterpretResult::Ok,
                Err(fault) => {
                    let diagnostic = self.diagnostic_for_fault(fault, document.eof_span());
                    host.diagnostic(diagnostic);
                    self.cleanup_execution();
                    return InterpretResult::RuntimeError;
                }
            }
        }
    }

    pub(crate) fn prepare(
        &mut self,
        document: &SourceDocument,
        host: &mut dyn RuntimeHost,
    ) -> Result<(), StartError> {
        let function = compile(document, self, host).ok_or(StartError::Compile)?;
        self.push(Value::Obj(function as *mut Obj))
            .then_some(())
            .ok_or_else(|| StartError::Runtime(VmFault::new("Stack overflow.", 0)))?;
        let closure = allocate_closure(self, function);
        self.pop();
        self.push(Value::Obj(closure as *mut Obj))
            .then_some(())
            .ok_or_else(|| StartError::Runtime(VmFault::new("Stack overflow.", 0)))?;
        self.install_frame(closure, 0, None)
            .map_err(StartError::Runtime)
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

    fn install_frame(
        &mut self,
        closure: *mut ObjClosure,
        arg_count: usize,
        call_site: Option<(usize, SourceSpan)>,
    ) -> Result<(), VmFault> {
        let arity = unsafe { (*(*closure).function).arity };
        if arg_count != arity {
            return Err(VmFault::new(
                format!("Expected {} arguments but got {}.", arity, arg_count),
                self.current_offset().unwrap_or(0),
            ));
        }

        if self.frame_count == FRAMES_MAX {
            return Err(VmFault::new(
                "Stack overflow.",
                self.current_offset().unwrap_or(0),
            ));
        }

        let activation_id = crate::session::ActivationId(self.next_activation_id);
        self.next_activation_id = self.next_activation_id.checked_add(1).ok_or_else(|| {
            VmFault::new(
                "Activation counter exhausted.",
                self.current_offset().unwrap_or(0),
            )
        })?;

        self.frames[self.frame_count] = CallFrame {
            closure,
            ip: 0,
            slots: unsafe { self.stack_top.sub(arg_count + 1) },
            activation_id,
            call_site_offset: call_site.map(|(offset, _)| offset),
            call_site: call_site.map(|(_, span)| span),
        };

        self.frame_count += 1;
        Ok(())
    }

    pub(crate) fn diagnostic_for_fault(
        &self,
        fault: VmFault,
        fallback_span: SourceSpan,
    ) -> Diagnostic {
        let mut frames = Vec::with_capacity(self.frame_count);
        let mut primary_span = fallback_span;

        for i in (0..self.frame_count).rev() {
            let frame = &self.frames[i];
            let chunk = unsafe { &(*(*frame.closure).function).chunk };
            let span = if i == self.frame_count - 1 {
                chunk
                    .spans
                    .get(fault.offset)
                    .copied()
                    .unwrap_or(fallback_span)
            } else {
                let child = &self.frames[i + 1];
                child.call_site.unwrap_or_else(|| {
                    child
                        .call_site_offset
                        .and_then(|offset| chunk.spans.get(offset).copied())
                        .unwrap_or(fallback_span)
                })
            };
            if i == self.frame_count - 1 {
                primary_span = span;
            }
            let function = if unsafe { (*(*frame.closure).function).name.is_null() } {
                "<script>".to_string()
            } else {
                unsafe { ObjString::as_str((*(*frame.closure).function).name) }.to_string()
            };
            frames.push(RuntimeFrame { function, span });
        }

        Diagnostic {
            phase: DiagnosticPhase::Runtime,
            severity: DiagnosticSeverity::Error,
            code: "runtime.error".to_string(),
            message: fault.message,
            span: primary_span,
            frames,
        }
    }

    pub(crate) fn cleanup_execution(&mut self) {
        let stack_start = self.stack.as_mut_ptr();
        close_upvalues(self, stack_start);
        self.open_upvalues = std::ptr::null_mut();
        self.frame_count = 0;
        self.stack_top = stack_start;
    }

    pub(crate) fn current_offset(&self) -> Option<usize> {
        (self.frame_count > 0).then(|| self.frames[self.frame_count - 1].ip)
    }

    pub(crate) fn activation_chain(&self) -> Vec<crate::session::ActivationId> {
        self.frames[..self.frame_count]
            .iter()
            .map(|frame| frame.activation_id)
            .collect()
    }

    pub(crate) fn contains_activation(&self, activation_id: crate::session::ActivationId) -> bool {
        self.frames[..self.frame_count]
            .iter()
            .any(|frame| frame.activation_id == activation_id)
    }

    pub(crate) fn current_semantic_point(
        &self,
    ) -> Option<(crate::session::ActivationId, crate::DebugPoint)> {
        if self.frame_count == 0 {
            return None;
        }
        let frame = self.frames[self.frame_count - 1];
        let info = unsafe { &(*(*frame.closure).function).debug_info };
        crate::session::semantic_point(&info.points, frame.ip)
            .map(|point| (frame.activation_id, point))
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
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64());
    Value::Number(seconds)
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

impl VM {
    pub(crate) fn dispatch_one(
        &mut self,
        host: &mut dyn RuntimeHost,
    ) -> Result<DispatchResult, VmFault> {
        if self.frame_count == 0 {
            return Ok(DispatchResult::Complete);
        }

        let frame_index = self.frame_count - 1;
        let current_offset = self.frames[frame_index].ip;
        let function = unsafe { (*self.frames[frame_index].closure).function };
        let width = instruction_width_at(function, current_offset)
            .map_err(|message| VmFault::new(message, current_offset))?;
        let next_offset = current_offset
            .checked_add(width)
            .ok_or_else(|| VmFault::new("Instruction offset overflow.", current_offset))?;
        let instruction = unsafe { (&(*function).chunk.code)[current_offset] };
        let span = unsafe {
            (&(*function).chunk.spans)
                .get(current_offset)
                .copied()
                .unwrap_or((*function).debug_info.declaration)
        };

        #[cfg(feature = "debug_trace_execution")]
        {
            eprint!("          ");
            let stack_len = self.stack_len();
            for value in &self.stack[..stack_len] {
                eprint!("[ {} ]", value);
            }
            eprintln!();
            disassemble_instruction(unsafe { &(*function).chunk }, current_offset);
        }

        self.frames[frame_index].ip = next_offset;

        match instruction {
            x if x == OpCode::Constant as u8 => {
                let index = self.code_byte(function, current_offset + 1, current_offset)? as usize;
                let value = self.constant(function, index, current_offset)?;
                self.push_checked(value, current_offset)?;
            }
            x if x == OpCode::ConstantLong as u8 => {
                let index = self.code_u24(function, current_offset + 1, current_offset)?;
                let value = self.constant(function, index, current_offset)?;
                self.push_checked(value, current_offset)?;
            }
            x if x == OpCode::Nil as u8 => self.push_checked(Value::Nil, current_offset)?,
            x if x == OpCode::True as u8 => self.push_checked(Value::Bool(true), current_offset)?,
            x if x == OpCode::False as u8 => {
                self.push_checked(Value::Bool(false), current_offset)?
            }
            x if x == OpCode::Pop as u8 => {
                self.require_stack(1, current_offset)?;
                self.pop();
            }
            x if x == OpCode::Dup as u8 => {
                let value = self.peek_checked(0, current_offset)?;
                self.push_checked(value, current_offset)?;
            }
            x if x == OpCode::GetLocal as u8 || x == OpCode::GetLocalLong as u8 => {
                let slot = if instruction == OpCode::GetLocal as u8 {
                    self.code_byte(function, current_offset + 1, current_offset)? as usize
                } else {
                    self.code_u16(function, current_offset + 1, current_offset)? as usize
                };
                let value = self.local_value(frame_index, slot, current_offset)?;
                self.push_checked(value, current_offset)?;
            }
            x if x == OpCode::SetLocal as u8 || x == OpCode::SetLocalLong as u8 => {
                let slot = if instruction == OpCode::SetLocal as u8 {
                    self.code_byte(function, current_offset + 1, current_offset)? as usize
                } else {
                    self.code_u16(function, current_offset + 1, current_offset)? as usize
                };
                let value = self.peek_checked(0, current_offset)?;
                let index = self.local_index(frame_index, slot, current_offset)?;
                self.stack[index] = value;
            }
            x if x == OpCode::GetGlobal as u8
                || x == OpCode::DefineGlobal as u8
                || x == OpCode::SetGlobal as u8 =>
            {
                let index = self.code_byte(function, current_offset + 1, current_offset)? as usize;
                let name = self.string_constant(function, index, current_offset)?;
                if instruction == OpCode::GetGlobal as u8 {
                    let value = self.globals.get(name).ok_or_else(|| {
                        VmFault::new(
                            format!("Undefined variable '{}'.", ObjString::as_str(name)),
                            current_offset,
                        )
                    })?;
                    self.push_checked(value, current_offset)?;
                } else if instruction == OpCode::DefineGlobal as u8 {
                    let value = self.peek_checked(0, current_offset)?;
                    self.globals.set(name, value);
                    self.pop();
                } else {
                    let value = self.peek_checked(0, current_offset)?;
                    if self.globals.set(name, value) {
                        self.globals.delete(name);
                        return Err(VmFault::new(
                            format!("Undefined variable '{}'.", ObjString::as_str(name)),
                            current_offset,
                        ));
                    }
                }
            }
            x if x == OpCode::GetUpvalue as u8 || x == OpCode::SetUpvalue as u8 => {
                let slot = self.code_byte(function, current_offset + 1, current_offset)? as usize;
                let closure = self.frames[frame_index].closure;
                let upvalue = unsafe { (&(*closure).upvalues).get(slot).copied() }
                    .filter(|upvalue| !upvalue.is_null())
                    .ok_or_else(|| VmFault::new("Invalid upvalue.", current_offset))?;
                if instruction == OpCode::GetUpvalue as u8 {
                    let value = unsafe { *(*upvalue).location };
                    self.push_checked(value, current_offset)?;
                } else {
                    let value = self.peek_checked(0, current_offset)?;
                    unsafe {
                        *(*upvalue).location = value;
                    }
                }
            }
            x if x == OpCode::BuildList as u8 || x == OpCode::BuildListLong as u8 => {
                let count = if instruction == OpCode::BuildList as u8 {
                    self.code_byte(function, current_offset + 1, current_offset)? as usize
                } else {
                    self.code_u16(function, current_offset + 1, current_offset)? as usize
                };
                self.require_stack(count, current_offset)?;
                let len = self.stack_len();
                let start = len - count;
                let items = self.stack[start..len].to_vec();
                let list = allocate_list(self, items);
                self.stack_top = unsafe { self.stack.as_mut_ptr().add(start) };
                self.push_checked(Value::Obj(list as *mut Obj), current_offset)?;
            }
            x if x == OpCode::GetIndex as u8 => {
                self.require_stack(2, current_offset)?;
                let index_value = self.peek_checked(0, current_offset)?;
                let list_value = self.peek_checked(1, current_offset)?;
                if !list_value.is_list() {
                    return Err(VmFault::new(
                        "Only lists can be subscripted.",
                        current_offset,
                    ));
                }
                let list = unsafe { &*list_value.as_list() };
                let index = checked_list_index(index_value, list.items.len())
                    .map_err(|message| VmFault::new(message, current_offset))?;
                let value = list.items[index];
                self.pop();
                self.pop();
                self.push_checked(value, current_offset)?;
            }
            x if x == OpCode::SetIndex as u8 => {
                self.require_stack(3, current_offset)?;
                let value = self.peek_checked(0, current_offset)?;
                let index_value = self.peek_checked(1, current_offset)?;
                let list_value = self.peek_checked(2, current_offset)?;
                if !list_value.is_list() {
                    return Err(VmFault::new(
                        "Only lists can be subscripted.",
                        current_offset,
                    ));
                }
                let list = unsafe { &mut *list_value.as_list() };
                let index = checked_list_index(index_value, list.items.len())
                    .map_err(|message| VmFault::new(message, current_offset))?;
                list.items[index] = value;
                self.pop();
                self.pop();
                self.pop();
                self.push_checked(value, current_offset)?;
            }
            x if x == OpCode::Equal as u8 => {
                let (a, b) = self.binary_values(current_offset)?;
                self.replace_binary(Value::Bool(values_equal(a, b)));
            }
            x if x == OpCode::Greater as u8 || x == OpCode::Less as u8 => {
                let (a, b) = self.number_values(current_offset)?;
                self.replace_binary(Value::Bool(if instruction == OpCode::Greater as u8 {
                    a > b
                } else {
                    a < b
                }));
            }
            x if x == OpCode::Add as u8 => {
                let (a, b) = self.binary_values(current_offset)?;
                if a.is_number() && b.is_number() {
                    self.replace_binary(Value::Number(a.as_number() + b.as_number()));
                } else if a.is_string() && b.is_string() {
                    let chars = format!("{}{}", a.as_cstring(), b.as_cstring());
                    let result = take_string(self, chars);
                    self.pop();
                    self.pop();
                    self.push_checked(Value::Obj(result as *mut Obj), current_offset)?;
                } else {
                    return Err(VmFault::new(
                        "Operands must be two numbers or two strings.",
                        current_offset,
                    ));
                }
            }
            x if x == OpCode::Subtract as u8
                || x == OpCode::Multiply as u8
                || x == OpCode::Divide as u8
                || x == OpCode::IntDivide as u8 =>
            {
                let (a, b) = self.number_values(current_offset)?;
                let value = if instruction == OpCode::Subtract as u8 {
                    a - b
                } else if instruction == OpCode::Multiply as u8 {
                    a * b
                } else if instruction == OpCode::Divide as u8 {
                    a / b
                } else {
                    (a / b).trunc()
                };
                self.replace_binary(Value::Number(value));
            }
            x if x == OpCode::Not as u8 => {
                let value = self.peek_checked(0, current_offset)?;
                let index = self.stack_len() - 1;
                self.stack[index] = Value::Bool(value.is_falsy());
            }
            x if x == OpCode::Negate as u8 => {
                let value = self.peek_checked(0, current_offset)?;
                if !value.is_number() {
                    return Err(VmFault::new("Operand must be a number.", current_offset));
                }
                let index = self.stack_len() - 1;
                self.stack[index] = Value::Number(-value.as_number());
            }
            x if x == OpCode::Print as u8 => {
                let output = self.peek_checked(0, current_offset)?.to_string();
                self.pop();
                host.output(output);
            }
            x if x == OpCode::Jump as u8
                || x == OpCode::JumpIfFalse as u8
                || x == OpCode::Loop as u8 =>
            {
                let distance =
                    self.code_u16(function, current_offset + 1, current_offset)? as usize;
                let target = if instruction == OpCode::Loop as u8 {
                    next_offset.checked_sub(distance)
                } else {
                    next_offset.checked_add(distance)
                }
                .ok_or_else(|| VmFault::new("Invalid jump target.", current_offset))?;
                let selected = if instruction == OpCode::JumpIfFalse as u8 {
                    if self.peek_checked(0, current_offset)?.is_falsy() {
                        target
                    } else {
                        next_offset
                    }
                } else {
                    target
                };
                if !is_opcode_start(function, selected) {
                    return Err(VmFault::new("Invalid jump target.", current_offset));
                }
                self.frames[frame_index].ip = selected;
            }
            x if x == OpCode::Call as u8 => {
                let arg_count =
                    self.code_byte(function, current_offset + 1, current_offset)? as usize;
                self.require_stack(arg_count + 1, current_offset)?;
                let callee = self.peek_checked(arg_count, current_offset)?;
                if callee.is_closure() {
                    self.install_frame(
                        callee.as_closure(),
                        arg_count,
                        Some((current_offset, span)),
                    )
                    .map_err(|mut fault| {
                        fault.offset = current_offset;
                        fault
                    })?;
                } else if callee.is_native() {
                    let native = callee.as_native();
                    let len = self.stack_len();
                    let args = self.stack[len - arg_count..len].to_vec();
                    let result = unsafe { ((*native).function)(arg_count, &args) };
                    let callee_index = len - arg_count - 1;
                    self.stack_top = unsafe { self.stack.as_mut_ptr().add(callee_index) };
                    self.push_checked(result, current_offset)?;
                } else {
                    return Err(VmFault::new(
                        "Can only call functions and classes.",
                        current_offset,
                    ));
                }
            }
            x if x == OpCode::Closure as u8 => {
                let constant_index =
                    self.code_byte(function, current_offset + 1, current_offset)? as usize;
                let function_value = self.constant(function, constant_index, current_offset)?;
                if !function_value.is_function() {
                    return Err(VmFault::new("Invalid closure function.", current_offset));
                }
                let child_function = function_value.as_function();
                let count = unsafe { (*child_function).upvalue_count };
                let mut descriptors = Vec::with_capacity(count);
                for index in 0..count {
                    let descriptor_offset = current_offset + 2 + index * 2;
                    let is_local = self.code_byte(function, descriptor_offset, current_offset)?;
                    let capture =
                        self.code_byte(function, descriptor_offset + 1, current_offset)? as usize;
                    if is_local > 1 {
                        return Err(VmFault::new(
                            "Invalid closure capture descriptor.",
                            current_offset,
                        ));
                    }
                    if is_local == 1 {
                        self.local_index(frame_index, capture, current_offset)?;
                    } else {
                        let parent = self.frames[frame_index].closure;
                        let valid = unsafe {
                            (&(*parent).upvalues)
                                .get(capture)
                                .copied()
                                .is_some_and(|upvalue| !upvalue.is_null())
                        };
                        if !valid {
                            return Err(VmFault::new("Invalid closure upvalue.", current_offset));
                        }
                    }
                    descriptors.push((is_local == 1, capture));
                }

                let closure = allocate_closure(self, child_function);
                self.push_checked(Value::Obj(closure as *mut Obj), current_offset)?;
                for (index, (is_local, capture)) in descriptors.into_iter().enumerate() {
                    let upvalue = if is_local {
                        let stack_index = self.local_index(frame_index, capture, current_offset)?;
                        let location = unsafe { self.stack.as_mut_ptr().add(stack_index) };
                        capture_upvalue(self, location)
                    } else {
                        unsafe { (*self.frames[frame_index].closure).upvalues[capture] }
                    };
                    unsafe {
                        (*closure).upvalues[index] = upvalue;
                    }
                }
            }
            x if x == OpCode::CloseUpvalue as u8 => {
                self.require_stack(1, current_offset)?;
                let location = unsafe { self.stack_top.sub(1) };
                close_upvalues(self, location);
                self.pop();
            }
            x if x == OpCode::Return as u8 => {
                self.require_stack(1, current_offset)?;
                let result = self.peek_checked(0, current_offset)?;
                let slots = self.frames[frame_index].slots;
                close_upvalues(self, slots);
                self.frame_count -= 1;

                if self.frame_count == 0 {
                    self.stack_top = self.stack.as_mut_ptr();
                    return Ok(DispatchResult::Complete);
                }

                self.stack_top = slots;
                self.push_checked(result, current_offset)?;
            }
            _ => {
                return Err(VmFault::new(
                    format!("Unknown opcode {}.", instruction),
                    current_offset,
                ));
            }
        }

        Ok(DispatchResult::Continue)
    }

    fn stack_len(&self) -> usize {
        unsafe { self.stack_top.offset_from(self.stack.as_ptr()) as usize }
    }

    fn require_stack(&self, count: usize, offset: usize) -> Result<(), VmFault> {
        let protected = if self.frame_count == 0 {
            0
        } else {
            let frame = &self.frames[self.frame_count - 1];
            let base = unsafe { frame.slots.offset_from(self.stack.as_ptr()) as usize };
            base.checked_add(1)
                .ok_or_else(|| VmFault::new("Invalid frame stack.", offset))?
        };
        let required = protected
            .checked_add(count)
            .ok_or_else(|| VmFault::new("Stack size overflow.", offset))?;
        if self.stack_len() < required {
            Err(VmFault::new("Stack underflow.", offset))
        } else {
            Ok(())
        }
    }

    fn peek_checked(&self, distance: usize, offset: usize) -> Result<Value, VmFault> {
        self.require_stack(distance + 1, offset)?;
        Ok(self.stack[self.stack_len() - distance - 1])
    }

    fn push_checked(&mut self, value: Value, offset: usize) -> Result<(), VmFault> {
        if self.push(value) {
            Ok(())
        } else {
            Err(VmFault::new("Stack overflow.", offset))
        }
    }

    fn code_byte(
        &self,
        function: *mut ObjFunction,
        index: usize,
        offset: usize,
    ) -> Result<u8, VmFault> {
        unsafe {
            (&(*function).chunk.code)
                .get(index)
                .copied()
                .ok_or_else(|| VmFault::new("Truncated instruction.", offset))
        }
    }

    fn code_u16(
        &self,
        function: *mut ObjFunction,
        index: usize,
        offset: usize,
    ) -> Result<u16, VmFault> {
        let lo = self.code_byte(function, index, offset)?;
        let hi = self.code_byte(function, index + 1, offset)?;
        Ok(u16::from_le_bytes([lo, hi]))
    }

    fn code_u24(
        &self,
        function: *mut ObjFunction,
        index: usize,
        offset: usize,
    ) -> Result<usize, VmFault> {
        let lo = self.code_byte(function, index, offset)? as usize;
        let mid = self.code_byte(function, index + 1, offset)? as usize;
        let hi = self.code_byte(function, index + 2, offset)? as usize;
        Ok(lo | (mid << 8) | (hi << 16))
    }

    fn constant(
        &self,
        function: *mut ObjFunction,
        index: usize,
        offset: usize,
    ) -> Result<Value, VmFault> {
        unsafe {
            (&(*function).chunk.constants)
                .get(index)
                .copied()
                .ok_or_else(|| VmFault::new("Invalid constant index.", offset))
        }
    }

    fn string_constant(
        &self,
        function: *mut ObjFunction,
        index: usize,
        offset: usize,
    ) -> Result<*mut ObjString, VmFault> {
        let value = self.constant(function, index, offset)?;
        if !value.is_string() {
            return Err(VmFault::new("Invalid string constant.", offset));
        }
        Ok(value.as_obj() as *mut ObjString)
    }

    fn local_index(
        &self,
        frame_index: usize,
        slot: usize,
        offset: usize,
    ) -> Result<usize, VmFault> {
        let base = unsafe {
            self.frames[frame_index]
                .slots
                .offset_from(self.stack.as_ptr()) as usize
        };
        let index = base
            .checked_add(slot)
            .ok_or_else(|| VmFault::new("Invalid local slot.", offset))?;
        if index >= self.stack_len() {
            return Err(VmFault::new("Invalid local slot.", offset));
        }
        Ok(index)
    }

    fn local_value(
        &self,
        frame_index: usize,
        slot: usize,
        offset: usize,
    ) -> Result<Value, VmFault> {
        Ok(self.stack[self.local_index(frame_index, slot, offset)?])
    }

    fn binary_values(&self, offset: usize) -> Result<(Value, Value), VmFault> {
        self.require_stack(2, offset)?;
        Ok((
            self.stack[self.stack_len() - 2],
            self.stack[self.stack_len() - 1],
        ))
    }

    fn number_values(&self, offset: usize) -> Result<(f64, f64), VmFault> {
        let (a, b) = self.binary_values(offset)?;
        if !a.is_number() || !b.is_number() {
            return Err(VmFault::new("Operands must be numbers.", offset));
        }
        Ok((a.as_number(), b.as_number()))
    }

    fn replace_binary(&mut self, value: Value) {
        self.pop();
        self.pop();
        let pushed = self.push(value);
        debug_assert!(pushed);
    }
}

fn instruction_width_at(function: *mut ObjFunction, offset: usize) -> Result<usize, String> {
    let chunk = unsafe { &(*function).chunk };
    let opcode = *chunk
        .code
        .get(offset)
        .ok_or_else(|| format!("Missing opcode at {offset}."))?;
    if chunk.spans.get(offset).is_none() {
        return Err(format!("Missing instruction span at {offset}."));
    }
    let width = if matches!(
        opcode,
        x if x == OpCode::Constant as u8
            || x == OpCode::GetLocal as u8
            || x == OpCode::SetLocal as u8
            || x == OpCode::GetGlobal as u8
            || x == OpCode::DefineGlobal as u8
            || x == OpCode::SetGlobal as u8
            || x == OpCode::GetUpvalue as u8
            || x == OpCode::SetUpvalue as u8
            || x == OpCode::BuildList as u8
            || x == OpCode::Call as u8
    ) {
        2
    } else if matches!(
        opcode,
        x if x == OpCode::GetLocalLong as u8
            || x == OpCode::SetLocalLong as u8
            || x == OpCode::BuildListLong as u8
            || x == OpCode::Jump as u8
            || x == OpCode::JumpIfFalse as u8
            || x == OpCode::Loop as u8
    ) {
        3
    } else if opcode == OpCode::ConstantLong as u8 {
        4
    } else if opcode == OpCode::Closure as u8 {
        let constant_index = *chunk
            .code
            .get(offset + 1)
            .ok_or_else(|| format!("Truncated Closure instruction at {offset}."))?
            as usize;
        let value = *chunk
            .constants
            .get(constant_index)
            .ok_or_else(|| format!("Invalid Closure constant at {offset}."))?;
        if !value.is_function() {
            return Err(format!("Invalid Closure function at {offset}."));
        }
        let count = unsafe { (*value.as_function()).upvalue_count };
        2usize
            .checked_add(
                count
                    .checked_mul(2)
                    .ok_or_else(|| format!("Closure width overflow at {offset}."))?,
            )
            .ok_or_else(|| format!("Closure width overflow at {offset}."))?
    } else if matches!(
        opcode,
        x if x == OpCode::Nil as u8
            || x == OpCode::True as u8
            || x == OpCode::False as u8
            || x == OpCode::Pop as u8
            || x == OpCode::Dup as u8
            || x == OpCode::GetIndex as u8
            || x == OpCode::SetIndex as u8
            || x == OpCode::Equal as u8
            || x == OpCode::Greater as u8
            || x == OpCode::Less as u8
            || x == OpCode::Add as u8
            || x == OpCode::Subtract as u8
            || x == OpCode::Multiply as u8
            || x == OpCode::Divide as u8
            || x == OpCode::IntDivide as u8
            || x == OpCode::Not as u8
            || x == OpCode::Negate as u8
            || x == OpCode::Print as u8
            || x == OpCode::CloseUpvalue as u8
            || x == OpCode::Return as u8
    ) {
        1
    } else {
        return Err(format!("Unknown opcode {opcode} at {offset}."));
    };

    let end = offset
        .checked_add(width)
        .ok_or_else(|| format!("Instruction width overflow at {offset}."))?;
    if end > chunk.code.len() {
        return Err(format!("Truncated instruction at {offset}."));
    }

    let constant_index = if matches!(
        opcode,
        x if x == OpCode::Constant as u8
            || x == OpCode::GetGlobal as u8
            || x == OpCode::DefineGlobal as u8
            || x == OpCode::SetGlobal as u8
    ) {
        Some(chunk.code[offset + 1] as usize)
    } else if opcode == OpCode::ConstantLong as u8 {
        Some(
            (chunk.code[offset + 1] as usize)
                | ((chunk.code[offset + 2] as usize) << 8)
                | ((chunk.code[offset + 3] as usize) << 16),
        )
    } else {
        None
    };
    if let Some(index) = constant_index
        && chunk.constants.get(index).is_none()
    {
        return Err(format!("Invalid constant index {index} at {offset}."));
    }
    Ok(width)
}

fn is_opcode_start(function: *mut ObjFunction, target: usize) -> bool {
    let code_len = unsafe { (*function).chunk.code.len() };
    if target >= code_len {
        return false;
    }
    let mut offset = 0;
    while offset < target {
        let Ok(width) = instruction_width_at(function, offset) else {
            return false;
        };
        let Some(next) = offset.checked_add(width) else {
            return false;
        };
        offset = next;
    }
    offset == target
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RecordingHost, RevisionId, SourceId};

    fn prepared_vm(source: &str) -> (VM, RecordingHost) {
        let document = SourceDocument::new(SourceId(1), RevisionId(1), "vm-test.lox", source);
        let mut host = RecordingHost::default();
        let mut vm = VM::new();
        assert!(vm.prepare(&document, &mut host).is_ok());
        (vm, host)
    }

    fn active_function(vm: &VM) -> *mut ObjFunction {
        assert_eq!(vm.frame_count, 1);
        unsafe { (*vm.frames[0].closure).function }
    }

    #[test]
    fn malformed_constant_faults_before_trace_disassembly() {
        let (mut vm, mut host) = prepared_vm("print 1;");
        let function = active_function(&vm);
        unsafe { (*function).chunk.constants.clear() };

        let fault = vm.dispatch_one(&mut host).unwrap_err();

        assert!(fault.message.contains("Invalid constant index"));
        assert_eq!(fault.offset, 0);
    }

    #[test]
    fn malformed_span_table_faults_before_trace_disassembly() {
        let (mut vm, mut host) = prepared_vm("print 1;");
        let function = active_function(&vm);
        unsafe { (*function).chunk.spans.clear() };

        let fault = vm.dispatch_one(&mut host).unwrap_err();

        assert!(fault.message.contains("Missing instruction span"));
        assert_eq!(fault.offset, 0);
    }

    #[test]
    fn malformed_pop_cannot_cross_the_active_frame_floor() {
        let (mut vm, mut host) = prepared_vm("print 1;");
        let function = active_function(&vm);
        unsafe { (&mut (*function).chunk.code)[0] = OpCode::Pop as u8 };

        let fault = vm.dispatch_one(&mut host).unwrap_err();

        assert_eq!(fault.message, "Stack underflow.");
        assert_eq!(fault.offset, 0);
    }
}
