use std::{
    alloc::{Layout, alloc, dealloc},
    collections::HashSet,
    fmt,
};

use crate::chunk::Chunk;
use crate::debug_info::FunctionDebugInfo;
use crate::value::Value;
use crate::vm::VM;
#[cfg(feature = "debug_stress_gc")]
use crate::vm::collect_garbage;

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum ObjType {
    Closure,
    Function,
    Native,
    String,
    Upvalue,
    List,
}

#[repr(C)]
pub struct Obj {
    pub obj_type: ObjType,
    pub is_marked: bool,
    pub next: *mut Obj,
}

#[repr(C)]
pub struct ObjFunction {
    pub obj: Obj,
    pub arity: usize,
    pub upvalue_count: usize,
    pub chunk: Chunk,
    pub debug_info: FunctionDebugInfo,
    pub name: *mut ObjString,
}

pub type NativeFn = fn(arg_count: usize, args: &[Value]) -> Value;

#[repr(C)]
pub struct ObjNative {
    pub obj: Obj,
    pub function: NativeFn,
}

#[repr(C)]
pub struct ObjString {
    pub obj: Obj,
    pub length: usize,
    pub hash: u32,
}

#[repr(C)]
pub struct ObjUpvalue {
    pub obj: Obj,
    pub location: *mut Value,
    pub closed: Value,
    pub next: *mut ObjUpvalue,
}

#[repr(C)]
pub struct ObjClosure {
    pub obj: Obj,
    pub function: *mut ObjFunction,
    pub upvalues: Box<[*mut ObjUpvalue]>,
    pub upvalue_count: usize,
}

#[repr(C)]
pub struct ObjList {
    pub obj: Obj,
    pub items: Vec<Value>,
}

impl Obj {
    pub fn obj_type(ptr: *mut Obj) -> ObjType {
        unsafe { (*ptr).obj_type }
    }
}

impl ObjString {
    pub fn as_str<'a>(ptr: *const ObjString) -> &'a str {
        unsafe {
            let len = (*ptr).length;
            let chars_ptr = (ptr as *const u8).add(std::mem::size_of::<ObjString>());

            let slice = std::slice::from_raw_parts(chars_ptr, len);
            std::str::from_utf8_unchecked(slice)
        }
    }
}

impl fmt::Display for Obj {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut state = FormatState::new();
        if state.consume_node() {
            format_object(self as *const Obj, &mut state, 0);
        }
        f.write_str(&state.output)
    }
}

const FORMAT_DEPTH_LIMIT: usize = 64;
const FORMAT_ELEMENT_LIMIT: usize = 100;
const FORMAT_NODE_LIMIT: usize = 1_024;
const FORMAT_TOTAL_ELEMENT_LIMIT: usize = 1_024;
const FORMAT_BYTE_LIMIT: usize = 8 * 1_024;
const TRUNCATION_MARKER: &str = "<truncated>";

struct FormatState {
    output: String,
    visited: HashSet<*const Obj>,
    nodes_remaining: usize,
    elements_remaining: usize,
    truncated: bool,
}

