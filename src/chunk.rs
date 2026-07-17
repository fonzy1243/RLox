use crate::SourceSpan;
use crate::value::{Value, values_equal};
#[cfg(test)]
use std::collections::HashSet;

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

    #[cfg(test)]
    pub(crate) fn opcode_starts(&self) -> Result<HashSet<usize>, String> {
        let mut starts = HashSet::new();
        let mut offset = 0;
        while offset < self.code.len() {
            starts.insert(offset);
            let opcode = self.code[offset];
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
                if offset + 2 > self.code.len() {
                    return Err(format!("truncated Closure instruction at {offset}"));
                }
                let constant = self.code[offset + 1] as usize;
                let Some(value) = self.constants.get(constant) else {
                    return Err(format!("invalid Closure constant at {offset}"));
                };
                if !value.is_function() {
                    return Err(format!("non-function Closure constant at {offset}"));
                }
                let function = value.as_function();
                let descriptors = unsafe { (*function).upvalue_count }
                    .checked_mul(2)
                    .ok_or_else(|| format!("Closure width overflow at {offset}"))?;
                2usize
                    .checked_add(descriptors)
                    .ok_or_else(|| format!("Closure width overflow at {offset}"))?
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
                return Err(format!("unknown opcode {opcode} at {offset}"));
            };

            let end = offset
                .checked_add(width)
                .ok_or_else(|| format!("instruction width overflow at {offset}"))?;
            if end > self.code.len() {
                return Err(format!("truncated instruction at {offset}"));
            }
            offset = end;
        }
        Ok(starts)
    }
}

#[cfg(test)]
mod tests {
    use super::{Chunk, OpCode};

    #[test]
    fn opcode_start_validation_rejects_unknown_opcodes() {
        let mut chunk = Chunk::new();
        chunk.code.push(u8::MAX);

        assert!(
            chunk
                .opcode_starts()
                .unwrap_err()
                .contains("unknown opcode")
        );
    }

    #[test]
    fn opcode_start_validation_rejects_truncated_operands() {
        let mut chunk = Chunk::new();
        chunk.code.push(OpCode::ConstantLong as u8);
        chunk.code.push(0);

        assert!(chunk.opcode_starts().unwrap_err().contains("truncated"));
    }
}
