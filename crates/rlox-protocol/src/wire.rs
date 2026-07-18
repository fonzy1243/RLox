use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::Error as _};

const MAX_DEPTH: usize = 16;
const MAX_UNSIGNED_DECIMAL_BYTES: usize = 20;

pub const MAX_SNAPSHOT_JSON_BYTES: usize = 5 * 1_048_576;

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
        TextPositionSerialize {
            byte_offset: u64::try_from(self.byte_offset).map_err(S::Error::custom)?,
            line: u64::try_from(self.line).map_err(S::Error::custom)?,
            column: u64::try_from(self.column).map_err(S::Error::custom)?,
        }
        .serialize(serializer)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DebugPointId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BindingId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActivationId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    DebugPoint,
    Step,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PauseLocation {
    pub source_id: SourceId,
    pub revision: RevisionId,
    pub span: SourceSpan,
    pub debug_point_id: DebugPointId,
    pub activation_id: ActivationId,
    pub dynamic_event: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticPhase {
    Scanner,
    Parser,
    Compiler,
    Runtime,
    Worker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum SnapshotReason {
    Paused(PauseReason),
    Faulted,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmSnapshot {
    pub reason: SnapshotReason,
    pub current_span: SourceSpan,
    pub frames: Vec<FrameSnapshot>,
    pub frames_truncated: bool,
    pub globals: Vec<BindingSnapshot>,
    pub globals_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameSnapshot {
    pub activation_id: ActivationId,
    pub function: String,
    pub function_truncated: bool,
    pub current_span: SourceSpan,
    pub call_site: Option<SourceSpan>,
    pub parameters: Vec<BindingSnapshot>,
    pub parameters_truncated: bool,
    pub locals: Vec<BindingSnapshot>,
    pub locals_truncated: bool,
    pub upvalues: Vec<BindingSnapshot>,
    pub upvalues_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingSnapshot {
    pub binding_id: Option<BindingId>,
    pub name: String,
    pub name_truncated: bool,
    pub binding_kind: String,
    pub value_kind: ValueKind,
    pub value: DebugValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    Nil,
    Bool,
    Number,
    String,
    Function,
    Closure,
    Native,
    List,
    Cycle,
    Truncated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum DebugValue {
    Nil,
    Bool(bool),
    Number(String),
    String(String),
    Function(String),
    Closure(String),
    Native(String),
    List {
        object_id: u64,
        items: Vec<DebugValue>,
        truncated: bool,
    },
    Cycle {
        object_id: u64,
    },
    Truncated,
}

#[derive(Default)]
enum PayloadField<T> {
    #[default]
    Missing,
    Present(T),
}

fn deserialize_present_payload<'de, D, T>(deserializer: D) -> Result<PayloadField<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(PayloadField::Present)
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum SnapshotReasonKind {
    Paused,
    Faulted,
    Cancelled,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotReasonDeserialize {
    kind: SnapshotReasonKind,
    #[serde(default, deserialize_with = "deserialize_present_payload")]
    payload: PayloadField<PauseReason>,
}

impl<'de> Deserialize<'de> for SnapshotReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SnapshotReasonDeserialize::deserialize(deserializer)?;
        match (wire.kind, wire.payload) {
            (SnapshotReasonKind::Paused, PayloadField::Present(reason)) => Ok(Self::Paused(reason)),
            (SnapshotReasonKind::Faulted, PayloadField::Missing) => Ok(Self::Faulted),
            (SnapshotReasonKind::Cancelled, PayloadField::Missing) => Ok(Self::Cancelled),
            _ => Err(D::Error::custom(
                "payload does not match snapshot reason kind",
            )),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DebugValueKind {
    Nil,
    Bool,
    Number,
    String,
    Function,
    Closure,
    Native,
    List,
    Cycle,
    Truncated,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DebugValuePayload {
    Bool(bool),
    String(String),
    List(DebugListPayload),
    Cycle(DebugCyclePayload),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DebugListPayload {
    object_id: u64,
    items: Vec<DebugValue>,
    truncated: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DebugCyclePayload {
    object_id: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DebugValueDeserialize {
    kind: DebugValueKind,
    #[serde(default, deserialize_with = "deserialize_present_payload")]
    payload: PayloadField<DebugValuePayload>,
}

impl<'de> Deserialize<'de> for DebugValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = DebugValueDeserialize::deserialize(deserializer)?;
        match (wire.kind, wire.payload) {
            (DebugValueKind::Nil, PayloadField::Missing) => Ok(Self::Nil),
            (DebugValueKind::Bool, PayloadField::Present(DebugValuePayload::Bool(v))) => {
                Ok(Self::Bool(v))
            }
            (DebugValueKind::Number, PayloadField::Present(DebugValuePayload::String(v))) => {
                Ok(Self::Number(v))
            }
            (DebugValueKind::String, PayloadField::Present(DebugValuePayload::String(v))) => {
                Ok(Self::String(v))
            }
            (DebugValueKind::Function, PayloadField::Present(DebugValuePayload::String(v))) => {
                Ok(Self::Function(v))
            }
            (DebugValueKind::Closure, PayloadField::Present(DebugValuePayload::String(v))) => {
                Ok(Self::Closure(v))
            }
            (DebugValueKind::Native, PayloadField::Present(DebugValuePayload::String(v))) => {
                Ok(Self::Native(v))
            }
            (DebugValueKind::List, PayloadField::Present(DebugValuePayload::List(v))) => {
                Ok(Self::List {
                    object_id: v.object_id,
                    items: v.items,
                    truncated: v.truncated,
                })
            }
            (DebugValueKind::Cycle, PayloadField::Present(DebugValuePayload::Cycle(v))) => {
                Ok(Self::Cycle {
                    object_id: v.object_id,
                })
            }
            (DebugValueKind::Truncated, PayloadField::Missing) => Ok(Self::Truncated),
            _ => Err(D::Error::custom("payload does not match debug value kind")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotSizeError {
    Overflow,
    DepthLimit,
}

impl VmSnapshot {
    pub fn conservative_json_size(&self) -> Result<usize, SnapshotSizeError> {
        let mut size = JsonSize::default();
        size.literal(r#"{"reason":"#)?;
        size.snapshot_reason(&self.reason)?;
        size.literal(r#", "current_span":"#)?;
        size.span(self.current_span)?;
        size.literal(r#", "frames":"#)?;
        size.frames(&self.frames)?;
        size.literal(r#", "frames_truncated":false,"#)?;
        size.literal(r#""globals":"#)?;
        size.bindings(&self.globals)?;
        size.literal(r#", "globals_truncated":false}"#)?;
        Ok(size.bytes)
    }
}

#[derive(Clone, Copy, Default)]
struct JsonSize {
    bytes: usize,
}

impl JsonSize {
    fn add(&mut self, bytes: usize) -> Result<(), SnapshotSizeError> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or(SnapshotSizeError::Overflow)?;
        Ok(())
    }
    fn literal(&mut self, value: &str) -> Result<(), SnapshotSizeError> {
        self.add(value.len())
    }
    fn string(&mut self, value: &str) -> Result<(), SnapshotSizeError> {
        self.add(2)?;
        for character in value.chars() {
            self.add(match character {
                '"' | '\\' => 2,
                '\u{0000}'..='\u{001f}' => 6,
                '\u{0020}'..='\u{007f}' => 1,
                '\u{0080}'..='\u{ffff}' => 6,
                _ => 12,
            })?;
        }
        Ok(())
    }
    fn unsigned(&mut self) -> Result<(), SnapshotSizeError> {
        self.add(MAX_UNSIGNED_DECIMAL_BYTES)
    }
    fn span(&mut self, _: SourceSpan) -> Result<(), SnapshotSizeError> {
        self.literal(r#"{"source_id":"#)?;
        self.unsigned()?;
        self.literal(r#", "revision":"#)?;
        self.unsigned()?;
        self.literal(r#", "start":{"byte_offset":"#)?;
        self.unsigned()?;
        self.literal(r#", "line":"#)?;
        self.unsigned()?;
        self.literal(r#", "column":"#)?;
        self.unsigned()?;
        self.literal(r#"},"end":{"byte_offset":"#)?;
        self.unsigned()?;
        self.literal(r#", "line":"#)?;
        self.unsigned()?;
        self.literal(r#", "column":"#)?;
        self.unsigned()?;
        self.literal("}}")
    }
    fn snapshot_reason(&mut self, reason: &SnapshotReason) -> Result<(), SnapshotSizeError> {
        match reason {
            SnapshotReason::Paused(reason) => {
                self.literal(r#"{"kind":"paused","payload":"#)?;
                self.string(match reason {
                    PauseReason::DebugPoint => "debug_point",
                    PauseReason::Step => "step",
                    PauseReason::Explicit => "explicit",
                })?;
                self.literal("}")
            }
            SnapshotReason::Faulted => self.literal(r#"{"kind":"faulted"}"#),
            SnapshotReason::Cancelled => self.literal(r#"{"kind":"cancelled"}"#),
        }
    }
    fn frames(&mut self, frames: &[FrameSnapshot]) -> Result<(), SnapshotSizeError> {
        self.literal("[")?;
        for (i, frame) in frames.iter().enumerate() {
            if i > 0 {
                self.literal(",")?;
            }
            self.frame(frame)?;
        }
        self.literal("]")
    }
    fn frame(&mut self, frame: &FrameSnapshot) -> Result<(), SnapshotSizeError> {
        self.literal(r#"{"activation_id":"#)?;
        self.unsigned()?;
        self.literal(r#", "function":"#)?;
        self.string(&frame.function)?;
        self.literal(r#", "function_truncated":false,"current_span":"#)?;
        self.span(frame.current_span)?;
        self.literal(r#", "call_site":"#)?;
        if let Some(span) = frame.call_site {
            self.span(span)?;
        } else {
            self.literal("null")?;
        }
        self.literal(r#", "parameters":"#)?;
        self.bindings(&frame.parameters)?;
        self.literal(r#", "parameters_truncated":false,"locals":"#)?;
        self.bindings(&frame.locals)?;
        self.literal(r#", "locals_truncated":false,"upvalues":"#)?;
        self.bindings(&frame.upvalues)?;
        self.literal(r#", "upvalues_truncated":false}"#)
    }
    fn bindings(&mut self, bindings: &[BindingSnapshot]) -> Result<(), SnapshotSizeError> {
        self.literal("[")?;
        for (i, binding) in bindings.iter().enumerate() {
            if i > 0 {
                self.literal(",")?;
            }
            self.binding(binding)?;
        }
        self.literal("]")
    }
    fn binding(&mut self, binding: &BindingSnapshot) -> Result<(), SnapshotSizeError> {
        self.literal(r#"{"binding_id":"#)?;
        if binding.binding_id.is_some() {
            self.unsigned()?;
        } else {
            self.literal("null")?;
        }
        self.literal(r#", "name":"#)?;
        self.string(&binding.name)?;
        self.literal(r#", "name_truncated":false,"binding_kind":"#)?;
        self.string(&binding.binding_kind)?;
        self.literal(r#", "value_kind":"#)?;
        self.string(value_kind_name(binding.value_kind))?;
        self.literal(r#", "value":"#)?;
        self.value_at_depth(&binding.value, 0)?;
        self.literal("}")
    }
    fn value_at_depth(
        &mut self,
        value: &DebugValue,
        depth: usize,
    ) -> Result<(), SnapshotSizeError> {
        if depth > MAX_DEPTH {
            return Err(SnapshotSizeError::DepthLimit);
        }
        match value {
            DebugValue::Nil => self.literal(r#"{"kind":"nil"}"#),
            DebugValue::Bool(_) => self.literal(r#"{"kind":"bool","payload":false}"#),
            DebugValue::Number(v) => self.tagged_string("number", v),
            DebugValue::String(v) => self.tagged_string("string", v),
            DebugValue::Function(v) => self.tagged_string("function", v),
            DebugValue::Closure(v) => self.tagged_string("closure", v),
            DebugValue::Native(v) => self.tagged_string("native", v),
            DebugValue::List { items, .. } => {
                self.literal(r#"{"kind":"list","payload":{"object_id":"#)?;
                self.unsigned()?;
                self.literal(r#", "items":"#)?;
                self.literal("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.literal(",")?;
                    }
                    self.value_at_depth(item, depth + 1)?;
                }
                self.literal(r#"], "truncated":false}}"#)
            }
            DebugValue::Cycle { .. } => {
                self.literal(r#"{"kind":"cycle","payload":{"object_id":"#)?;
                self.unsigned()?;
                self.literal("}}")
            }
            DebugValue::Truncated => self.literal(r#"{"kind":"truncated"}"#),
        }
    }
    fn tagged_string(&mut self, tag: &str, value: &str) -> Result<(), SnapshotSizeError> {
        self.literal(r#"{"kind":"#)?;
        self.literal(tag)?;
        self.literal(r#"","payload":"#)?;
        self.string(value)?;
        self.literal("}")
    }
}

fn value_kind_name(kind: ValueKind) -> &'static str {
    match kind {
        ValueKind::Nil => "nil",
        ValueKind::Bool => "bool",
        ValueKind::Number => "number",
        ValueKind::String => "string",
        ValueKind::Function => "function",
        ValueKind::Closure => "closure",
        ValueKind::Native => "native",
        ValueKind::List => "list",
        ValueKind::Cycle => "cycle",
        ValueKind::Truncated => "truncated",
    }
}
