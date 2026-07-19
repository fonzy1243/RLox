use crate::chunk::{Chunk, OpCode};

pub fn disassemble_chunk(chunk: &Chunk, name: &str) {
    eprintln!("== {} ==", name);

    let mut offset = 0;
    while offset < chunk.code.len() {
        offset = disassemble_instruction(chunk, offset);
    }
}

pub fn simple_instruction(name: &str, offset: usize) -> usize {
    eprintln!("{}", name);
    return offset + 1;
}

pub fn byte_instruction(name: &str, chunk: &Chunk, offset: usize) -> usize {
    let slot = chunk.code[offset + 1];
    eprintln!("{:-16} {:4}", name, slot);
    offset + 2
}

pub fn local_long_instruction(name: &str, chunk: &Chunk, offset: usize) -> usize {
    let slot = chunk.code[offset + 1] as usize | ((chunk.code[offset + 2] as usize) << 8);
    eprintln!("{:-16} {:4}", name, slot);
    offset + 3
}

pub fn constant_instruction(name: &str, chunk: &Chunk, offset: usize) -> usize {
    let constant = chunk.code[offset + 1] as usize;
    eprintln!(
        "{:-16} {:4} '{}'",
        name, constant, chunk.constants[constant]
    );
    offset + 2
}

pub fn constant_long_instruction(name: &str, chunk: &Chunk, offset: usize) -> usize {
    let index = chunk.code[offset + 1] as usize
        | ((chunk.code[offset + 2] as usize) << 8)
        | ((chunk.code[offset + 3] as usize) << 16);
    eprintln!("{:-16} {:4} '{}'", name, index, chunk.constants[index]);
    offset + 4 // opcode + 3 bytes
}

pub fn jump_instruction(name: &str, sign: isize, chunk: &Chunk, offset: usize) -> usize {
    let mut jump = chunk.code[offset + 1] as u16;
    jump |= (chunk.code[offset + 2] as u16) << 8;

    let target = (offset as isize + 3 + sign * (jump as isize)) as usize;
    eprintln!("{:-16} {:4} -> {}", name, offset, target);

    offset + 3
}

fn closure_instruction(name: &str, chunk: &Chunk, offset: usize, operand_width: usize) -> usize {
    let constant = if operand_width == 1 {
        chunk.code[offset + 1] as usize
    } else {
        (chunk.code[offset + 1] as usize)
            | ((chunk.code[offset + 2] as usize) << 8)
            | ((chunk.code[offset + 3] as usize) << 16)
    };
    let value = chunk.constants[constant];
    eprintln!("{:-16} {:4} {}", name, constant, value);

    let function_ptr = value.as_function();
    let upvalue_count = unsafe { (*function_ptr).upvalue_count };
    let mut current_offset = offset + 1 + operand_width;
    for _ in 0..upvalue_count {
        let is_local = chunk.code[current_offset];
        let index = chunk.code[current_offset + 1];
        eprintln!(
            "{:04}      |                     {} {}",
            current_offset,
            if is_local == 1 { "local" } else { "upvalue" },
            index
        );
        current_offset += 2;
    }
    current_offset
}

pub fn disassemble_instruction(chunk: &Chunk, offset: usize) -> usize {
    eprint!("{:04} ", offset);

    let line = chunk.get_line(offset);
    if offset > 0 && line == chunk.get_line(offset - 1) {
        eprint!("   | ");
    } else {
        eprint!("{:04} ", line);
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
        x if x == OpCode::Dup as u8 => simple_instruction("OP_DUP", offset),
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
        x if x == OpCode::GetUpvalue as u8 => byte_instruction("OP_GET_UPVALUE", chunk, offset),
        x if x == OpCode::SetUpvalue as u8 => byte_instruction("OP_SET_UPVALUE", chunk, offset),
        x if x == OpCode::BuildList as u8 => byte_instruction("OP_BUILD_LIST", chunk, offset),
        x if x == OpCode::BuildListLong as u8 => {
            local_long_instruction("OP_BUILD_LIST_LONG", chunk, offset)
        }
        x if x == OpCode::GetIndex as u8 => simple_instruction("OP_GET_INDEX", offset),
        x if x == OpCode::SetIndex as u8 => simple_instruction("OP_SET_INDEX", offset),
        x if x == OpCode::Print as u8 => simple_instruction("OP_PRINT", offset),
        x if x == OpCode::Jump as u8 => jump_instruction("OP_JUMP", 1, chunk, offset),
        x if x == OpCode::JumpIfFalse as u8 => {
            jump_instruction("OP_JUMP_IF_FALSE", 1, chunk, offset)
        }
        x if x == OpCode::Loop as u8 => jump_instruction("OP_LOOP", -1, chunk, offset),
        x if x == OpCode::Call as u8 => byte_instruction("OP_CALL", chunk, offset),
        x if x == OpCode::Closure as u8 => closure_instruction("OP_CLOSURE", chunk, offset, 1),
        x if x == OpCode::ClosureLong as u8 => {
            closure_instruction("OP_CLOSURE_LONG", chunk, offset, 3)
        }
        x if x == OpCode::CloseUpvalue as u8 => simple_instruction("OP_CLOSE_UPVALUE", offset),
        x if x == OpCode::Return as u8 => simple_instruction("OP_RETURN", offset),
        _ => {
            eprintln!("Unknown opcode {}", instruction);
            offset + 1
        }
    }
}
