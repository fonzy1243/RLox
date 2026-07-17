use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::chunk::OpCode;
use crate::compiler::compile;
use crate::object::{
    FORMAT_BYTE_LIMIT, FORMAT_DEPTH_LIMIT, FORMAT_ELEMENT_LIMIT, FORMAT_NODE_LIMIT,
    FORMAT_TOTAL_ELEMENT_LIMIT, NativeFn, Obj, ObjClosure, ObjFunction, ObjList, ObjString,
    ObjType, ObjUpvalue, TRUNCATION_MARKER, allocate_closure, allocate_list, allocate_native,
    allocate_upvalue, copy_string, free_object, take_string,
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
    object_registry: HashMap<*mut Obj, ObjectAllocation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObjectAllocation {
    pub kind: ObjType,
    pub string_len: Option<usize>,
}

struct CheckedFormatState {
    output: String,
    visited: HashSet<*mut Obj>,
    nodes_remaining: usize,
    elements_remaining: usize,
    truncated: bool,
}

impl CheckedFormatState {
    fn new() -> Self {
        Self {
            output: String::new(),
            visited: HashSet::new(),
            nodes_remaining: FORMAT_NODE_LIMIT,
            elements_remaining: FORMAT_TOTAL_ELEMENT_LIMIT,
            truncated: false,
        }
    }

    fn consume_node(&mut self) -> bool {
        if self.nodes_remaining == 0 {
            self.truncate();
            return false;
        }
        self.nodes_remaining -= 1;
        true
    }

    fn consume_element(&mut self) -> bool {
        if self.elements_remaining == 0 {
            self.truncate();
            return false;
        }
        self.elements_remaining -= 1;
        true
    }

    fn write_str(&mut self, value: &str) {
        if self.truncated {
            return;
        }
        let content_limit = FORMAT_BYTE_LIMIT - TRUNCATION_MARKER.len();
        let remaining = content_limit.saturating_sub(self.output.len());
        if value.len() <= remaining {
            self.output.push_str(value);
            return;
        }
        let mut end = remaining.min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        self.output.push_str(&value[..end]);
        self.truncate();
    }

    fn truncate(&mut self) {
        if !self.truncated {
            self.output.push_str(TRUNCATION_MARKER);
            self.truncated = true;
        }
    }
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
            object_registry: HashMap::new(),
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
        let fault_offset = self.current_offset().unwrap_or(0);
        self.require_object_kind(closure as *mut Obj, ObjType::Closure, fault_offset)?;
        let function = unsafe { (*closure).function };
        self.require_object_kind(function as *mut Obj, ObjType::Function, fault_offset)?;
        let arity = unsafe { (*function).arity };
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
        self.close_upvalues_for_cleanup();
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

    pub(crate) fn register_object(
        &mut self,
        object: *mut Obj,
        kind: ObjType,
        string_len: Option<usize>,
    ) {
        let previous = self
            .object_registry
            .insert(object, ObjectAllocation { kind, string_len });
        debug_assert!(previous.is_none(), "object address registered twice");
    }

    pub(crate) fn object_allocation(&self, object: *mut Obj) -> Option<ObjectAllocation> {
        self.object_registry.get(&object).copied()
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
                let allocation = self
                    .object_registry
                    .remove(&object)
                    .expect("every allocated object remains registered until it is freed");
                free_object(object, allocation.kind, allocation.string_len);
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
        let function = self.frame_function(frame_index, current_offset)?;
        let width = instruction_width_at(self, function, current_offset)
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
                let rendered = self.format_value_checked(*value, current_offset)?;
                eprint!("[ {rendered} ]");
            }
            eprintln!();
            let constant_index = if matches!(
                instruction,
                x if x == OpCode::Constant as u8
                    || x == OpCode::GetGlobal as u8
                    || x == OpCode::DefineGlobal as u8
                    || x == OpCode::SetGlobal as u8
                    || x == OpCode::Closure as u8
            ) {
                Some(self.code_byte(function, current_offset + 1, current_offset)? as usize)
            } else if instruction == OpCode::ConstantLong as u8
                || instruction == OpCode::ClosureLong as u8
            {
                Some(self.code_u24(function, current_offset + 1, current_offset)?)
            } else {
                None
            };
            if let Some(index) = constant_index {
                let value = self.constant(function, index, current_offset)?;
                self.format_value_checked(value, current_offset)?;
            }
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
                    let Some(value) = self.globals.get(name) else {
                        let name = self.string_text(name, current_offset)?;
                        return Err(VmFault::new(
                            format!("Undefined variable '{name}'."),
                            current_offset,
                        ));
                    };
                    self.push_checked(value, current_offset)?;
                } else if instruction == OpCode::DefineGlobal as u8 {
                    let value = self.peek_checked(0, current_offset)?;
                    self.globals.set(name, value);
                    self.pop();
                } else {
                    let value = self.peek_checked(0, current_offset)?;
                    if self.globals.set(name, value) {
                        self.globals.delete(name);
                        let name = self.string_text(name, current_offset)?;
                        return Err(VmFault::new(
                            format!("Undefined variable '{name}'."),
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
                let location = self.upvalue_location(upvalue, current_offset)?;
                if instruction == OpCode::GetUpvalue as u8 {
                    let value = unsafe { *location };
                    self.push_checked(value, current_offset)?;
                } else {
                    let value = self.peek_checked(0, current_offset)?;
                    unsafe {
                        *location = value;
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
                let Some((list, ObjType::List)) = self.value_object(list_value, current_offset)?
                else {
                    return Err(VmFault::new(
                        "Only lists can be subscripted.",
                        current_offset,
                    ));
                };
                let list = unsafe { &*(list as *mut ObjList) };
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
                let Some((list, ObjType::List)) = self.value_object(list_value, current_offset)?
                else {
                    return Err(VmFault::new(
                        "Only lists can be subscripted.",
                        current_offset,
                    ));
                };
                let list = unsafe { &mut *(list as *mut ObjList) };
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
                } else if let (
                    Some((a_string, ObjType::String)),
                    Some((b_string, ObjType::String)),
                ) = (
                    self.value_object(a, current_offset)?,
                    self.value_object(b, current_offset)?,
                ) {
                    let chars = format!(
                        "{}{}",
                        self.string_text(a_string as *mut ObjString, current_offset)?,
                        self.string_text(b_string as *mut ObjString, current_offset)?
                    );
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
                let value = self.peek_checked(0, current_offset)?;
                let output = self.format_value_checked(value, current_offset)?;
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
                if !is_opcode_start(self, function, selected) {
                    return Err(VmFault::new("Invalid jump target.", current_offset));
                }
                self.frames[frame_index].ip = selected;
            }
            x if x == OpCode::Call as u8 => {
                let arg_count =
                    self.code_byte(function, current_offset + 1, current_offset)? as usize;
                self.require_stack(arg_count + 1, current_offset)?;
                let callee = self.peek_checked(arg_count, current_offset)?;
                let callee_object = self.value_object(callee, current_offset)?;
                if let Some((closure, ObjType::Closure)) = callee_object {
                    self.install_frame(
                        closure as *mut ObjClosure,
                        arg_count,
                        Some((current_offset, span)),
                    )
                    .map_err(|mut fault| {
                        fault.offset = current_offset;
                        fault
                    })?;
                } else if let Some((native, ObjType::Native)) = callee_object {
                    let native = native as *mut crate::object::ObjNative;
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
            x if x == OpCode::Closure as u8 || x == OpCode::ClosureLong as u8 => {
                let constant_index = if instruction == OpCode::Closure as u8 {
                    self.code_byte(function, current_offset + 1, current_offset)? as usize
                } else {
                    self.code_u24(function, current_offset + 1, current_offset)?
                };
                let function_value = self.constant(function, constant_index, current_offset)?;
                let Some((child_function, ObjType::Function)) =
                    self.value_object(function_value, current_offset)?
                else {
                    return Err(VmFault::new("Invalid closure function.", current_offset));
                };
                let child_function = child_function as *mut ObjFunction;
                let count = unsafe { (*child_function).upvalue_count };
                let mut descriptors = Vec::with_capacity(count);
                for index in 0..count {
                    let operand_width = if instruction == OpCode::Closure as u8 {
                        1
                    } else {
                        3
                    };
                    let descriptor_offset = current_offset + 1 + operand_width + index * 2;
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
                                .is_some_and(|upvalue| {
                                    !upvalue.is_null()
                                        && self.object_allocation(upvalue as *mut Obj).is_some_and(
                                            |allocation| allocation.kind == ObjType::Upvalue,
                                        )
                                })
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
                        self.capture_upvalue_checked(location, current_offset)?
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
                self.close_upvalues_checked(location, current_offset)?;
                self.pop();
            }
            x if x == OpCode::Return as u8 => {
                self.require_stack(1, current_offset)?;
                let result = self.peek_checked(0, current_offset)?;
                let slots = self.frames[frame_index].slots;
                self.close_upvalues_checked(slots, current_offset)?;
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

    fn stack_boundary_index(&self, location: *mut Value, offset: usize) -> Result<usize, VmFault> {
        let address = location as usize;
        let stack_start = self.stack.as_ptr() as usize;
        let stack_top = self.stack_top as usize;
        let value_size = std::mem::size_of::<Value>();
        let distance = address
            .checked_sub(stack_start)
            .ok_or_else(|| VmFault::new("Invalid upvalue location.", offset))?;
        if address > stack_top || distance % value_size != 0 {
            return Err(VmFault::new("Invalid upvalue location.", offset));
        }
        Ok(distance / value_size)
    }

    fn stack_slot_index(&self, location: *mut Value, offset: usize) -> Result<usize, VmFault> {
        let index = self.stack_boundary_index(location, offset)?;
        if location as usize == self.stack_top as usize {
            return Err(VmFault::new("Invalid upvalue location.", offset));
        }
        Ok(index)
    }

    fn validate_open_upvalues(
        &self,
        offset: usize,
    ) -> Result<Vec<(*mut ObjUpvalue, usize)>, VmFault> {
        let mut current = self.open_upvalues;
        let mut visited = HashSet::new();
        let mut previous_slot = None;
        let mut upvalues = Vec::new();

        while !current.is_null() {
            self.require_object_kind(current as *mut Obj, ObjType::Upvalue, offset)?;
            if !visited.insert(current) {
                return Err(VmFault::new("Invalid open upvalue chain.", offset));
            }

            let location = unsafe { (*current).location };
            let slot = self.stack_slot_index(location, offset)?;
            if previous_slot.is_some_and(|previous| slot >= previous) {
                return Err(VmFault::new("Invalid open upvalue chain.", offset));
            }

            upvalues.push((current, slot));
            previous_slot = Some(slot);
            current = unsafe { (*current).next };
        }

        Ok(upvalues)
    }

    fn capture_upvalue_checked(
        &mut self,
        local: *mut Value,
        offset: usize,
    ) -> Result<*mut ObjUpvalue, VmFault> {
        let local_slot = self.stack_slot_index(local, offset)?;
        let upvalues = self.validate_open_upvalues(offset)?;

        if let Some((upvalue, _)) = upvalues.iter().find(|(_, slot)| *slot == local_slot) {
            return Ok(*upvalue);
        }

        let insertion = upvalues
            .iter()
            .position(|(_, slot)| *slot < local_slot)
            .unwrap_or(upvalues.len());
        let next = upvalues
            .get(insertion)
            .map_or(std::ptr::null_mut(), |(upvalue, _)| *upvalue);
        let created = allocate_upvalue(self, local);
        unsafe { (*created).next = next };

        if insertion == 0 {
            self.open_upvalues = created;
        } else {
            let previous = upvalues[insertion - 1].0;
            unsafe { (*previous).next = created };
        }

        Ok(created)
    }

    fn close_upvalues_checked(&mut self, last: *mut Value, offset: usize) -> Result<(), VmFault> {
        let last_slot = self.stack_boundary_index(last, offset)?;
        let upvalues = self.validate_open_upvalues(offset)?;
        let close_count = upvalues
            .iter()
            .take_while(|(_, slot)| *slot >= last_slot)
            .count();
        let remaining = upvalues
            .get(close_count)
            .map_or(std::ptr::null_mut(), |(upvalue, _)| *upvalue);

        for (upvalue, _) in upvalues.into_iter().take(close_count) {
            unsafe {
                (*upvalue).closed = *(*upvalue).location;
                (*upvalue).location = std::ptr::addr_of_mut!((*upvalue).closed);
                (*upvalue).next = std::ptr::null_mut();
            }
        }
        self.open_upvalues = remaining;
        Ok(())
    }

    fn close_upvalues_for_cleanup(&mut self) {
        self.open_upvalues = std::ptr::null_mut();
        let upvalues: Vec<_> = self
            .object_registry
            .iter()
            .filter_map(|(object, allocation)| {
                (allocation.kind == ObjType::Upvalue).then_some(*object as *mut ObjUpvalue)
            })
            .collect();

        for upvalue in upvalues {
            let closed = unsafe { std::ptr::addr_of_mut!((*upvalue).closed) };
            let location = unsafe { (*upvalue).location };
            if location != closed {
                if self.stack_slot_index(location, 0).is_ok() {
                    unsafe { (*upvalue).closed = *location };
                }
                unsafe { (*upvalue).location = closed };
            }
            unsafe { (*upvalue).next = std::ptr::null_mut() };
        }
    }

    fn object_kind(&self, object: *mut Obj, offset: usize) -> Result<ObjType, VmFault> {
        let allocation = self
            .object_allocation(object)
            .ok_or_else(|| VmFault::new("Invalid object reference.", offset))?;
        let stored_tag = unsafe { std::ptr::addr_of!((*object).obj_type).cast::<u8>().read() };
        if stored_tag != allocation.kind as u8 {
            return Err(VmFault::new("Invalid object header.", offset));
        }
        Ok(allocation.kind)
    }

    fn value_object(
        &self,
        value: Value,
        offset: usize,
    ) -> Result<Option<(*mut Obj, ObjType)>, VmFault> {
        match value {
            Value::Obj(object) => Ok(Some((object, self.object_kind(object, offset)?))),
            _ => Ok(None),
        }
    }

    fn require_object_kind(
        &self,
        object: *mut Obj,
        expected: ObjType,
        offset: usize,
    ) -> Result<(), VmFault> {
        if self.object_kind(object, offset)? == expected {
            Ok(())
        } else {
            Err(VmFault::new("Invalid object type.", offset))
        }
    }

    fn string_text(&self, string: *mut ObjString, offset: usize) -> Result<&str, VmFault> {
        let object = string as *mut Obj;
        self.require_object_kind(object, ObjType::String, offset)?;
        let allocation = self
            .object_allocation(object)
            .ok_or_else(|| VmFault::new("Invalid string object.", offset))?;
        let length = allocation
            .string_len
            .ok_or_else(|| VmFault::new("Invalid string allocation.", offset))?;
        if unsafe { (*string).length } != length {
            return Err(VmFault::new("Invalid string allocation.", offset));
        }
        let bytes = unsafe {
            let chars = (string as *const u8).add(std::mem::size_of::<ObjString>());
            std::slice::from_raw_parts(chars, length)
        };
        std::str::from_utf8(bytes).map_err(|_| VmFault::new("Invalid string encoding.", offset))
    }

    fn frame_function(
        &self,
        frame_index: usize,
        offset: usize,
    ) -> Result<*mut ObjFunction, VmFault> {
        let closure = self.frames[frame_index].closure;
        self.require_object_kind(closure as *mut Obj, ObjType::Closure, offset)?;
        let function = unsafe { (*closure).function };
        self.require_object_kind(function as *mut Obj, ObjType::Function, offset)?;
        Ok(function)
    }

    fn upvalue_location(
        &self,
        upvalue: *mut ObjUpvalue,
        offset: usize,
    ) -> Result<*mut Value, VmFault> {
        self.require_object_kind(upvalue as *mut Obj, ObjType::Upvalue, offset)?;
        let location = unsafe { (*upvalue).location };
        if location.is_null() {
            return Err(VmFault::new("Invalid upvalue location.", offset));
        }
        let closed = unsafe { std::ptr::addr_of_mut!((*upvalue).closed) };
        if location == closed {
            return Ok(location);
        }

        let open_upvalues = self.validate_open_upvalues(offset)?;
        if !open_upvalues
            .iter()
            .any(|(candidate, _)| *candidate == upvalue)
        {
            return Err(VmFault::new("Invalid upvalue location.", offset));
        }
        Ok(location)
    }

    fn validate_value(&self, value: Value, offset: usize) -> Result<(), VmFault> {
        if let Value::Obj(object) = value {
            self.object_kind(object, offset)?;
        }
        Ok(())
    }

    fn format_value_checked(&self, value: Value, offset: usize) -> Result<String, VmFault> {
        let mut state = CheckedFormatState::new();
        self.format_value_nested_checked(value, &mut state, 0, offset)?;
        Ok(state.output)
    }

    fn format_value_nested_checked(
        &self,
        value: Value,
        state: &mut CheckedFormatState,
        depth: usize,
        offset: usize,
    ) -> Result<(), VmFault> {
        if !state.consume_node() {
            return Ok(());
        }
        match value {
            Value::Bool(value) => state.write_str(if value { "true" } else { "false" }),
            Value::Nil => state.write_str("nil"),
            Value::Number(value) => state.write_str(&value.to_string()),
            Value::Obj(object) => {
                let kind = self.object_kind(object, offset)?;
                match kind {
                    ObjType::Closure => {
                        let closure = unsafe { &*(object as *mut ObjClosure) };
                        self.format_function_checked(closure.function, state, offset)?;
                    }
                    ObjType::Function => {
                        self.format_function_checked(object as *mut ObjFunction, state, offset)?;
                    }
                    ObjType::Native => state.write_str("<native fn>"),
                    ObjType::String => {
                        state.write_str(self.string_text(object as *mut ObjString, offset)?);
                    }
                    ObjType::Upvalue => state.write_str("upvalue"),
                    ObjType::List => {
                        if depth >= FORMAT_DEPTH_LIMIT {
                            state.write_str("<depth-limit>");
                            return Ok(());
                        }
                        if !state.visited.insert(object) {
                            state.write_str("<cycle>");
                            return Ok(());
                        }
                        let list = unsafe { &*(object as *mut ObjList) };
                        state.write_str("[");
                        for (index, item) in
                            list.items.iter().take(FORMAT_ELEMENT_LIMIT).enumerate()
                        {
                            if state.truncated || !state.consume_element() {
                                break;
                            }
                            if index > 0 {
                                state.write_str(", ");
                            }
                            self.format_value_nested_checked(*item, state, depth + 1, offset)?;
                        }
                        if !state.truncated && list.items.len() > FORMAT_ELEMENT_LIMIT {
                            state.truncate();
                        }
                        state.write_str("]");
                        state.visited.remove(&object);
                    }
                }
            }
        }
        Ok(())
    }

    fn format_function_checked(
        &self,
        function: *mut ObjFunction,
        state: &mut CheckedFormatState,
        offset: usize,
    ) -> Result<(), VmFault> {
        self.require_object_kind(function as *mut Obj, ObjType::Function, offset)?;
        let name = unsafe { (*function).name };
        if name.is_null() {
            state.write_str("<script>");
        } else {
            state.write_str("<fn ");
            state.write_str(self.string_text(name, offset)?);
            state.write_str(">");
        }
        Ok(())
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
        let value = unsafe {
            (&(*function).chunk.constants)
                .get(index)
                .copied()
                .ok_or_else(|| VmFault::new("Invalid constant index.", offset))
        }?;
        self.validate_value(value, offset)?;
        Ok(value)
    }

    fn string_constant(
        &self,
        function: *mut ObjFunction,
        index: usize,
        offset: usize,
    ) -> Result<*mut ObjString, VmFault> {
        let value = self.constant(function, index, offset)?;
        let Some((string, ObjType::String)) = self.value_object(value, offset)? else {
            return Err(VmFault::new("Invalid string constant.", offset));
        };
        Ok(string as *mut ObjString)
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

fn instruction_width_at(
    vm: &VM,
    function: *mut ObjFunction,
    offset: usize,
) -> Result<usize, String> {
    vm.require_object_kind(function as *mut Obj, ObjType::Function, offset)
        .map_err(|fault| fault.message)?;
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
    } else if opcode == OpCode::Closure as u8 || opcode == OpCode::ClosureLong as u8 {
        let operand_width = if opcode == OpCode::Closure as u8 {
            1
        } else {
            3
        };
        let constant_index = if operand_width == 1 {
            *chunk
                .code
                .get(offset + 1)
                .ok_or_else(|| format!("Truncated Closure instruction at {offset}."))?
                as usize
        } else {
            let lo = *chunk
                .code
                .get(offset + 1)
                .ok_or_else(|| format!("Truncated Closure instruction at {offset}."))?
                as usize;
            let mid = *chunk
                .code
                .get(offset + 2)
                .ok_or_else(|| format!("Truncated Closure instruction at {offset}."))?
                as usize;
            let hi = *chunk
                .code
                .get(offset + 3)
                .ok_or_else(|| format!("Truncated Closure instruction at {offset}."))?
                as usize;
            lo | (mid << 8) | (hi << 16)
        };
        let value = *chunk
            .constants
            .get(constant_index)
            .ok_or_else(|| format!("Invalid Closure constant at {offset}."))?;
        vm.validate_value(value, offset)
            .map_err(|fault| fault.message)?;
        let Some((function_object, ObjType::Function)) = vm
            .value_object(value, offset)
            .map_err(|fault| fault.message)?
        else {
            return Err(format!("Invalid Closure function at {offset}."));
        };
        let count = unsafe { (*(function_object as *mut ObjFunction)).upvalue_count };
        (1usize + operand_width)
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
    if let Some(index) = constant_index {
        let value = chunk
            .constants
            .get(index)
            .copied()
            .ok_or_else(|| format!("Invalid constant index {index} at {offset}."))?;
        vm.validate_value(value, offset)
            .map_err(|fault| fault.message)?;
    }
    Ok(width)
}

fn is_opcode_start(vm: &VM, function: *mut ObjFunction, target: usize) -> bool {
    let code_len = unsafe { (*function).chunk.code.len() };
    if target >= code_len {
        return false;
    }
    let mut offset = 0;
    while offset < target {
        let Ok(width) = instruction_width_at(vm, function, offset) else {
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
    use crate::object::{allocate_closure, allocate_function, allocate_upvalue};
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

    fn replace_active_code(vm: &mut VM, code: Vec<u8>) {
        let function = active_function(vm);
        let span = unsafe { (*function).debug_info.declaration };
        unsafe {
            (*function).chunk.spans = vec![span; code.len()];
            (*function).chunk.code = code;
        }
        vm.frames[vm.frame_count - 1].ip = 0;
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
    fn null_object_constant_faults_before_stack_or_trace_use() {
        let (mut vm, mut host) = prepared_vm("print 1;");
        let function = active_function(&vm);
        unsafe { (&mut (*function).chunk.constants)[0] = Value::Obj(std::ptr::null_mut()) };

        let fault = vm.dispatch_one(&mut host).unwrap_err();

        assert_eq!(fault.message, "Invalid object reference.");
        assert_eq!(fault.offset, 0);
    }

    #[test]
    fn alien_object_constant_faults_before_stack_or_trace_use() {
        let (mut vm, mut host) = prepared_vm("print 1;");
        let function = active_function(&vm);
        unsafe { (&mut (*function).chunk.constants)[0] = Value::Obj(1usize as *mut Obj) };

        let fault = vm.dispatch_one(&mut host).unwrap_err();

        assert_eq!(fault.message, "Invalid object reference.");
        assert_eq!(fault.offset, 0);
    }

    #[test]
    #[cfg(feature = "debug_trace_execution")]
    fn registered_string_length_is_checked_before_trace_formatting() {
        let (mut vm, mut host) = prepared_vm("print \"safe\";");
        let function = active_function(&vm);
        let string = unsafe { (&(*function).chunk.constants)[0].as_obj() as *mut ObjString };
        let original_length = unsafe { (*string).length };
        unsafe { (*string).length = original_length + 1 };

        let fault = vm.dispatch_one(&mut host).unwrap_err();

        unsafe { (*string).length = original_length };
        assert_eq!(fault.message, "Invalid string allocation.");
        assert_eq!(fault.offset, 0);
    }

    #[test]
    fn malformed_instruction_widths_and_unknown_opcodes_fault() {
        let cases = [
            (vec![u8::MAX], "Unknown opcode"),
            (vec![OpCode::GetLocal as u8], "Truncated instruction"),
            (vec![OpCode::Jump as u8, 0], "Truncated instruction"),
            (
                vec![OpCode::ConstantLong as u8, 0, 0],
                "Truncated instruction",
            ),
            (
                vec![OpCode::ClosureLong as u8, 0, 0],
                "Truncated Closure instruction",
            ),
        ];

        for (code, expected) in cases {
            let (mut vm, mut host) = prepared_vm("print 1;");
            replace_active_code(&mut vm, code);
            let fault = vm.dispatch_one(&mut host).unwrap_err();
            assert!(fault.message.contains(expected), "{fault:?}");
            assert_eq!(fault.offset, 0);
        }
    }

    #[test]
    fn malformed_indexed_and_stack_operands_fault_without_crossing_frame_floor() {
        let cases = [
            (vec![OpCode::GetLocal as u8, u8::MAX], "Invalid local slot."),
            (vec![OpCode::GetUpvalue as u8, 0], "Invalid upvalue."),
            (vec![OpCode::BuildList as u8, 2], "Stack underflow."),
            (vec![OpCode::Call as u8, 1], "Stack underflow."),
            (vec![OpCode::Return as u8], "Stack underflow."),
        ];

        for (code, expected) in cases {
            let (mut vm, mut host) = prepared_vm("print 1;");
            replace_active_code(&mut vm, code);
            let fault = vm.dispatch_one(&mut host).unwrap_err();
            assert_eq!(fault.message, expected);
            assert_eq!(fault.offset, 0);
        }
    }

    #[test]
    fn malformed_jump_targets_fault_for_operand_interior_and_underflow() {
        let cases = [
            vec![
                OpCode::Jump as u8,
                1,
                0,
                OpCode::Constant as u8,
                0,
                OpCode::Return as u8,
            ],
            vec![OpCode::Loop as u8, 4, 0],
        ];

        for code in cases {
            let (mut vm, mut host) = prepared_vm("print 1;");
            replace_active_code(&mut vm, code);
            let fault = vm.dispatch_one(&mut host).unwrap_err();
            assert_eq!(fault.message, "Invalid jump target.");
            assert_eq!(fault.offset, 0);
        }
    }

    #[test]
    fn malformed_global_and_closure_constant_types_fault() {
        let (mut global_vm, mut global_host) = prepared_vm("print missing;");
        let global_function = active_function(&global_vm);
        unsafe { (&mut (*global_function).chunk.constants)[0] = Value::Number(1.0) };
        let global_fault = global_vm.dispatch_one(&mut global_host).unwrap_err();
        assert_eq!(global_fault.message, "Invalid string constant.");

        let (mut closure_vm, mut closure_host) = prepared_vm("fun f() {}");
        let closure_function = active_function(&closure_vm);
        unsafe { (&mut (*closure_function).chunk.constants)[1] = Value::Number(1.0) };
        let closure_fault = closure_vm.dispatch_one(&mut closure_host).unwrap_err();
        assert!(closure_fault.message.contains("Invalid Closure function"));
    }

    #[test]
    fn malformed_closure_descriptors_fault_before_allocation_or_capture() {
        let (mut vm, mut host) =
            prepared_vm("fun outer() { var value=1; fun inner() { print value; } } outer();");
        let script = active_function(&vm);
        let outer = unsafe {
            (*script)
                .chunk
                .constants
                .iter()
                .find_map(|value| match value {
                    Value::Obj(object)
                        if vm.object_allocation(*object).is_some_and(|allocation| {
                            allocation.kind == ObjType::Function
                                && !(*(*object as *mut ObjFunction)).name.is_null()
                                && ObjString::as_str((*(*object as *mut ObjFunction)).name)
                                    == "outer"
                        }) =>
                    {
                        Some(*object as *mut ObjFunction)
                    }
                    _ => None,
                })
                .unwrap()
        };
        let closure_offset = unsafe { (*outer).chunk.opcode_starts().unwrap() }
            .into_iter()
            .find(|offset| unsafe { (&(*outer).chunk.code)[*offset] == OpCode::Closure as u8 })
            .unwrap();
        vm.cleanup_execution();
        let outer_closure = allocate_closure(&mut vm, outer);
        assert!(vm.push(Value::Obj(outer_closure as *mut Obj)));
        vm.install_frame(outer_closure, 0, None).unwrap();
        while vm.current_offset().unwrap() < closure_offset {
            assert_eq!(
                vm.dispatch_one(&mut host).unwrap(),
                DispatchResult::Continue
            );
        }

        unsafe { (&mut (*outer).chunk.code)[closure_offset + 2] = 2 };
        let fault = vm.dispatch_one(&mut host).unwrap_err();
        assert_eq!(fault.message, "Invalid closure capture descriptor.");

        unsafe {
            (*outer).chunk.code.truncate(closure_offset + 3);
            (*outer).chunk.spans.truncate(closure_offset + 3);
        }
        vm.frames[0].ip = closure_offset;
        let truncated = vm.dispatch_one(&mut host).unwrap_err();
        assert!(truncated.message.contains("Truncated instruction"));
    }

    #[test]
    fn alien_open_upvalue_location_faults_before_dereference() {
        let mut vm = VM::new();
        let mut host = RecordingHost::default();
        let function = allocate_function(&mut vm);
        let span = unsafe { (*function).debug_info.declaration };
        unsafe {
            (*function).arity = 0;
            (*function).upvalue_count = 1;
            (*function).chunk.code = vec![OpCode::GetUpvalue as u8, 0];
            (*function).chunk.spans = vec![span; 2];
        }
        let closure = allocate_closure(&mut vm, function);
        let upvalue = allocate_upvalue(&mut vm, usize::MAX as *mut Value);
        unsafe { (*closure).upvalues[0] = upvalue };
        assert!(vm.push(Value::Obj(closure as *mut Obj)));
        vm.install_frame(closure, 0, None).unwrap();

        let fault = vm.dispatch_one(&mut host).unwrap_err();

        assert_eq!(fault.message, "Invalid upvalue location.");
    }

    #[test]
    fn aligned_pointer_inside_a_stack_slot_is_not_an_upvalue_location() {
        let mut vm = VM::new();
        assert!(std::mem::size_of::<Value>() > std::mem::align_of::<Value>());
        assert!(vm.push(Value::Nil));
        assert!(vm.push(Value::Nil));
        let location = unsafe {
            (vm.stack.as_mut_ptr() as *mut u8).add(std::mem::align_of::<Value>()) as *mut Value
        };
        let upvalue = allocate_upvalue(&mut vm, location);
        vm.open_upvalues = upvalue;

        let fault = vm.upvalue_location(upvalue, 0).unwrap_err();

        assert_eq!(fault.message, "Invalid upvalue location.");
    }

    #[test]
    fn checked_close_rejects_a_half_slot_and_terminal_cleanup_detaches_it() {
        let mut vm = VM::new();
        assert!(vm.push(Value::Nil));
        assert!(vm.push(Value::Nil));
        let stack_start = vm.stack.as_mut_ptr();
        let location =
            unsafe { (stack_start as *mut u8).add(std::mem::align_of::<Value>()) as *mut Value };
        let upvalue = allocate_upvalue(&mut vm, location);
        vm.open_upvalues = upvalue;

        let fault = vm.close_upvalues_checked(stack_start, 0).unwrap_err();
        assert_eq!(fault.message, "Invalid upvalue location.");

        vm.cleanup_execution();
        let closed = unsafe { std::ptr::addr_of_mut!((*upvalue).closed) };
        assert!(vm.open_upvalues.is_null());
        assert_eq!(unsafe { (*upvalue).location }, closed);
    }

    #[test]
    fn checked_close_rejects_an_open_upvalue_cycle_and_cleanup_terminates() {
        let mut vm = VM::new();
        assert!(vm.push(Value::Nil));
        assert!(vm.push(Value::Nil));
        let stack_start = vm.stack.as_mut_ptr();
        let high = allocate_upvalue(&mut vm, unsafe { stack_start.add(1) });
        let low = allocate_upvalue(&mut vm, stack_start);
        unsafe {
            (*high).next = low;
            (*low).next = high;
        }
        vm.open_upvalues = high;

        let fault = vm.close_upvalues_checked(stack_start, 0).unwrap_err();
        assert_eq!(fault.message, "Invalid open upvalue chain.");

        vm.cleanup_execution();
        assert!(vm.open_upvalues.is_null());
        assert_eq!(unsafe { (*high).location }, unsafe {
            std::ptr::addr_of_mut!((*high).closed)
        });
        assert_eq!(unsafe { (*low).location }, unsafe {
            std::ptr::addr_of_mut!((*low).closed)
        });
    }

    #[test]
    fn cleanup_detaches_registered_upvalues_hidden_by_an_alien_chain_head() {
        let mut vm = VM::new();
        assert!(vm.push(Value::Number(7.0)));
        let stack_start = vm.stack.as_mut_ptr();
        let upvalue = allocate_upvalue(&mut vm, stack_start);
        vm.open_upvalues = usize::MAX as *mut ObjUpvalue;

        vm.cleanup_execution();

        assert!(vm.open_upvalues.is_null());
        assert_eq!(unsafe { (*upvalue).closed }, Value::Number(7.0));
        assert_eq!(unsafe { (*upvalue).location }, unsafe {
            std::ptr::addr_of_mut!((*upvalue).closed)
        });
    }

    #[test]
    fn allocation_registry_rejects_and_safely_drops_a_mismatched_object_header() {
        let mut vm = VM::new();
        let string = copy_string(&mut vm, "safe");
        unsafe { (*string).obj.obj_type = ObjType::List };

        let fault = vm.object_kind(string as *mut Obj, 0).unwrap_err();

        assert_eq!(fault.message, "Invalid object header.");
    }

    #[test]
    fn checked_value_formatting_stops_at_its_depth_budget() {
        let mut vm = VM::new();
        let mut value = Value::Obj(1usize as *mut Obj);
        for _ in 0..=64 {
            value = Value::Obj(allocate_list(&mut vm, vec![value]) as *mut Obj);
        }

        let rendered = vm.format_value_checked(value, 0).unwrap();

        assert!(rendered.contains("<depth-limit>"));
        assert!(!rendered.contains("<truncated>"));
    }

    #[test]
    fn activation_exhaustion_faults_before_installing_a_frame() {
        let (mut vm, _host) = prepared_vm("print 1;");
        let closure = vm.frames[0].closure;
        let frame_count = vm.frame_count;
        vm.next_activation_id = u64::MAX;

        let fault = vm.install_frame(closure, 0, None).unwrap_err();

        assert_eq!(fault.message, "Activation counter exhausted.");
        assert_eq!(vm.frame_count, frame_count);
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
