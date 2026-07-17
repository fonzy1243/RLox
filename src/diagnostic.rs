use crate::SourceSpan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticPhase {
    Scanner,
    Parser,
    Compiler,
    Runtime,
    Worker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFrame {
    pub function: String,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub phase: DiagnosticPhase,
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub span: SourceSpan,
    pub frames: Vec<RuntimeFrame>,
}
