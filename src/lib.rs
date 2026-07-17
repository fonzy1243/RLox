mod chunk;
mod compiler;
mod debug;
mod object;
mod scanner;
mod table;
mod value;
mod vm;

pub use vm::InterpretResult;

pub struct Interpreter {
    vm: vm::VM,
}

impl Interpreter {
    pub fn new() -> Self {
        Self { vm: vm::VM::new() }
    }

    pub fn interpret(&mut self, source: &str) -> InterpretResult {
        self.vm.interpret(source)
    }
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}
