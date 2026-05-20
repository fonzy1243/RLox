use std::fmt;

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
    pub value: String,
}

impl Obj {
    pub fn obj_type(ptr: *mut Obj) -> ObjType {
        unsafe { (*ptr).obj_type }
    }
}

impl fmt::Display for Obj {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.obj_type {
            ObjType::String => {
                let s = unsafe { &*(self as *const Obj as *const ObjString) };
                write!(f, "{}", s.value)
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

pub fn allocate_string(vm: &mut VM, chars: String) -> *mut ObjString {
    let string = ObjString {
        obj: Obj {
            obj_type: ObjType::String,
            next: std::ptr::null_mut(),
        },
        value: chars,
    };

    allocate_object(vm, string)
}

pub fn copy_string(vm: &mut VM, chars: &str) -> *mut ObjString {
    allocate_string(vm, chars.to_string())
}

pub fn free_object(object: *mut Obj) {
    unsafe {
        match (*object).obj_type {
            ObjType::String => {
                let _ = Box::from_raw(object as *mut ObjString);
            }
        }
    }
}