impl FormatState {
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

pub(crate) fn format_value(value: Value, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut state = FormatState::new();
    format_value_nested(value, &mut state, 0);
    f.write_str(&state.output)
}

fn format_value_nested(value: Value, state: &mut FormatState, depth: usize) {
    if !state.consume_node() {
        return;
    }

    match value {
        Value::Bool(value) => state.write_str(if value { "true" } else { "false" }),
        Value::Nil => state.write_str("nil"),
        Value::Number(value) => state.write_str(&value.to_string()),
        Value::Obj(ptr) => format_object(ptr, state, depth),
    }
}

fn format_object(ptr: *const Obj, state: &mut FormatState, depth: usize) {
    match unsafe { (*ptr).obj_type } {
        ObjType::Closure => {
            let closure = unsafe { &*(ptr as *const ObjClosure) };
            let function = unsafe { &*closure.function };

            if function.name.is_null() {
                state.write_str("<script>");
            } else {
                state.write_str("<fn ");
                state.write_str(ObjString::as_str(function.name));
                state.write_str(">");
            }
        }
        ObjType::Function => {
            let function = unsafe { &*(ptr as *const ObjFunction) };
            if function.name.is_null() {
                state.write_str("<script>");
            } else {
                state.write_str("<fn ");
                state.write_str(ObjString::as_str(function.name));
                state.write_str(">");
            }
        }
        ObjType::Native => state.write_str("<native fn>"),
        ObjType::String => state.write_str(ObjString::as_str(ptr as *const ObjString)),
        ObjType::Upvalue => state.write_str("upvalue"),
        ObjType::List => {
            if depth >= FORMAT_DEPTH_LIMIT {
                state.write_str("<depth-limit>");
                return;
            }
            if !state.visited.insert(ptr) {
                state.write_str("<cycle>");
                return;
            }

            let list = unsafe { &*(ptr as *const ObjList) };
            state.write_str("[");
            for (index, item) in list.items.iter().take(FORMAT_ELEMENT_LIMIT).enumerate() {
                if state.truncated || !state.consume_element() {
                    break;
                }
                if index > 0 {
                    state.write_str(", ");
                }
                format_value_nested(*item, state, depth + 1);
            }
            if !state.truncated && list.items.len() > FORMAT_ELEMENT_LIMIT {
                state.truncate();
            }
            state.write_str("]");

            state.visited.remove(&ptr);
        }
    }
}

fn allocate_object<T>(vm: &mut VM, object: T) -> *mut T {
    let ptr = Box::into_raw(Box::new(object));

    unsafe {
        let obj_ptr = ptr as *mut Obj;

        (*obj_ptr).next = vm.objects;
        vm.objects = obj_ptr;
    }

    #[cfg(feature = "debug_stress_gc")]
    collect_garbage(vm);

    #[cfg(feature = "debug_log_gc")]
    eprintln!(
        "{:p} allocate for {:?}",
        ptr,
        Obj::obj_type(ptr as *mut Obj)
    );

    ptr
}

pub fn allocate_closure(vm: &mut VM, function: *mut ObjFunction) -> *mut ObjClosure {
    let upvalue_count = unsafe { (*function).upvalue_count };
    let upvalues = vec![std::ptr::null_mut(); upvalue_count].into_boxed_slice();

    let closure = Box::new(ObjClosure {
        obj: Obj {
            obj_type: ObjType::Closure,
            is_marked: false,
            next: vm.objects,
        },
        function,
        upvalues,
        upvalue_count,
    });

    let ptr = Box::into_raw(closure);
    vm.objects = ptr as *mut Obj;

    #[cfg(feature = "debug_stress_gc")]
    collect_garbage(vm);

    #[cfg(feature = "debug_log_gc")]
    eprintln!(
        "{:p} allocate for {:?}",
        ptr,
        Obj::obj_type(ptr as *mut Obj)
    );

    ptr
}

pub fn allocate_function(vm: &mut VM) -> *mut ObjFunction {
    let function = ObjFunction {
        obj: Obj {
            obj_type: ObjType::Function,
            is_marked: false,
            next: vm.objects,
        },
        arity: 0,
        upvalue_count: 0,
        chunk: Chunk::new(),
        debug_info: FunctionDebugInfo::default(),
        name: std::ptr::null_mut(),
    };

    let ptr = Box::into_raw(Box::new(function));
    vm.objects = ptr as *mut Obj;

    #[cfg(feature = "debug_stress_gc")]
    collect_garbage(vm);

    #[cfg(feature = "debug_log_gc")]
    eprintln!(
        "{:p} allocate for {:?}",
        ptr,
        Obj::obj_type(ptr as *mut Obj)
    );

    ptr
}

pub fn allocate_native(vm: &mut VM, function: NativeFn) -> *mut ObjNative {
    let native = ObjNative {
        obj: Obj {
            obj_type: ObjType::Native,
            is_marked: false,
            next: vm.objects,
        },
        function,
    };

    let ptr = Box::into_raw(Box::new(native));
    vm.objects = ptr as *mut Obj;

    #[cfg(feature = "debug_stress_gc")]
    collect_garbage(vm);

    #[cfg(feature = "debug_log_gc")]
    eprintln!(
        "{:p} allocate for {:?}",
        ptr,
        Obj::obj_type(ptr as *mut Obj)
    );

    ptr
}

pub fn allocate_string(vm: &mut VM, chars: &str, hash: u32) -> *mut ObjString {
    let len = chars.len();

    let layout = Layout::from_size_align(
        std::mem::size_of::<ObjString>() + len,
        std::mem::align_of::<ObjString>(),
    )
    .expect("Failed to create memory layout");

    unsafe {
        let ptr = alloc(layout) as *mut ObjString;
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }

        (*ptr).obj = Obj {
            obj_type: ObjType::String,
            is_marked: false,
            next: vm.objects,
        };
        (*ptr).length = len;
        (*ptr).hash = hash;
        vm.objects = ptr as *mut Obj;

        let chars_ptr = (ptr as *mut u8).add(std::mem::size_of::<ObjString>());
        std::ptr::copy_nonoverlapping(chars.as_ptr(), chars_ptr, len);

        #[cfg(feature = "debug_stress_gc")]
        collect_garbage(vm);

        #[cfg(feature = "debug_log_gc")]
        eprintln!(
            "{:p} allocate for {:?}",
            ptr,
            Obj::obj_type(ptr as *mut Obj)
        );

        ptr
    }
}

