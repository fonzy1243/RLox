use crate::Diagnostic;

pub trait RuntimeHost {
    fn output(&mut self, text: String);
    fn diagnostic(&mut self, value: Diagnostic);
}

#[derive(Default)]
pub struct RecordingHost {
    output: Vec<String>,
    diagnostics: Vec<Diagnostic>,
}

impl RecordingHost {
    pub fn output(&self) -> &[String] {
        &self.output
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

impl RuntimeHost for RecordingHost {
    fn output(&mut self, text: String) {
        self.output.push(text);
    }

    fn diagnostic(&mut self, value: Diagnostic) {
        self.diagnostics.push(value);
    }
}
