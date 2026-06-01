use crate::chunk::{Chunk, OpCode};

pub fn disassemble_chunk(chunk: &Chunk, name: &str) {
    println!("== {} ==", name);

    let mut offset = 0;
    while offset < chunk.code.len() {
        offset = disassemble_instruction(chunk, offset);
    }
}

pub fn simple_instruction(name: &str, offset: usize) -> usize {
    println!("{}", name);
    return offset + 1;
}

pub fn byte_instruction(name: &str, chunk: &Chunk, offset: usize) -> usize {
    let slot = chunk.code[offset + 1];
    println!("{:-16} {:4}", name, slot);
    offset + 2
}

pub fn local_long_instruction(name: &str, chunk: &Chunk, offset: usize) -> usize {
    let slot = chunk.code[offset + 1] as usize | ((chunk.code[offset + 2] as usize) << 8);
    println!("{:-16} {:4}", name, slot);
    offset + 3
}

pub fn constant_instruction(name: &str, chunk: &Chunk, offset: usize) -> usize {
    let constant = chunk.code[offset + 1] as usize;
    println!(
        "{:-16} {:4} '{}'",
        name, constant, chunk.constants[constant]
    );
    offset + 2
}

pub fn constant_long_instruction(name: &str, chunk: &Chunk, offset: usize) -> usize {
    let index = chunk.code[offset + 1] as usize
        | ((chunk.code[offset + 2] as usize) << 8)
        | ((chunk.code[offset + 3] as usize) << 16);
    println!("{:-16} {:4} '{}'", name, index, chunk.constants[index]);
    offset + 4 // opcode + 3 bytes
}

pub fn disassemble_instruction(chunk: &Chunk, offset: usize) -> usize {
    print!("{:04} ", offset);

    let line = chunk.get_line(offset);
    if offset > 0 && line == chunk.get_line(offset - 1) {
        print!("   | ");
    } else {
        print!("{:04} ", line);
    }

    let instruction = chunk.code[offset];
    match instruction {
        x if x == OpCode::Constant as u8 => constant_instruction("OP_CONSTANT", chunk, offset),
        x if x == OpCode::ConstantLong as u8 => {
            constant_long_instruction("OP_CONSTANT_LONG", chunk, offset)
        }
        x if x == OpCode::Nil as u8 => simple_instruction("OP_NIL", offset),
        x if x == OpCode::True as u8 => simple_instruction("OP_TRUE", offset),
        x if x == OpCode::False as u8 => simple_instruction("OP_FALSE", offset),
        x if x == OpCode::Equal as u8 => simple_instruction("OP_EQUAL", offset),
        x if x == OpCode::Greater as u8 => simple_instruction("OP_GREATER", offset),
        x if x == OpCode::Less as u8 => simple_instruction("OP_LESS", offset),
        x if x == OpCode::Add as u8 => simple_instruction("OP_ADD", offset),
        x if x == OpCode::Subtract as u8 => simple_instruction("OP_SUBTRACT", offset),
        x if x == OpCode::Multiply as u8 => simple_instruction("OP_MULTIPLY", offset),
        x if x == OpCode::Divide as u8 => simple_instruction("OP_DIVIDE", offset),
        x if x == OpCode::IntDivide as u8 => simple_instruction("OP_INT_DIVIDE", offset),
        x if x == OpCode::Not as u8 => simple_instruction("OP_NOT", offset),
        x if x == OpCode::Negate as u8 => simple_instruction("OP_NEGATE", offset),
        x if x == OpCode::Pop as u8 => simple_instruction("OP_POP", offset),
        x if x == OpCode::GetLocal as u8 => byte_instruction("OP_GET_LOCAL", chunk, offset),
        x if x == OpCode::SetLocal as u8 => byte_instruction("OP_SET_LOCAL", chunk, offset),
        x if x == OpCode::GetLocalLong as u8 => {
            local_long_instruction("OP_GET_LOCAL_LONG", chunk, offset)
        }
        x if x == OpCode::SetLocalLong as u8 => {
            local_long_instruction("OP_SET_LOCAL_LONG", chunk, offset)
        }
        x if x == OpCode::GetGlobal as u8 => constant_instruction("OP_GET_GLOBAL", chunk, offset),
        x if x == OpCode::DefineGlobal as u8 => {
            constant_instruction("OP_DEFINE_GLOBAL", chunk, offset)
        }
        x if x == OpCode::SetGlobal as u8 => constant_instruction("OP_SET_GLOBAL", chunk, offset),
        x if x == OpCode::Print as u8 => simple_instruction("OP_PRINT", offset),
        x if x == OpCode::Return as u8 => simple_instruction("OP_RETURN", offset),
        _ => {
            println!("Unknown opcode {}", instruction);
            offset + 1
        }
    }
}
