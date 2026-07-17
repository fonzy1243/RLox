use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::Error as _};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RevisionId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextPosition {
    pub byte_offset: usize,
    pub line: usize,
    pub column: usize,
}

#[derive(Serialize)]
struct TextPositionSerialize {
    byte_offset: u64,
    line: u64,
    column: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextPositionDeserialize {
    byte_offset: u64,
    line: u64,
    column: u64,
}

impl Serialize for TextPosition {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let wire = TextPositionSerialize {
            byte_offset: u64::try_from(self.byte_offset).map_err(S::Error::custom)?,
            line: u64::try_from(self.line).map_err(S::Error::custom)?,
            column: u64::try_from(self.column).map_err(S::Error::custom)?,
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TextPosition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TextPositionDeserialize::deserialize(deserializer)?;
        Ok(Self {
            byte_offset: usize::try_from(wire.byte_offset).map_err(D::Error::custom)?,
            line: usize::try_from(wire.line).map_err(D::Error::custom)?,
            column: usize::try_from(wire.column).map_err(D::Error::custom)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
