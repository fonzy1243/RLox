mod chunk;
mod debug;
mod value;
mod vm;

use crate::chunk::{Chunk, OpCode};
use crate::debug::disassemble_chunk;
use crate::vm::VM;

fn main() {
    let mut vm = VM::new();

    let mut chunk = Chunk::new();

    chunk.write_constant(1.2, 123);
    chunk.write_constant(3.4, 123);
    chunk.write(OpCode::Add, 123);

    chunk.write_constant(5.6, 123);
    chunk.write(OpCode::Divide, 123);

    chunk.write(OpCode::Negate, 123);
    chunk.write(OpCode::Return, 123);

    disassemble_chunk(&chunk, "test_chunk");

    vm.interpret(&chunk);
}
