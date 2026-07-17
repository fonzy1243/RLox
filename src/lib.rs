mod chunk;
mod compiler;
mod debug;
mod debug_info;
mod diagnostic;
mod object;
mod runtime_host;
mod scanner;
mod session;
mod snapshot;
mod source;
mod table;
mod value;
mod vm;

pub use debug_info::{
    BindingDebugInfo, BindingId, BindingKind, DebugPoint, DebugPointId, DebugPointKind,
    FunctionDebugInfo, UpvalueDebugInfo,
};
pub use diagnostic::{Diagnostic, DiagnosticPhase, DiagnosticSeverity, RuntimeFrame};
pub use runtime_host::{RecordingHost, RuntimeHost};
pub use session::{
    ActivationId, ExecutionControl, ExecutionState, InterpreterSession, PauseLocation, PauseReason,
    ResumeMode, RunOutcome, SessionError, SessionOperation,
};
pub use snapshot::{
    BindingSnapshot, DebugValue, FrameSnapshot, MAX_SNAPSHOT_JSON_BYTES, MIN_ESTIMATED_JSON_BYTES,
    SnapshotBuildError, SnapshotLimitError, SnapshotLimitField, SnapshotLimits, SnapshotReason,
    SnapshotSizeError, ValueKind, VmSnapshot,
};
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
        let mut host = CompatibilityHost {
            source: document.text.clone(),
        };
        self.run(document, &mut host)
    }

    pub fn run(&mut self, document: SourceDocument, host: &mut dyn RuntimeHost) -> InterpretResult {
        self.vm.run(&document, host)
    }
}

struct CompatibilityHost {
    source: std::sync::Arc<str>,
}

impl RuntimeHost for CompatibilityHost {
    fn output(&mut self, text: String) {
        println!("{text}");
    }

    fn diagnostic(&mut self, value: Diagnostic) {
        eprint!("{}", render_diagnostic(&value, &self.source));
    }
}

fn render_diagnostic(value: &Diagnostic, source: &str) -> String {
    let mut rendered = String::new();
    if value.phase == DiagnosticPhase::Runtime {
        rendered.push_str(&value.message);
        rendered.push('\n');
        for frame in &value.frames {
            if frame.function == "<script>" {
                rendered.push_str(&format!("[line {}] in script\n", frame.span.start.line));
            } else {
                rendered.push_str(&format!(
                    "[line {}] in {}()\n",
                    frame.span.start.line, frame.function
                ));
            }
        }
        return rendered;
    }

    rendered.push_str(&format!("[line {}] Error", value.span.start.line));
    if value.phase != DiagnosticPhase::Scanner {
        if value.span.start == value.span.end && value.span.start.byte_offset == source.len() {
            rendered.push_str(" at end");
        } else if let Some(lexeme) =
            source.get(value.span.start.byte_offset..value.span.end.byte_offset)
        {
            rendered.push_str(&format!(" at '{lexeme}'"));
        }
    }
    rendered.push_str(&format!(": {}\n", value.message));
    rendered
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Diagnostic, DiagnosticPhase, DiagnosticSeverity, RevisionId, SourceId, SourceSpan,
        TextPosition, render_diagnostic,
    };

    #[test]
    fn compatibility_renderer_preserves_exact_parser_location() {
        let mut diagnostic = Diagnostic {
            phase: DiagnosticPhase::Parser,
            severity: DiagnosticSeverity::Error,
            code: "parser.error".to_string(),
            message: "Expect variable name.".to_string(),
            span: SourceSpan {
                source_id: SourceId(0),
                revision: RevisionId(0),
                start: TextPosition {
                    byte_offset: 4,
                    line: 1,
                    column: 5,
                },
                end: TextPosition {
                    byte_offset: 5,
                    line: 1,
                    column: 6,
                },
            },
            frames: Vec::new(),
        };

        assert_eq!(
            render_diagnostic(&diagnostic, "var = 1;"),
            "[line 1] Error at '=': Expect variable name.\n"
        );

        diagnostic.message = "Expect expression.".to_string();
        diagnostic.span.start = TextPosition {
            byte_offset: 6,
            line: 1,
            column: 7,
        };
        diagnostic.span.end = diagnostic.span.start;
        assert_eq!(
            render_diagnostic(&diagnostic, "print "),
            "[line 1] Error at end: Expect expression.\n"
        );
    }
}
