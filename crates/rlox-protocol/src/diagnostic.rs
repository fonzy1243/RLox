use crate::{DiagnosticPhase, DiagnosticSeverity, SourceSpan};
use serde::{Deserialize, Serialize};

pub const MAX_DIAGNOSTIC_JSON_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_DIAGNOSTIC_CODE_BYTES: usize = 128;
pub const MAX_DIAGNOSTIC_MESSAGE_BYTES: usize = 16 * 1024;
pub const MAX_DIAGNOSTIC_FRAMES: usize = 64;
pub const MAX_DIAGNOSTIC_FUNCTION_BYTES: usize = 1024;

const INVALID_CODE: &str = "worker.invalid_diagnostic_code";
const FALLBACK_CODE: &str = "worker.diagnostic_too_large";
const FALLBACK_MESSAGE: &str = "The worker diagnostic exceeded the protocol budget.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireDiagnostic {
    pub phase: DiagnosticPhase,
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub code_truncated: bool,
    pub message: String,
    pub message_truncated: bool,
    pub span: SourceSpan,
    pub frames: Vec<WireRuntimeFrame>,
    pub frames_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRuntimeFrame {
    pub function: String,
    pub function_truncated: bool,
    pub span: SourceSpan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireDiagnosticError {
    InvalidCode,
    CodeTooLong,
    MessageTooLong,
    TooManyFrames,
    FunctionTooLong,
    JsonTooLarge,
    Serialization,
}

impl WireDiagnostic {
    pub fn bounded(
        phase: DiagnosticPhase,
        severity: DiagnosticSeverity,
        code: &str,
        message: &str,
        span: SourceSpan,
        frames: impl IntoIterator<Item = (String, SourceSpan)>,
    ) -> Self {
        let (code, code_truncated) = if valid_code(code) {
            (code.to_string(), false)
        } else {
            (INVALID_CODE.to_string(), true)
        };
        let (message, message_truncated) = truncate_utf8(message, MAX_DIAGNOSTIC_MESSAGE_BYTES);
        let mut input_frames = frames.into_iter();
        let frames: Vec<_> = input_frames
            .by_ref()
            .take(MAX_DIAGNOSTIC_FRAMES)
            .map(|(function, span)| WireRuntimeFrame::bounded(function, span))
            .collect();
        let frames_truncated = input_frames.next().is_some();
        let candidate = Self {
            phase,
            severity,
            code,
            code_truncated,
            message,
            message_truncated,
            span,
            frames,
            frames_truncated,
        };

        match candidate.json_size() {
            Ok(size) if size <= MAX_DIAGNOSTIC_JSON_BYTES => candidate,
            _ => Self::fallback(phase, severity, span),
        }
    }

    pub fn validate(&self) -> Result<(), WireDiagnosticError> {
        if self.code.len() > MAX_DIAGNOSTIC_CODE_BYTES {
            return Err(WireDiagnosticError::CodeTooLong);
        }
        if !valid_code(&self.code) {
            return Err(WireDiagnosticError::InvalidCode);
        }
        if self.message.len() > MAX_DIAGNOSTIC_MESSAGE_BYTES {
            return Err(WireDiagnosticError::MessageTooLong);
        }
        if self.frames.len() > MAX_DIAGNOSTIC_FRAMES {
            return Err(WireDiagnosticError::TooManyFrames);
        }
        if self
            .frames
            .iter()
            .any(|frame| frame.function.len() > MAX_DIAGNOSTIC_FUNCTION_BYTES)
        {
            return Err(WireDiagnosticError::FunctionTooLong);
        }
        if self.json_size()? > MAX_DIAGNOSTIC_JSON_BYTES {
            return Err(WireDiagnosticError::JsonTooLarge);
        }
        Ok(())
    }

    pub fn json_size(&self) -> Result<usize, WireDiagnosticError> {
        serde_json::to_vec(self)
            .map(|bytes| bytes.len())
            .map_err(|_| WireDiagnosticError::Serialization)
    }

    fn fallback(phase: DiagnosticPhase, severity: DiagnosticSeverity, span: SourceSpan) -> Self {
        Self {
            phase,
            severity,
            code: FALLBACK_CODE.to_string(),
            code_truncated: true,
            message: FALLBACK_MESSAGE.to_string(),
            message_truncated: true,
            span,
            frames: Vec::new(),
            frames_truncated: true,
        }
    }
}

impl WireRuntimeFrame {
    fn bounded(function: String, span: SourceSpan) -> Self {
        let (function, function_truncated) =
            truncate_utf8(&function, MAX_DIAGNOSTIC_FUNCTION_BYTES);
        Self {
            function,
            function_truncated,
            span,
        }
    }
}

fn valid_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_DIAGNOSTIC_CODE_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn truncate_utf8(value: &str, maximum: usize) -> (String, bool) {
    if value.len() <= maximum {
        return (value.to_string(), false);
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

impl From<WireDiagnosticError> for super::validation::StreamValidationError {
    fn from(_: WireDiagnosticError) -> Self {
        super::validation::StreamValidationError::InvalidPayload
    }
}
