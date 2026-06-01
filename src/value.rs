use crate::object::{Obj, ObjList, ObjString, ObjType};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Bool(bool),
    Nil,
    Number(f64),
    Obj(*mut Obj),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{}", b),
            Value::Nil => write!(f, "nil"),
            Value::Number(n) => write!(f, "{}", n),
            Value::Obj(ptr) => write!(f, "{}", unsafe { &**ptr }),
        }
    }
}

impl Value {
    // IS_ macros
    pub fn is_bool(&self) -> bool {
        matches!(self, Value::Bool(_))
    }
    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }
    pub fn is_number(&self) -> bool {
        matches!(self, Value::Number(_))
    }
    pub fn is_falsy(&self) -> bool {
        matches!(self, Value::Nil) || matches!(self, Value::Bool(false))
    }
    pub fn is_obj_type(&self, obj_type: ObjType) -> bool {
        match self {
            Value::Obj(ptr) => unsafe { (**ptr).obj_type == obj_type },
            _ => false,
        }
    }
    pub fn is_string(&self) -> bool {
        self.is_obj_type(ObjType::String)
    }
    pub fn is_list(&self) -> bool {
        self.is_obj_type(ObjType::List)
    }

    // AS_ macros
    pub fn as_bool(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            _ => panic!("Value is not a bool"),
        }
    }
    pub fn as_number(&self) -> f64 {
        match self {
            Value::Number(n) => *n,
            _ => panic!("Value is not a number"),
        }
    }
    pub fn as_obj(&self) -> *mut Obj {
        match self {
            Value::Obj(ptr) => *ptr,
            _ => panic!("Value is not an Obj"),
        }
    }
    pub fn as_string(&self) -> &ObjString {
        unsafe { &*(self.as_obj() as *const ObjString) }
    }
    pub fn as_cstring(&self) -> &str {
        ObjString::as_str(self.as_obj() as *const ObjString)
    }
    pub fn as_list(&self) -> *mut ObjList {
        match self {
            Value::Obj(ptr) => *ptr as *mut ObjList,
            _ => panic!("Value is not a list"),
        }
    }
}

pub fn obj_val(object: *mut Obj) -> Value {
    Value::Obj(object)
}

pub fn number_val(n: f64) -> Value {
    Value::Number(n)
}

pub fn bool_val(b: bool) -> Value {
    Value::Bool(b)
}

pub fn nil_val() -> Value {
    Value::Nil
}

pub fn values_equal(a: Value, b: Value) -> bool {
    match (a, b) {
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Nil, Value::Nil) => true,
        (Value::Number(a), Value::Number(b)) => a == b,
        (Value::Obj(a_ptr), Value::Obj(b_ptr)) => a_ptr == b_ptr,
        _ => false,
    }
}