pub fn allocate_upvalue(vm: &mut VM, slot: *mut Value) -> *mut ObjUpvalue {
    let upvalue = ObjUpvalue {
        obj: Obj {
            obj_type: ObjType::Upvalue,
            is_marked: false,
            next: vm.objects,
        },
        location: slot,
        closed: Value::Nil,
        next: std::ptr::null_mut(),
    };

    let ptr = Box::into_raw(Box::new(upvalue));
    vm.objects = ptr as *mut Obj;

    #[cfg(feature = "debug_stress_gc")]
    collect_garbage(vm);

    #[cfg(feature = "debug_log_gc")]
    eprintln!(
        "{:p} allocate for {:?}",
        ptr,
        Obj::obj_type(ptr as *mut Obj)
    );

    ptr
}

pub fn capture_upvalue(vm: &mut VM, local: *mut Value) -> *mut ObjUpvalue {
    let mut prev_upvalue: *mut ObjUpvalue = std::ptr::null_mut();
    let mut upvalue = vm.open_upvalues;

    unsafe {
        while !upvalue.is_null() && (*upvalue).location > local {
            prev_upvalue = upvalue;
            upvalue = (*upvalue).next;
        }

        if !upvalue.is_null() && (*upvalue).location == local {
            return upvalue;
        }

        let created_upvalue = allocate_upvalue(vm, local);
        (*created_upvalue).next = upvalue;

        if prev_upvalue.is_null() {
            vm.open_upvalues = created_upvalue;
        } else {
            (*prev_upvalue).next = created_upvalue;
        }

        created_upvalue
    }
}

pub fn close_upvalues(vm: &mut VM, last: *mut Value) {
    unsafe {
        while !vm.open_upvalues.is_null() && (*vm.open_upvalues).location >= last {
            let upvalue = vm.open_upvalues;
            (*upvalue).closed = *(*upvalue).location;
            (*upvalue).location = &mut (*upvalue).closed;
            vm.open_upvalues = (*upvalue).next;
        }
    }
}

pub fn allocate_list(vm: &mut VM, items: Vec<Value>) -> *mut ObjList {
    let list = ObjList {
        obj: Obj {
            obj_type: ObjType::List,
            is_marked: false,
            next: vm.objects,
        },
        items,
    };

    let ptr = Box::into_raw(Box::new(list));
    vm.objects = ptr as *mut Obj;

    #[cfg(feature = "debug_stress_gc")]
    collect_garbage(vm);

    #[cfg(feature = "debug_log_gc")]
    eprintln!(
        "{:p} allocate for {:?}",
        ptr,
        Obj::obj_type(ptr as *mut Obj)
    );

    ptr
}

fn hash_string(key: &str) -> u32 {
    let mut hash = 2166136261u32;
    for byte in key.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(16777619);
    }
    hash
}

pub fn copy_string(vm: &mut VM, chars: &str) -> *mut ObjString {
    let hash = hash_string(chars);

    if let Some(interned) = vm.strings.find_string(chars, hash) {
        return interned;
    }

    let result = allocate_string(vm, chars, hash);
    vm.strings.set(result, Value::Nil);
    result
}

pub fn take_string(vm: &mut VM, chars: String) -> *mut ObjString {
    let hash = hash_string(&chars);

    if let Some(interned) = vm.strings.find_string(&chars, hash) {
        return interned;
    }

    let result = allocate_string(vm, &chars, hash);
    vm.strings.set(result, Value::Nil);
    result
}

pub fn free_object(object: *mut Obj) {
    #[cfg(feature = "debug_log_gc")]
    eprintln!("{:p} free type {:?}", object, Obj::obj_type(object));

    unsafe {
        match (*object).obj_type {
            ObjType::Closure => {
                let _ = Box::from_raw(object as *mut ObjClosure);
            }
            ObjType::Function => {
                let _ = Box::from_raw(object as *mut ObjFunction);
            }
            ObjType::Native => {
                let _ = Box::from_raw(object as *mut ObjNative);
            }
            ObjType::String => {
                let string_ptr = object as *mut ObjString;
                let len = (*string_ptr).length;

                let layout = Layout::from_size_align(
                    std::mem::size_of::<ObjString>() + len,
                    std::mem::align_of::<ObjString>(),
                )
                .unwrap();

                dealloc(object as *mut u8, layout);
            }
            ObjType::Upvalue => {
                let _ = Box::from_raw(object as *mut ObjUpvalue);
            }
            ObjType::List => {
                // Rebox drops Vec memory
                let _ = Box::from_raw(object as *mut ObjList);
            }
        }
    }
}
