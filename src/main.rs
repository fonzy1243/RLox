mod chunk;
mod debug;
mod value;
mod vm;

use crate::chunk::{Chunk, OpCode};
use crate::debug::disassemble_chunk;

fn main() {
    let mut chunk = Chunk::new();
    // let constant = chunk.add_constant(1.2);
    // chunk.write(OpCode::Constant, 123);
    // chunk.write(constant as u8, 123);
    chunk.write_constant(1.2, 123);

    chunk.write(OpCode::Return, 123);

    disassemble_chunk(&chunk, "test_chunk");
}
