use crate::value::{Value, values_equal};

#[derive(Debug)]
#[repr(u8)]
pub enum OpCode {
    Constant,
    ConstantLong,
    Nil,
    True,
    False,
    Pop,
    GetLocal,
    SetLocal,
    GetGlobal,
    DefineGlobal,
    SetGlobal,
    Equal,
    Greater,
    Less,
    Add,
    Subtract,
    Multiply,
    Divide,
    Not,
    Negate,
    Print,
    Return,
}

impl From<OpCode> for u8 {
    fn from(op: OpCode) -> Self {
        op as u8
    }
}

pub struct Chunk {
    pub code: Vec<u8>,
    pub lines: Vec<usize>,
    pub constants: Vec<Value>,
}

impl Chunk {
    pub fn new() -> Self {
        Chunk {
            code: Vec::new(),
            lines: Vec::new(),
            constants: Vec::new(),
        }
    }

    pub fn write(&mut self, byte: impl Into<u8>, line: usize) {
        self.code.push(byte.into());
        // Here we implement the run-length encoding of the line numbers.
        // lines[last - 1] is the line, while lines[last] is the count
        if self.lines.len() >= 2 && self.lines[self.lines.len() - 2] == line {
            *self.lines.last_mut().unwrap() += 1;
        } else {
            self.lines.push(line);
            self.lines.push(1);
        }
    }

    pub fn get_line(&self, index: usize) -> usize {
        let mut remaining = index + 1;
        for pair in self.lines.chunks(2) {
            let (line, count) = (pair[0], pair[1]);
            if remaining <= count {
                return line;
            }
            remaining -= count;
        }
        panic!("No line info for instruction {}", index);
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

    pub fn write_constant(&mut self, value: Value, line: usize) {
        let index = self.add_constant(value);
        if index < 256 {
            self.write(OpCode::Constant, line);
            self.write(index as u8, line);
        } else {
            // store as 3 big-endian bytes
            self.write(OpCode::ConstantLong, line);
            self.write((index & 0xFF) as u8, line);
            self.write(((index >> 8) & 0xFF) as u8, line);
            self.write(((index >> 16) & 0xFF) as u8, line);
        }
    }
}
