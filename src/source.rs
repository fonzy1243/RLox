use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RevisionId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextPosition {
    pub byte_offset: usize,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSpan {
    pub source_id: SourceId,
    pub revision: RevisionId,
    pub start: TextPosition,
    pub end: TextPosition,
}

#[derive(Debug, Clone)]
pub struct SourceDocument {
    pub id: SourceId,
    pub revision: RevisionId,
    pub name: String,
    pub text: Arc<str>,
}

impl SourceDocument {
    pub fn new(
        id: SourceId,
        revision: RevisionId,
        name: impl Into<String>,
        text: impl AsRef<str>,
    ) -> Self {
        let text = text
            .as_ref()
            .strip_prefix('\u{feff}')
            .unwrap_or(text.as_ref());
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");

        Self {
            id,
            revision,
            name: name.into(),
            text: Arc::from(normalized),
        }
    }

    pub(crate) fn eof_span(&self) -> SourceSpan {
        let mut line = 1;
        let mut column = 1;
        for scalar in self.text.chars() {
            if scalar == '\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
        }
        let position = TextPosition {
            byte_offset: self.text.len(),
            line,
            column,
        };
        SourceSpan {
            source_id: self.id,
            revision: self.revision,
            start: position,
            end: position,
        }
    }
}
