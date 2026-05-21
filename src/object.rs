use std::{
    alloc::{Layout, alloc, dealloc},
    fmt,
};

use crate::value::Value;
use crate::vm::VM;

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum ObjType {
    String,
}

#[repr(C)]
pub struct Obj {
    pub obj_type: ObjType,
    pub next: *mut Obj,
}

#[repr(C)]
pub struct ObjString {
    pub obj: Obj,
    pub length: usize,
    pub hash: u32,
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
            ObjType::String => {
                let s = ObjString::as_str(self as *const Obj as *const ObjString);
                write!(f, "{}", s)
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
            next: vm.objects,
        };
        (*ptr).length = len;
        (*ptr).hash = hash;
        vm.objects = ptr as *mut Obj;

        let chars_ptr = (ptr as *mut u8).add(std::mem::size_of::<ObjString>());
        std::ptr::copy_nonoverlapping(chars.as_ptr(), chars_ptr, len);

        ptr
    }
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
    unsafe {
        match (*object).obj_type {
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
        }
    }
}
