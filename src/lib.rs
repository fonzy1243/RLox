mod chunk;
mod compiler;
mod debug;
mod diagnostic;
mod object;
mod runtime_host;
mod scanner;
mod source;
mod table;
mod value;
mod vm;

pub use diagnostic::{Diagnostic, DiagnosticPhase, DiagnosticSeverity, RuntimeFrame};
pub use runtime_host::{RecordingHost, RuntimeHost};
pub use source::{RevisionId, SourceDocument, SourceId, SourceSpan, TextPosition};
pub use vm::InterpretResult;

pub struct Interpreter {
    vm: vm::VM,
}

impl Interpreter {
    pub fn new() -> Self {
        Self { vm: vm::VM::new() }
    }

    pub fn interpret(&mut self, source: &str) -> InterpretResult {
        let document = SourceDocument::new(SourceId(0), RevisionId(0), "<memory>", source);
        let mut host = CompatibilityHost;
        self.run(document, &mut host)
    }

    pub fn run(&mut self, document: SourceDocument, host: &mut dyn RuntimeHost) -> InterpretResult {
        self.vm.run(&document, host)
    }
}

struct CompatibilityHost;

impl RuntimeHost for CompatibilityHost {
    fn output(&mut self, text: String) {
        println!("{text}");
    }

    fn diagnostic(&mut self, value: Diagnostic) {
        match value.phase {
            DiagnosticPhase::Runtime => {
                eprintln!("{}", value.message);
                for frame in value.frames {
                    if frame.function == "<script>" {
                        eprintln!("[line {}] in script", frame.span.start.line);
                    } else {
                        eprintln!("[line {}] in {}()", frame.span.start.line, frame.function);
                    }
                }
            }
            _ => eprintln!("[line {}] Error: {}", value.span.start.line, value.message),
        }
    }
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}
