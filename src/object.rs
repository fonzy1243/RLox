use std::{
    alloc::{Layout, alloc, dealloc},
    fmt,
};

use crate::chunk::Chunk;
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
    pub name: *mut ObjString,
}

pub type NativeFn = fn(arg_count: usize, args: &[Value]) -> Value;

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
        match self.obj_type {
            ObjType::Closure => {
                let closure = unsafe { &*(self as *const Obj as *const ObjClosure) };
                let function = unsafe { &*closure.function };

                if function.name.is_null() {
                    write!(f, "<script>")
                } else {
                    let name = ObjString::as_str(function.name);
                    write!(f, "<fn {}>", name)
                }
            }
            ObjType::Function => {
                let function = unsafe { &*(self as *const Obj as *const ObjFunction) };
                if function.name.is_null() {
                    write!(f, "<script>")
                } else {
                    let name = ObjString::as_str(function.name);
                    write!(f, "<fn {}>", name)
                }
            }
            ObjType::Native => {
                write!(f, "<native fn>")
            }
            ObjType::String => {
                let s = ObjString::as_str(self as *const Obj as *const ObjString);
                write!(f, "{}", s)
            }
            ObjType::Upvalue => write!(f, "upvalue"),
            ObjType::List => {
                let list = unsafe { &*(self as *const Obj as *const ObjList) };

                write!(f, "[]")?;
                for (i, item) in list.items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", item)?;
                }
                write!(f, "]")
            }
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
    unsafe {
        println!("{:p} allocate for {:?}", ptr, (*ptr).obj_type);
    }

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
    unsafe {
        println!("{:p} allocate for {:?}", ptr, (*ptr).obj_type);
    }

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
        name: std::ptr::null_mut(),
    };

    let ptr = Box::into_raw(Box::new(function));
    vm.objects = ptr as *mut Obj;

    #[cfg(feature = "debug_stress_gc")]
    collect_garbage(vm);

    #[cfg(feature = "debug_log_gc")]
    unsafe {
        println!("{:p} allocate for {:?}", ptr, (*ptr).obj_type);
    }

    ptr
}

pub fn allocate_native(vm: &mut VM, function: NativeFn) -> *mut ObjNative {
    let ptr = allocate_object(vm, ObjType::Native) as *mut ObjNative;
    unsafe {
        (*ptr).function = function;
    }

    #[cfg(feature = "debug_stress_gc")]
    collect_garbage(vm);

    #[cfg(feature = "debug_log_gc")]
    unsafe {
        println!("{:p} allocate for {:?}", ptr, (*ptr).obj_type);
    }

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
        println!("{:p} allocate for {:?}", ptr, (*ptr).obj_type);

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
    unsafe {
        println!("{:p} allocate for {:?}", ptr, (*ptr).obj_type);
    }

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
    unsafe {
        println!("{:p} allocate for {:?}", ptr, (*ptr).obj_type);
    }

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
    println!("{:p} free type {:?}", object, (*object).obj_type);

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
