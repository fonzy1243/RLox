use crate::SourceSpan;
use crate::value::{Value, values_equal};

#[derive(Debug)]
#[repr(u8)]
#[derive(PartialEq)]
pub enum OpCode {
    Constant,
    ConstantLong,
    Nil,
    True,
    False,
    Pop,
    Dup,
    GetLocal,
    SetLocal,
    GetLocalLong,
    SetLocalLong,
    GetGlobal,
    DefineGlobal,
    SetGlobal,
    GetUpvalue,
    SetUpvalue,
    BuildList,
    BuildListLong,
    GetIndex,
    SetIndex,
    Equal,
    Greater,
    Less,
    Add,
    Subtract,
    Multiply,
    Divide,
    IntDivide,
    Not,
    Negate,
    Print,
    Jump,
    JumpIfFalse,
    Loop,
    Call,
    Closure,
    CloseUpvalue,
    Return,
}

impl From<OpCode> for u8 {
    fn from(op: OpCode) -> Self {
        op as u8
    }
}

pub struct Chunk {
    pub code: Vec<u8>,
    pub spans: Vec<SourceSpan>,
    pub constants: Vec<Value>,
}

impl Chunk {
    pub fn new() -> Self {
        Chunk {
            code: Vec::new(),
            spans: Vec::new(),
            constants: Vec::new(),
        }
    }

    pub fn write(&mut self, byte: impl Into<u8>, span: SourceSpan) {
        self.code.push(byte.into());
        self.spans.push(span);
    }

    pub fn get_line(&self, index: usize) -> usize {
        self.span_at(index).start.line
    }

    pub fn span_at(&self, index: usize) -> SourceSpan {
        self.spans[index]
    }

    pub fn add_constant(&mut self, value: Value) -> usize {
        for (i, constant) in self.constants.iter().enumerate() {
            if values_equal(*constant, value) {
                return i;
            }
        }

        self.constants.push(value);
        self.constants.len() - 1
    }

    pub fn write_constant(&mut self, value: Value, span: SourceSpan) {
        let index = self.add_constant(value);
        if index < 256 {
            self.write(OpCode::Constant, span);
            self.write(index as u8, span);
        } else {
            // store as 3 big-endian bytes
            self.write(OpCode::ConstantLong, span);
            self.write((index & 0xFF) as u8, span);
            self.write(((index >> 8) & 0xFF) as u8, span);
            self.write(((index >> 16) & 0xFF) as u8, span);
        }
    }
}
