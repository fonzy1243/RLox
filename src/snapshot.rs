use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::OnceLock;

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::chunk::OpCode;
use crate::object::{
    Obj, ObjClosure, ObjFunction, ObjList, ObjNative, ObjString, ObjType, ObjUpvalue,
};
use crate::value::Value;
use crate::{ActivationId, BindingId, BindingKind, PauseReason, SourceSpan};

pub const MIN_ESTIMATED_JSON_BYTES: usize = 1_024;

const MAX_DEPTH: usize = 16;
const MAX_COLLECTION_ITEMS: usize = 256;
const MAX_STRING_BYTES: usize = 65_536;
const MAX_TOTAL_STRING_BYTES: usize = 1_048_576;
const MAX_VALUE_NODES: usize = 16_384;
const MAX_FRAMES: usize = 256;
const MAX_BINDINGS_PER_FRAME: usize = 512;
const MAX_TOTAL_BINDINGS: usize = 16_384;
const MAX_GLOBALS: usize = 8_192;
pub const MAX_SNAPSHOT_JSON_BYTES: usize = 5 * 1_048_576;
const MAX_UNSIGNED_DECIMAL_BYTES: usize = 20;

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
            (DebugValueKind::Bool, PayloadField::Present(DebugValuePayload::Bool(value))) => {
                Ok(Self::Bool(value))
            }
            (DebugValueKind::Number, PayloadField::Present(DebugValuePayload::String(value))) => {
                Ok(Self::Number(value))
            }
            (DebugValueKind::String, PayloadField::Present(DebugValuePayload::String(value))) => {
                Ok(Self::String(value))
            }
            (DebugValueKind::Function, PayloadField::Present(DebugValuePayload::String(value))) => {
                Ok(Self::Function(value))
            }
            (DebugValueKind::Closure, PayloadField::Present(DebugValuePayload::String(value))) => {
                Ok(Self::Closure(value))
            }
            (DebugValueKind::Native, PayloadField::Present(DebugValuePayload::String(value))) => {
                Ok(Self::Native(value))
            }
            (DebugValueKind::List, PayloadField::Present(DebugValuePayload::List(value))) => {
                Ok(Self::List {
                    object_id: value.object_id,
                    items: value.items,
                    truncated: value.truncated,
                })
            }
            (DebugValueKind::Cycle, PayloadField::Present(DebugValuePayload::Cycle(value))) => {
                Ok(Self::Cycle {
                    object_id: value.object_id,
                })
            }
            (DebugValueKind::Truncated, PayloadField::Missing) => Ok(Self::Truncated),
            _ => Err(D::Error::custom("payload does not match debug value kind")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotLimits {
    pub max_depth: usize,
    pub max_collection_items: usize,
    pub max_string_bytes: usize,
    pub max_total_string_bytes: usize,
    pub max_value_nodes: usize,
    pub max_frames: usize,
    pub max_bindings_per_frame: usize,
    pub max_total_bindings: usize,
    pub max_globals: usize,
    pub max_estimated_json_bytes: usize,
}

impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            max_depth: 4,
            max_collection_items: 64,
            max_string_bytes: 4_096,
            max_total_string_bytes: 1_048_576,
            max_value_nodes: 10_000,
            max_frames: 256,
            max_bindings_per_frame: 512,
            max_total_bindings: 8_192,
            max_globals: 2_048,
            max_estimated_json_bytes: MAX_SNAPSHOT_JSON_BYTES,
        }
    }
}

impl SnapshotLimits {
    pub fn validate(&self) -> Result<(), SnapshotLimitError> {
        validate_max(SnapshotLimitField::Depth, self.max_depth, MAX_DEPTH)?;
        validate_max(
            SnapshotLimitField::CollectionItems,
            self.max_collection_items,
            MAX_COLLECTION_ITEMS,
        )?;
        validate_max(
            SnapshotLimitField::StringBytes,
            self.max_string_bytes,
            MAX_STRING_BYTES,
        )?;
        validate_max(
            SnapshotLimitField::TotalStringBytes,
            self.max_total_string_bytes,
            MAX_TOTAL_STRING_BYTES,
        )?;
        validate_max(
            SnapshotLimitField::ValueNodes,
            self.max_value_nodes,
            MAX_VALUE_NODES,
        )?;
        validate_max(SnapshotLimitField::Frames, self.max_frames, MAX_FRAMES)?;
        validate_max(
            SnapshotLimitField::BindingsPerFrame,
            self.max_bindings_per_frame,
            MAX_BINDINGS_PER_FRAME,
        )?;
        validate_max(
            SnapshotLimitField::TotalBindings,
            self.max_total_bindings,
            MAX_TOTAL_BINDINGS,
        )?;
        validate_max(SnapshotLimitField::Globals, self.max_globals, MAX_GLOBALS)?;
        if self.max_estimated_json_bytes < MIN_ESTIMATED_JSON_BYTES {
            return Err(SnapshotLimitError::BelowMinimum {
                field: SnapshotLimitField::EstimatedJsonBytes,
                requested: self.max_estimated_json_bytes,
                minimum: MIN_ESTIMATED_JSON_BYTES,
            });
        }
        validate_max(
            SnapshotLimitField::EstimatedJsonBytes,
            self.max_estimated_json_bytes,
            MAX_SNAPSHOT_JSON_BYTES,
        )
    }
}

fn validate_max(
    field: SnapshotLimitField,
    requested: usize,
    maximum: usize,
) -> Result<(), SnapshotLimitError> {
    if requested > maximum {
        Err(SnapshotLimitError::AboveMaximum {
            field,
            requested,
            maximum,
        })
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotLimitField {
    Depth,
    CollectionItems,
    StringBytes,
    TotalStringBytes,
    ValueNodes,
    Frames,
    BindingsPerFrame,
    TotalBindings,
    Globals,
    EstimatedJsonBytes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotLimitError {
    BelowMinimum {
        field: SnapshotLimitField,
        requested: usize,
        minimum: usize,
    },
    AboveMaximum {
        field: SnapshotLimitField,
        requested: usize,
        maximum: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotSizeError {
    Overflow,
    DepthLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotBuildError {
    FrameState,
    StackState,
    BindingMetadata,
    TableState,
    ObjectState,
    UpvalueState,
    ObjectIdExhausted,
    SizeOverflow,
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
            let bytes = match character {
                '"' | '\\' => 2,
                '\u{0000}'..='\u{001f}' => 6,
                '\u{0020}'..='\u{007f}' => 1,
                '\u{0080}'..='\u{ffff}' => 6,
                _ => 12,
            };
            self.add(bytes)?;
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
        for (index, frame) in frames.iter().enumerate() {
            if index > 0 {
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
        for (index, binding) in bindings.iter().enumerate() {
            if index > 0 {
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
        self.value(&binding.value)?;
        self.literal("}")
    }

    fn value(&mut self, value: &DebugValue) -> Result<(), SnapshotSizeError> {
        self.value_at_depth(value, 0)
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
            DebugValue::Number(value) => self.tagged_string("number", value),
            DebugValue::String(value) => self.tagged_string("string", value),
            DebugValue::Function(value) => self.tagged_string("function", value),
            DebugValue::Closure(value) => self.tagged_string("closure", value),
            DebugValue::Native(value) => self.tagged_string("native", value),
            DebugValue::List {
                items, truncated, ..
            } => {
                self.literal(r#"{"kind":"list","payload":{"object_id":"#)?;
                self.unsigned()?;
                self.literal(r#", "items":"#)?;
                self.values_at_depth(items, depth + 1)?;
                let _ = truncated;
                self.literal(r#", "truncated":false}}"#)
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

    fn values_at_depth(
        &mut self,
        values: &[DebugValue],
        depth: usize,
    ) -> Result<(), SnapshotSizeError> {
        self.literal("[")?;
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                self.literal(",")?;
            }
            self.value_at_depth(value, depth)?;
        }
        self.literal("]")
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

#[derive(Clone, Copy)]
pub(crate) struct SnapshotRequest {
    pub reason: SnapshotReason,
    pub active_offset: Option<usize>,
    pub current_span: SourceSpan,
}

pub(crate) fn unavailable_snapshot(reason: SnapshotReason, current_span: SourceSpan) -> VmSnapshot {
    VmSnapshot {
        reason,
        current_span,
        frames: Vec::new(),
        frames_truncated: true,
        globals: Vec::new(),
        globals_truncated: true,
    }
}

struct SchemaEstimates {
    snapshot: usize,
    frame: usize,
    binding: usize,
    value: usize,
}

fn schema_estimates() -> &'static SchemaEstimates {
    static ESTIMATES: OnceLock<SchemaEstimates> = OnceLock::new();
    ESTIMATES.get_or_init(|| {
        let position = crate::TextPosition {
            byte_offset: 0,
            line: 1,
            column: 1,
        };
        let span = SourceSpan {
            source_id: crate::SourceId(0),
            revision: crate::RevisionId(0),
            start: position,
            end: position,
        };
        let snapshot = VmSnapshot {
            reason: SnapshotReason::Paused(PauseReason::DebugPoint),
            current_span: span,
            frames: Vec::new(),
            frames_truncated: false,
            globals: Vec::new(),
            globals_truncated: false,
        }
        .conservative_json_size()
        .expect("the fixed snapshot schema has a finite size");
        let frame = FrameSnapshot {
            activation_id: ActivationId(u64::MAX),
            function: String::new(),
            function_truncated: false,
            current_span: span,
            call_site: Some(span),
            parameters: Vec::new(),
            parameters_truncated: false,
            locals: Vec::new(),
            locals_truncated: false,
            upvalues: Vec::new(),
            upvalues_truncated: false,
        };
        let mut frame_size = JsonSize::default();
        frame_size
            .frame(&frame)
            .expect("the fixed frame schema has a finite size");
        let binding = BindingSnapshot {
            binding_id: Some(BindingId(u64::MAX)),
            name: String::new(),
            name_truncated: false,
            binding_kind: String::new(),
            value_kind: ValueKind::Truncated,
            value: DebugValue::Nil,
        };
        let mut binding_size = JsonSize::default();
        binding_size
            .binding(&binding)
            .expect("the fixed binding schema has a finite size");
        let values = [
            DebugValue::Nil,
            DebugValue::Bool(false),
            DebugValue::Number(String::new()),
            DebugValue::String(String::new()),
            DebugValue::Function(String::new()),
            DebugValue::Closure(String::new()),
            DebugValue::Native(String::new()),
            DebugValue::List {
                object_id: u64::MAX,
                items: Vec::new(),
                truncated: false,
            },
            DebugValue::Cycle {
                object_id: u64::MAX,
            },
            DebugValue::Truncated,
        ];
        let value = values
            .iter()
            .map(|value| {
                let mut size = JsonSize::default();
                size.value(value)
                    .expect("the fixed value schema has a finite size");
                size.bytes
            })
            .max()
            .unwrap();
        SchemaEstimates {
            snapshot,
            frame: frame_size.bytes + 1,
            binding: binding_size.bytes + 1,
            value: value + 1,
        }
    })
}

#[derive(Clone, Copy)]
struct BudgetCheckpoint {
    total_string_bytes: usize,
    value_nodes: usize,
    total_bindings: usize,
    estimated_json_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AggregateBudget {
    TotalStringBytes,
    ValueNodes,
    TotalBindings,
    EstimatedJsonBytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DynamicStringAdmission {
    Admitted,
    PerStringLimit,
    Exhausted(AggregateBudget),
}

struct BuildBudget<'a> {
    limits: &'a SnapshotLimits,
    total_string_bytes: usize,
    value_nodes: usize,
    total_bindings: usize,
    estimated_json_bytes: usize,
    exhausted: Option<AggregateBudget>,
}

impl<'a> BuildBudget<'a> {
    fn new(limits: &'a SnapshotLimits) -> Result<Self, SnapshotBuildError> {
        let snapshot_estimate = schema_estimates().snapshot;
        if snapshot_estimate > limits.max_estimated_json_bytes {
            return Err(SnapshotBuildError::SizeOverflow);
        }
        Ok(Self {
            limits,
            total_string_bytes: 0,
            value_nodes: 0,
            total_bindings: 0,
            estimated_json_bytes: snapshot_estimate,
            exhausted: None,
        })
    }

    fn checkpoint(&self) -> BudgetCheckpoint {
        BudgetCheckpoint {
            total_string_bytes: self.total_string_bytes,
            value_nodes: self.value_nodes,
            total_bindings: self.total_bindings,
            estimated_json_bytes: self.estimated_json_bytes,
        }
    }

    fn restore(&mut self, checkpoint: BudgetCheckpoint) {
        self.total_string_bytes = checkpoint.total_string_bytes;
        self.value_nodes = checkpoint.value_nodes;
        self.total_bindings = checkpoint.total_bindings;
        self.estimated_json_bytes = checkpoint.estimated_json_bytes;
    }

    fn is_exhausted(&self) -> bool {
        self.exhausted.is_some()
    }

    fn exhaust(&mut self, budget: AggregateBudget) -> AggregateBudget {
        *self.exhausted.get_or_insert(budget)
    }

    fn reserve_estimate(&mut self, bytes: usize) -> Result<(), AggregateBudget> {
        if let Some(exhausted) = self.exhausted {
            return Err(exhausted);
        }
        let Some(total) = self.estimated_json_bytes.checked_add(bytes) else {
            return Err(self.exhaust(AggregateBudget::EstimatedJsonBytes));
        };
        if total > self.limits.max_estimated_json_bytes {
            return Err(self.exhaust(AggregateBudget::EstimatedJsonBytes));
        }
        self.estimated_json_bytes = total;
        Ok(())
    }

    fn reserve_owned_string(&mut self, value: &str) -> Result<(), AggregateBudget> {
        if let Some(exhausted) = self.exhausted {
            return Err(exhausted);
        }
        let Some(total) = self.total_string_bytes.checked_add(value.len()) else {
            return Err(self.exhaust(AggregateBudget::TotalStringBytes));
        };
        if total > self.limits.max_total_string_bytes {
            return Err(self.exhaust(AggregateBudget::TotalStringBytes));
        }
        let mut escaped = JsonSize::default();
        if escaped.string(value).is_err() {
            return Err(self.exhaust(AggregateBudget::EstimatedJsonBytes));
        }
        self.reserve_estimate(escaped.bytes)?;
        self.total_string_bytes = total;
        Ok(())
    }

    fn reserve_dynamic_string(&mut self, value: &str) -> DynamicStringAdmission {
        if value.len() > self.limits.max_string_bytes {
            return DynamicStringAdmission::PerStringLimit;
        }
        match self.reserve_owned_string(value) {
            Ok(()) => DynamicStringAdmission::Admitted,
            Err(exhausted) => DynamicStringAdmission::Exhausted(exhausted),
        }
    }

    fn reserve_fixed_string(&mut self, value: &'static str) -> Result<(), AggregateBudget> {
        self.reserve_owned_string(value)
    }

    fn reserve_binding(&mut self) -> Result<(), AggregateBudget> {
        if let Some(exhausted) = self.exhausted {
            return Err(exhausted);
        }
        if self.total_bindings >= self.limits.max_total_bindings {
            return Err(self.exhaust(AggregateBudget::TotalBindings));
        }
        self.reserve_estimate(schema_estimates().binding)?;
        self.total_bindings += 1;
        Ok(())
    }

    fn reserve_value(&mut self) -> Result<(), AggregateBudget> {
        if let Some(exhausted) = self.exhausted {
            return Err(exhausted);
        }
        if self.value_nodes >= self.limits.max_value_nodes {
            return Err(self.exhaust(AggregateBudget::ValueNodes));
        }
        self.reserve_estimate(schema_estimates().value)?;
        self.value_nodes += 1;
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct ValidatedFrame {
    closure: *mut ObjClosure,
    function: *mut ObjFunction,
    base: usize,
    end: usize,
    liveness_offset: usize,
    activation_id: ActivationId,
    current_span: SourceSpan,
    call_site: Option<SourceSpan>,
}

#[derive(Clone, Copy)]
struct GlobalCandidate<'a> {
    name: &'a str,
    value: Value,
}

impl PartialEq for GlobalCandidate<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.name.as_bytes() == other.name.as_bytes()
    }
}

impl Eq for GlobalCandidate<'_> {}

impl PartialOrd for GlobalCandidate<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for GlobalCandidate<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.name.as_bytes().cmp(other.name.as_bytes())
    }
}

struct SnapshotBuilder<'a> {
    vm: &'a crate::vm::VM,
    limits: &'a SnapshotLimits,
    budget: BuildBudget<'a>,
    list_ids: HashMap<*mut Obj, u64>,
    active_lists: HashSet<*mut Obj>,
    next_list_id: u64,
    open_upvalues: HashMap<*mut ObjUpvalue, usize>,
    top_index: usize,
}

impl crate::vm::VM {
    pub(crate) fn build_snapshot(
        &self,
        request: SnapshotRequest,
        limits: &SnapshotLimits,
    ) -> Result<VmSnapshot, SnapshotBuildError> {
        limits
            .validate()
            .map_err(|_| SnapshotBuildError::SizeOverflow)?;
        SnapshotBuilder::new(self, limits)?.build(request)
    }
}

impl<'a> SnapshotBuilder<'a> {
    fn new(vm: &'a crate::vm::VM, limits: &'a SnapshotLimits) -> Result<Self, SnapshotBuildError> {
        let top_index = checked_stack_index(vm, vm.stack_top, true)?;
        let open_upvalues = validate_open_upvalues(vm, top_index)?;
        Ok(Self {
            vm,
            limits,
            budget: BuildBudget::new(limits)?,
            list_ids: HashMap::new(),
            active_lists: HashSet::new(),
            next_list_id: 1,
            open_upvalues,
            top_index,
        })
    }

    fn build(mut self, request: SnapshotRequest) -> Result<VmSnapshot, SnapshotBuildError> {
        let validated = self.validate_frames(request)?;
        let mut frames = Vec::new();
        let mut frames_truncated = validated.len() > self.limits.max_frames;

        for (index, frame) in validated
            .iter()
            .rev()
            .take(self.limits.max_frames)
            .enumerate()
        {
            let checkpoint = self.budget.checkpoint();
            if self
                .budget
                .reserve_estimate(schema_estimates().frame)
                .is_err()
            {
                self.budget.restore(checkpoint);
                frames_truncated = true;
                break;
            }
            match self.build_frame(*frame)? {
                Some(frame) => {
                    frames.push(frame);
                    if self.budget.is_exhausted() {
                        if index + 1 < validated.len().min(self.limits.max_frames) {
                            frames_truncated = true;
                        }
                        break;
                    }
                }
                None => {
                    self.budget.restore(checkpoint);
                    frames_truncated = true;
                    break;
                }
            }
        }

        let (globals, globals_truncated) = self.build_globals()?;
        let snapshot = VmSnapshot {
            reason: request.reason,
            current_span: request.current_span,
            frames,
            frames_truncated,
            globals,
            globals_truncated,
        };
        let size = snapshot
            .conservative_json_size()
            .map_err(|_| SnapshotBuildError::SizeOverflow)?;
        if size > self.limits.max_estimated_json_bytes || size > self.budget.estimated_json_bytes {
            return Err(SnapshotBuildError::SizeOverflow);
        }
        Ok(snapshot)
    }

    fn validate_frames(
        &self,
        request: SnapshotRequest,
    ) -> Result<Vec<ValidatedFrame>, SnapshotBuildError> {
        if self.vm.frame_count > self.vm.frames.len() {
            return Err(SnapshotBuildError::FrameState);
        }
        if request.active_offset.is_none() {
            if self.vm.frame_count != 0 {
                return Err(SnapshotBuildError::FrameState);
            }
            return Ok(Vec::new());
        }
        if self.vm.frame_count == 0 {
            return Err(SnapshotBuildError::FrameState);
        }

        let mut bases = Vec::with_capacity(self.vm.frame_count);
        let mut functions = Vec::with_capacity(self.vm.frame_count);
        for index in 0..self.vm.frame_count {
            let frame = self.vm.frames[index];
            let closure = require_kind(self.vm, frame.closure as *mut Obj, ObjType::Closure)
                .map_err(|_| SnapshotBuildError::FrameState)?
                as *mut ObjClosure;
            let function = unsafe { (*closure).function };
            let function = require_kind(self.vm, function as *mut Obj, ObjType::Function)
                .map_err(|_| SnapshotBuildError::FrameState)?
                as *mut ObjFunction;
            let base = checked_stack_index(self.vm, frame.slots, false)?;
            if index > 0 && base <= bases[index - 1] {
                return Err(SnapshotBuildError::FrameState);
            }
            if !matches!(self.vm.stack[base], Value::Obj(object) if object == frame.closure as *mut Obj)
            {
                return Err(SnapshotBuildError::FrameState);
            }
            let function_ref = unsafe { &*function };
            if function_ref.chunk.code.len() != function_ref.chunk.spans.len()
                || function_ref.chunk.code.is_empty()
            {
                return Err(SnapshotBuildError::FrameState);
            }
            if index == 0 {
                if frame.call_site.is_some() || frame.call_site_offset.is_some() {
                    return Err(SnapshotBuildError::FrameState);
                }
            } else if frame.call_site.is_none() || frame.call_site_offset.is_none() {
                return Err(SnapshotBuildError::FrameState);
            }
            bases.push(base);
            functions.push(function);
        }

        let mut frames = Vec::with_capacity(self.vm.frame_count);
        for index in 0..self.vm.frame_count {
            let stored = self.vm.frames[index];
            let function = functions[index];
            let function_ref = unsafe { &*function };
            let (liveness_offset, current_span) = if index + 1 == self.vm.frame_count {
                (request.active_offset.unwrap(), request.current_span)
            } else {
                let child = self.vm.frames[index + 1];
                let offset = child
                    .call_site_offset
                    .ok_or(SnapshotBuildError::FrameState)?;
                let span = child.call_site.ok_or(SnapshotBuildError::FrameState)?;
                if function_ref.chunk.code.get(offset).copied() != Some(OpCode::Call as u8)
                    || function_ref.chunk.spans.get(offset).copied() != Some(span)
                {
                    return Err(SnapshotBuildError::FrameState);
                }
                (offset, span)
            };
            if liveness_offset >= function_ref.chunk.code.len()
                || !crate::vm::is_opcode_start(self.vm, function, liveness_offset)
            {
                return Err(SnapshotBuildError::FrameState);
            }
            frames.push(ValidatedFrame {
                closure: require_kind(self.vm, stored.closure as *mut Obj, ObjType::Closure)?
                    as *mut ObjClosure,
                function,
                base: bases[index],
                end: bases.get(index + 1).copied().unwrap_or(self.top_index),
                liveness_offset,
                activation_id: stored.activation_id,
                current_span,
                call_site: stored.call_site,
            });
        }
        Ok(frames)
    }

    fn build_frame(
        &mut self,
        frame: ValidatedFrame,
    ) -> Result<Option<FrameSnapshot>, SnapshotBuildError> {
        let function_ref = unsafe { &*frame.function };
        let function_name = checked_function_name(self.vm, frame.function)?;
        let Some((function, function_truncated)) = self.owned_name(function_name) else {
            return Ok(None);
        };

        for binding in &function_ref.debug_info.bindings {
            if binding.live_start == usize::MAX
                || binding.live_end == usize::MAX
                || binding.live_start > binding.live_end
                || binding.live_end > function_ref.chunk.code.len()
            {
                return Err(SnapshotBuildError::BindingMetadata);
            }
        }

        let mut emitted = 0usize;
        let (parameters, parameters_truncated) =
            self.build_local_category(frame, BindingKind::Parameter, &mut emitted)?;
        let (locals, locals_truncated) =
            self.build_local_category(frame, BindingKind::Local, &mut emitted)?;
        let (upvalues, upvalues_truncated) = self.build_upvalues(frame, &mut emitted)?;

        Ok(Some(FrameSnapshot {
            activation_id: frame.activation_id,
            function,
            function_truncated,
            current_span: frame.current_span,
            call_site: frame.call_site,
            parameters,
            parameters_truncated,
            locals,
            locals_truncated,
            upvalues,
            upvalues_truncated,
        }))
    }

    fn build_local_category(
        &mut self,
        frame: ValidatedFrame,
        category: BindingKind,
        emitted: &mut usize,
    ) -> Result<(Vec<BindingSnapshot>, bool), SnapshotBuildError> {
        let function_ref = unsafe { &*frame.function };
        let mut values = Vec::new();
        let mut truncated = false;
        for (index, binding) in function_ref.debug_info.bindings.iter().enumerate() {
            let selected = match category {
                BindingKind::Parameter => binding.kind == BindingKind::Parameter,
                BindingKind::Local => {
                    matches!(binding.kind, BindingKind::Local | BindingKind::Implicit)
                }
                BindingKind::Implicit => false,
            };
            if !selected
                || binding.slot == 0
                || !(binding.live_start <= frame.liveness_offset
                    && frame.liveness_offset < binding.live_end)
            {
                continue;
            }
            if self.budget.is_exhausted() {
                truncated = true;
                break;
            }
            if *emitted >= self.limits.max_bindings_per_frame {
                truncated = true;
                break;
            }
            let slot = frame
                .base
                .checked_add(binding.slot as usize)
                .ok_or(SnapshotBuildError::BindingMetadata)?;
            if slot >= frame.end || slot >= self.top_index {
                return Err(SnapshotBuildError::BindingMetadata);
            }
            let kind = match binding.kind {
                BindingKind::Parameter => "parameter",
                BindingKind::Local => "local",
                BindingKind::Implicit => "implicit",
            };
            match self.build_binding(Some(binding.id), &binding.name, kind, |builder| {
                Ok(builder.vm.stack[slot])
            })? {
                Some(value) => {
                    values.push(value);
                    *emitted += 1;
                    if self.budget.is_exhausted() {
                        truncated =
                            function_ref.debug_info.bindings[index + 1..]
                                .iter()
                                .any(|remaining| {
                                    let selected = match category {
                                        BindingKind::Parameter => {
                                            remaining.kind == BindingKind::Parameter
                                        }
                                        BindingKind::Local => matches!(
                                            remaining.kind,
                                            BindingKind::Local | BindingKind::Implicit
                                        ),
                                        BindingKind::Implicit => false,
                                    };
                                    selected
                                        && remaining.slot != 0
                                        && remaining.live_start <= frame.liveness_offset
                                        && frame.liveness_offset < remaining.live_end
                                });
                        break;
                    }
                }
                None => {
                    truncated = true;
                    break;
                }
            }
        }
        Ok((values, truncated))
    }

    fn build_upvalues(
        &mut self,
        frame: ValidatedFrame,
        emitted: &mut usize,
    ) -> Result<(Vec<BindingSnapshot>, bool), SnapshotBuildError> {
        let closure = unsafe { &*frame.closure };
        let function = unsafe { &*frame.function };
        if closure.upvalue_count != closure.upvalues.len()
            || closure.upvalue_count != function.upvalue_count
            || function.upvalue_count != function.debug_info.upvalues.len()
        {
            return Err(SnapshotBuildError::UpvalueState);
        }
        if self.budget.is_exhausted() {
            return Ok((Vec::new(), !function.debug_info.upvalues.is_empty()));
        }
        let mut seen = HashSet::new();
        let mut values = Vec::new();
        let mut truncated = false;
        for (metadata_position, metadata) in function.debug_info.upvalues.iter().enumerate() {
            if self.budget.is_exhausted() {
                truncated = true;
                break;
            }
            let index = metadata.index as usize;
            if index >= closure.upvalues.len() || !seen.insert(index) {
                return Err(SnapshotBuildError::UpvalueState);
            }
            if *emitted >= self.limits.max_bindings_per_frame {
                truncated = true;
                break;
            }
            let cell = closure.upvalues[index];
            match self.build_binding(
                Some(metadata.binding_id),
                &metadata.name,
                "upvalue",
                |builder| builder.read_upvalue(cell),
            )? {
                Some(value) => {
                    values.push(value);
                    *emitted += 1;
                    if self.budget.is_exhausted() {
                        truncated = metadata_position + 1 < function.debug_info.upvalues.len();
                        break;
                    }
                }
                None => {
                    truncated = true;
                    break;
                }
            }
        }
        Ok((values, truncated))
    }

    fn read_upvalue(&self, cell: *mut ObjUpvalue) -> Result<Value, SnapshotBuildError> {
        let cell = require_kind(self.vm, cell as *mut Obj, ObjType::Upvalue)
            .map_err(|_| SnapshotBuildError::UpvalueState)? as *mut ObjUpvalue;
        let closed = unsafe { std::ptr::addr_of!((*cell).closed) as *mut Value };
        let location = unsafe { (*cell).location };
        if location == closed {
            return Ok(unsafe { (*cell).closed });
        }
        let slot = checked_stack_index(self.vm, location, false)
            .map_err(|_| SnapshotBuildError::UpvalueState)?;
        if self.open_upvalues.get(&cell).copied() != Some(slot) {
            return Err(SnapshotBuildError::UpvalueState);
        }
        Ok(self.vm.stack[slot])
    }

    fn build_globals(&mut self) -> Result<(Vec<BindingSnapshot>, bool), SnapshotBuildError> {
        let mut selected = BinaryHeap::new();
        let mut live_count = 0usize;
        let mut visit_error = None;
        self.vm
            .globals
            .visit_live(|key, value| {
                if visit_error.is_some() {
                    return;
                }
                let result = (|| {
                    let name = checked_string(self.vm, key)?;
                    live_count = live_count
                        .checked_add(1)
                        .ok_or(SnapshotBuildError::TableState)?;
                    if self.limits.max_globals == 0 {
                        return Ok(());
                    }
                    let candidate = GlobalCandidate { name, value };
                    if selected.len() < self.limits.max_globals {
                        selected.push(candidate);
                    } else if selected.peek().is_some_and(|largest| candidate < *largest) {
                        selected.pop();
                        selected.push(candidate);
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    visit_error = Some(error);
                }
            })
            .map_err(|_| SnapshotBuildError::TableState)?;
        if let Some(error) = visit_error {
            return Err(error);
        }

        let mut selected = selected.into_vec();
        selected.sort_unstable_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
        if selected
            .windows(2)
            .any(|pair| pair[0].name.as_bytes() == pair[1].name.as_bytes())
        {
            return Err(SnapshotBuildError::TableState);
        }
        let mut truncated = live_count > selected.len();
        let mut globals = Vec::new();
        let selected_len = selected.len();
        for (index, candidate) in selected.into_iter().enumerate() {
            if self.budget.is_exhausted() {
                truncated = true;
                break;
            }
            match self.build_binding(None, candidate.name, "global", |_| Ok(candidate.value))? {
                Some(binding) => {
                    globals.push(binding);
                    if self.budget.is_exhausted() {
                        if index + 1 < selected_len {
                            truncated = true;
                        }
                        break;
                    }
                }
                None => {
                    truncated = true;
                    break;
                }
            }
        }
        Ok((globals, truncated))
    }

    fn build_binding<F>(
        &mut self,
        binding_id: Option<BindingId>,
        name: &str,
        binding_kind: &'static str,
        read_value: F,
    ) -> Result<Option<BindingSnapshot>, SnapshotBuildError>
    where
        F: FnOnce(&Self) -> Result<Value, SnapshotBuildError>,
    {
        let checkpoint = self.budget.checkpoint();
        if self.budget.reserve_binding().is_err()
            || self.budget.reserve_fixed_string(binding_kind).is_err()
        {
            self.budget.restore(checkpoint);
            return Ok(None);
        }
        let Some((name, name_truncated)) = self.owned_name(name) else {
            self.budget.restore(checkpoint);
            return Ok(None);
        };
        if self.budget.reserve_value().is_err() {
            self.budget.restore(checkpoint);
            return Ok(None);
        }
        let raw_value = read_value(self)?;
        let (value_kind, value) = self.build_admitted_value(raw_value, 0)?;
        Ok(Some(BindingSnapshot {
            binding_id,
            name,
            name_truncated,
            binding_kind: binding_kind.to_string(),
            value_kind,
            value,
        }))
    }

    fn owned_name(&mut self, value: &str) -> Option<(String, bool)> {
        match self.budget.reserve_dynamic_string(value) {
            DynamicStringAdmission::Admitted => return Some((value.to_string(), false)),
            DynamicStringAdmission::Exhausted(_) => return None,
            DynamicStringAdmission::PerStringLimit => {}
        }
        match self.budget.reserve_dynamic_string("") {
            DynamicStringAdmission::Admitted => Some((String::new(), true)),
            DynamicStringAdmission::PerStringLimit | DynamicStringAdmission::Exhausted(_) => None,
        }
    }

    fn build_value(
        &mut self,
        value: Value,
        depth: usize,
    ) -> Result<Option<(ValueKind, DebugValue)>, SnapshotBuildError> {
        if self.budget.reserve_value().is_err() {
            return Ok(None);
        }
        Ok(Some(self.build_admitted_value(value, depth)?))
    }

    fn build_admitted_value(
        &mut self,
        value: Value,
        depth: usize,
    ) -> Result<(ValueKind, DebugValue), SnapshotBuildError> {
        match value {
            Value::Nil => Ok((ValueKind::Nil, DebugValue::Nil)),
            Value::Bool(value) => Ok((ValueKind::Bool, DebugValue::Bool(value))),
            Value::Number(value) => {
                let value = canonical_number(value);
                if self.budget.reserve_dynamic_string(&value) == DynamicStringAdmission::Admitted {
                    Ok((ValueKind::Number, DebugValue::Number(value)))
                } else {
                    Ok((ValueKind::Truncated, DebugValue::Truncated))
                }
            }
            Value::Obj(object) => self
                .build_object_value(object, depth)?
                .ok_or(SnapshotBuildError::SizeOverflow),
        }
    }

    fn build_object_value(
        &mut self,
        object: *mut Obj,
        depth: usize,
    ) -> Result<Option<(ValueKind, DebugValue)>, SnapshotBuildError> {
        let (object, kind) = checked_kind(self.vm, object)?;
        match kind {
            ObjType::String => {
                let value = checked_string(self.vm, object as *mut ObjString)?;
                if self.budget.reserve_dynamic_string(value) == DynamicStringAdmission::Admitted {
                    Ok(Some((
                        ValueKind::String,
                        DebugValue::String(value.to_string()),
                    )))
                } else {
                    Ok(Some((ValueKind::Truncated, DebugValue::Truncated)))
                }
            }
            ObjType::Function => {
                self.build_callable_name(ValueKind::Function, object as *mut ObjFunction)
            }
            ObjType::Closure => {
                let closure = unsafe { &*(object as *mut ObjClosure) };
                let function =
                    require_kind(self.vm, closure.function as *mut Obj, ObjType::Function)?
                        as *mut ObjFunction;
                self.build_callable_name(ValueKind::Closure, function)
            }
            ObjType::Native => {
                let native = unsafe { &*(object as *mut ObjNative) };
                if self.budget.reserve_dynamic_string(&native.name)
                    == DynamicStringAdmission::Admitted
                {
                    Ok(Some((
                        ValueKind::Native,
                        DebugValue::Native(native.name.to_string()),
                    )))
                } else {
                    Ok(Some((ValueKind::Truncated, DebugValue::Truncated)))
                }
            }
            ObjType::List => self.build_list(object, depth),
            ObjType::Upvalue => Err(SnapshotBuildError::ObjectState),
        }
    }

    fn build_callable_name(
        &mut self,
        kind: ValueKind,
        function: *mut ObjFunction,
    ) -> Result<Option<(ValueKind, DebugValue)>, SnapshotBuildError> {
        let name = checked_function_name(self.vm, function)?;
        if self.budget.reserve_dynamic_string(name) != DynamicStringAdmission::Admitted {
            return Ok(Some((ValueKind::Truncated, DebugValue::Truncated)));
        }
        let value = match kind {
            ValueKind::Function => DebugValue::Function(name.to_string()),
            ValueKind::Closure => DebugValue::Closure(name.to_string()),
            _ => return Err(SnapshotBuildError::ObjectState),
        };
        Ok(Some((kind, value)))
    }

    fn build_list(
        &mut self,
        object: *mut Obj,
        depth: usize,
    ) -> Result<Option<(ValueKind, DebugValue)>, SnapshotBuildError> {
        let Some(object_id) = self.list_ids.get(&object).copied() else {
            let object_id = self.next_list_id;
            self.next_list_id = self
                .next_list_id
                .checked_add(1)
                .ok_or(SnapshotBuildError::ObjectIdExhausted)?;
            self.list_ids.insert(object, object_id);
            return self.expand_list(object, object_id, depth);
        };
        if self.active_lists.contains(&object) {
            return Ok(Some((ValueKind::Cycle, DebugValue::Cycle { object_id })));
        }
        self.expand_list(object, object_id, depth)
    }

    fn expand_list(
        &mut self,
        object: *mut Obj,
        object_id: u64,
        depth: usize,
    ) -> Result<Option<(ValueKind, DebugValue)>, SnapshotBuildError> {
        let list = unsafe { &*(object as *mut ObjList) };
        if depth >= self.limits.max_depth {
            return Ok(Some((
                ValueKind::List,
                DebugValue::List {
                    object_id,
                    items: Vec::new(),
                    truncated: !list.items.is_empty(),
                },
            )));
        }
        self.active_lists.insert(object);
        let result = (|| {
            let mut items = Vec::new();
            let mut truncated = list.items.len() > self.limits.max_collection_items;
            for index in 0..list.items.len().min(self.limits.max_collection_items) {
                let item = list.items[index];
                match self.build_value(item, depth + 1)? {
                    Some((_, value)) => {
                        if value == DebugValue::Truncated {
                            truncated = true;
                        }
                        items.push(value);
                        if self.budget.is_exhausted() {
                            truncated = true;
                            break;
                        }
                    }
                    None => {
                        truncated = true;
                        break;
                    }
                }
            }
            Ok(Some((
                ValueKind::List,
                DebugValue::List {
                    object_id,
                    items,
                    truncated,
                },
            )))
        })();
        self.active_lists.remove(&object);
        result
    }
}

fn canonical_number(value: f64) -> String {
    if value.is_nan() {
        "nan".to_string()
    } else if value == f64::INFINITY {
        "infinity".to_string()
    } else if value == f64::NEG_INFINITY {
        "-infinity".to_string()
    } else if value == 0.0 && value.is_sign_negative() {
        "-0".to_string()
    } else {
        value.to_string()
    }
}

fn checked_stack_index(
    vm: &crate::vm::VM,
    location: *mut Value,
    allow_top: bool,
) -> Result<usize, SnapshotBuildError> {
    let start = vm.stack.as_ptr() as usize;
    let bytes = vm
        .stack
        .len()
        .checked_mul(std::mem::size_of::<Value>())
        .ok_or(SnapshotBuildError::StackState)?;
    let end = start
        .checked_add(bytes)
        .ok_or(SnapshotBuildError::StackState)?;
    let top = vm.stack_top as usize;
    let size = std::mem::size_of::<Value>();
    if top < start || top > end || (top - start) % size != 0 {
        return Err(SnapshotBuildError::StackState);
    }
    let address = location as usize;
    if address < start || address > top || (address - start) % size != 0 {
        return Err(SnapshotBuildError::StackState);
    }
    if !allow_top && address == top {
        return Err(SnapshotBuildError::StackState);
    }
    Ok((address - start) / size)
}

fn validate_open_upvalues(
    vm: &crate::vm::VM,
    top_index: usize,
) -> Result<HashMap<*mut ObjUpvalue, usize>, SnapshotBuildError> {
    let mut values = HashMap::new();
    let mut current = vm.open_upvalues;
    let mut previous = None;
    while !current.is_null() {
        current = require_kind(vm, current as *mut Obj, ObjType::Upvalue)
            .map_err(|_| SnapshotBuildError::UpvalueState)? as *mut ObjUpvalue;
        if values.contains_key(&current) {
            return Err(SnapshotBuildError::UpvalueState);
        }
        let location = unsafe { (*current).location };
        let slot = checked_stack_index(vm, location, false)
            .map_err(|_| SnapshotBuildError::UpvalueState)?;
        if slot >= top_index || previous.is_some_and(|previous| slot >= previous) {
            return Err(SnapshotBuildError::UpvalueState);
        }
        values.insert(current, slot);
        previous = Some(slot);
        current = unsafe { (*current).next };
    }
    Ok(values)
}

fn checked_kind(
    vm: &crate::vm::VM,
    object: *mut Obj,
) -> Result<(*mut Obj, ObjType), SnapshotBuildError> {
    if object.is_null() {
        return Err(SnapshotBuildError::ObjectState);
    }
    let (canonical, allocation) = vm
        .registered_object(object)
        .ok_or(SnapshotBuildError::ObjectState)?;
    let header = unsafe {
        std::ptr::addr_of!((*canonical).obj_type)
            .cast::<u8>()
            .read()
    };
    if header != allocation.kind as u8 {
        return Err(SnapshotBuildError::ObjectState);
    }
    Ok((canonical, allocation.kind))
}

fn require_kind(
    vm: &crate::vm::VM,
    object: *mut Obj,
    expected: ObjType,
) -> Result<*mut Obj, SnapshotBuildError> {
    let (canonical, kind) = checked_kind(vm, object)?;
    if kind == expected {
        Ok(canonical)
    } else {
        Err(SnapshotBuildError::ObjectState)
    }
}

fn checked_string<'a>(
    vm: &'a crate::vm::VM,
    string: *mut ObjString,
) -> Result<&'a str, SnapshotBuildError> {
    let string = require_kind(vm, string as *mut Obj, ObjType::String)? as *mut ObjString;
    let allocation = vm
        .registered_object(string as *mut Obj)
        .ok_or(SnapshotBuildError::ObjectState)?;
    let length = allocation
        .1
        .string_len
        .ok_or(SnapshotBuildError::ObjectState)?;
    if unsafe { (*string).length } != length {
        return Err(SnapshotBuildError::ObjectState);
    }
    let bytes = unsafe {
        let chars = (string as *const u8).add(std::mem::size_of::<ObjString>());
        std::slice::from_raw_parts(chars, length)
    };
    std::str::from_utf8(bytes).map_err(|_| SnapshotBuildError::ObjectState)
}

fn checked_function_name<'a>(
    vm: &'a crate::vm::VM,
    function: *mut ObjFunction,
) -> Result<&'a str, SnapshotBuildError> {
    let function = require_kind(vm, function as *mut Obj, ObjType::Function)? as *mut ObjFunction;
    let name = unsafe { (*function).name };
    if name.is_null() {
        Ok("<script>")
    } else {
        checked_string(vm, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RevisionId, SourceId, TextPosition};

    fn span() -> SourceSpan {
        let start = TextPosition {
            byte_offset: 0,
            line: 1,
            column: 1,
        };
        SourceSpan {
            source_id: SourceId(1),
            revision: RevisionId(1),
            start,
            end: start,
        }
    }

    fn prepared_vm(source: &str) -> (crate::vm::VM, crate::SourceDocument) {
        let document = crate::SourceDocument::new(
            crate::SourceId(10),
            crate::RevisionId(4),
            "state.lox",
            source,
        );
        let mut vm = crate::vm::VM::new();
        let mut host = crate::RecordingHost::default();
        assert!(vm.prepare(&document, &mut host).is_ok());
        (vm, document)
    }

    fn active_request(vm: &crate::vm::VM, fallback: SourceSpan) -> SnapshotRequest {
        let (offset, current_span) = vm.current_snapshot_point(fallback).unwrap();
        SnapshotRequest {
            reason: SnapshotReason::Paused(PauseReason::Step),
            active_offset: Some(offset),
            current_span,
        }
    }

    fn first_live_global(vm: &mut crate::vm::VM) -> *mut crate::table::Entry {
        for index in 0..vm.globals.capacity {
            let entry = unsafe { vm.globals.entries.add(index) };
            if !unsafe { (*entry).key }.is_null() {
                return entry;
            }
        }
        panic!("new VM has a native global")
    }

    #[test]
    fn limits_accept_zero_counts_and_reject_every_excessive_field() {
        let defaults = SnapshotLimits::default();
        defaults.validate().unwrap();

        let set = |limits: &mut SnapshotLimits, field: SnapshotLimitField, value: usize| match field
        {
            SnapshotLimitField::Depth => limits.max_depth = value,
            SnapshotLimitField::CollectionItems => limits.max_collection_items = value,
            SnapshotLimitField::StringBytes => limits.max_string_bytes = value,
            SnapshotLimitField::TotalStringBytes => limits.max_total_string_bytes = value,
            SnapshotLimitField::ValueNodes => limits.max_value_nodes = value,
            SnapshotLimitField::Frames => limits.max_frames = value,
            SnapshotLimitField::BindingsPerFrame => limits.max_bindings_per_frame = value,
            SnapshotLimitField::TotalBindings => limits.max_total_bindings = value,
            SnapshotLimitField::Globals => limits.max_globals = value,
            SnapshotLimitField::EstimatedJsonBytes => limits.max_estimated_json_bytes = value,
        };

        let cases = [
            (SnapshotLimitField::Depth, MAX_DEPTH),
            (SnapshotLimitField::CollectionItems, MAX_COLLECTION_ITEMS),
            (SnapshotLimitField::StringBytes, MAX_STRING_BYTES),
            (SnapshotLimitField::TotalStringBytes, MAX_TOTAL_STRING_BYTES),
            (SnapshotLimitField::ValueNodes, MAX_VALUE_NODES),
            (SnapshotLimitField::Frames, MAX_FRAMES),
            (SnapshotLimitField::BindingsPerFrame, MAX_BINDINGS_PER_FRAME),
            (SnapshotLimitField::TotalBindings, MAX_TOTAL_BINDINGS),
            (SnapshotLimitField::Globals, MAX_GLOBALS),
            (
                SnapshotLimitField::EstimatedJsonBytes,
                MAX_SNAPSHOT_JSON_BYTES,
            ),
        ];

        for (field, maximum) in cases {
            let mut exact = defaults.clone();
            set(&mut exact, field, maximum);
            exact.validate().unwrap();

            let mut limits = defaults.clone();
            set(&mut limits, field, maximum + 1);
            assert_eq!(
                limits.validate(),
                Err(SnapshotLimitError::AboveMaximum {
                    field,
                    requested: maximum + 1,
                    maximum,
                })
            );

            let mut extreme = defaults.clone();
            set(&mut extreme, field, usize::MAX);
            assert_eq!(
                extreme.validate(),
                Err(SnapshotLimitError::AboveMaximum {
                    field,
                    requested: usize::MAX,
                    maximum,
                })
            );
        }

        for requested in [0, 1, MIN_ESTIMATED_JSON_BYTES - 1] {
            let mut below = defaults.clone();
            below.max_estimated_json_bytes = requested;
            assert_eq!(
                below.validate(),
                Err(SnapshotLimitError::BelowMinimum {
                    field: SnapshotLimitField::EstimatedJsonBytes,
                    requested,
                    minimum: MIN_ESTIMATED_JSON_BYTES,
                })
            );
        }

        let mut zero = defaults;
        zero.max_depth = 0;
        zero.max_collection_items = 0;
        zero.max_string_bytes = 0;
        zero.max_total_string_bytes = 0;
        zero.max_value_nodes = 0;
        zero.max_frames = 0;
        zero.max_bindings_per_frame = 0;
        zero.max_total_bindings = 0;
        zero.max_globals = 0;
        zero.max_estimated_json_bytes = MIN_ESTIMATED_JSON_BYTES;
        zero.validate().unwrap();
    }

    #[test]
    fn estimator_bounds_control_and_unicode_expansion() {
        let snapshot = VmSnapshot {
            reason: SnapshotReason::Paused(PauseReason::Step),
            current_span: span(),
            frames: Vec::new(),
            frames_truncated: false,
            globals: vec![BindingSnapshot {
                binding_id: None,
                name: "\0\"\\é😀".to_string(),
                name_truncated: false,
                binding_kind: "global".to_string(),
                value_kind: ValueKind::String,
                value: DebugValue::String("\0\"\\é😀".to_string()),
            }],
            globals_truncated: false,
        };

        let size = snapshot.conservative_json_size().unwrap();
        assert!(size > 300);
        assert!(size < MIN_ESTIMATED_JSON_BYTES);
    }

    #[test]
    fn estimator_rejects_public_values_beyond_the_supported_depth() {
        let mut value = DebugValue::Nil;
        for object_id in 1..=(MAX_DEPTH as u64 + 2) {
            value = DebugValue::List {
                object_id,
                items: vec![value],
                truncated: false,
            };
        }
        let snapshot = VmSnapshot {
            reason: SnapshotReason::Faulted,
            current_span: span(),
            frames: Vec::new(),
            frames_truncated: false,
            globals: vec![BindingSnapshot {
                binding_id: None,
                name: "value".to_string(),
                name_truncated: false,
                binding_kind: "global".to_string(),
                value_kind: ValueKind::List,
                value,
            }],
            globals_truncated: false,
        };

        assert_eq!(
            snapshot.conservative_json_size(),
            Err(SnapshotSizeError::DepthLimit)
        );
    }

    #[test]
    fn aggregate_budget_failures_latch_across_checkpoint_restore() {
        let mut limits = SnapshotLimits::default();
        limits.max_string_bytes = 0;
        limits.max_total_string_bytes = 6;
        let mut budget = BuildBudget::new(&limits).unwrap();
        let checkpoint = budget.checkpoint();

        assert_eq!(
            budget.reserve_dynamic_string("name"),
            DynamicStringAdmission::PerStringLimit
        );
        assert!(!budget.is_exhausted());
        budget.reserve_fixed_string("global").unwrap();
        assert_eq!(budget.total_string_bytes, 6);
        assert_eq!(
            budget.reserve_fixed_string("local"),
            Err(AggregateBudget::TotalStringBytes)
        );
        budget.restore(checkpoint);
        assert!(budget.is_exhausted());
        assert_eq!(budget.total_string_bytes, 0);
        assert_eq!(
            budget.reserve_binding(),
            Err(AggregateBudget::TotalStringBytes)
        );
    }

    #[test]
    fn table_allocation_shape_mismatch_is_rejected_before_iteration() {
        let mut vm = crate::vm::VM::new();
        let original_capacity = vm.globals.capacity;
        assert!(original_capacity > 1);
        vm.globals.capacity = original_capacity - 1;

        let result = vm.build_snapshot(
            SnapshotRequest {
                reason: SnapshotReason::Faulted,
                active_offset: None,
                current_span: span(),
            },
            &SnapshotLimits::default(),
        );

        vm.globals.capacity = original_capacity;
        assert_eq!(result, Err(SnapshotBuildError::TableState));
    }

    #[test]
    fn fault_diagnostic_rejects_an_invalid_frame_before_dereference() {
        let document = crate::SourceDocument::new(
            crate::SourceId(9),
            crate::RevisionId(2),
            "fault.lox",
            "print missing;",
        );
        let mut host = crate::RecordingHost::default();
        let mut vm = crate::vm::VM::new();
        assert!(vm.prepare(&document, &mut host).is_ok());
        let original = vm.frames[0].closure;
        vm.frames[0].closure = std::ptr::null_mut();

        let result =
            vm.try_diagnostic_for_fault(crate::vm::VmFault::new("fault", 0), document.eof_span());

        vm.frames[0].closure = original;
        vm.cleanup_execution();
        assert!(result.is_err());
    }

    #[test]
    fn object_values_require_exact_registered_allocation_bases() {
        let mut vm = crate::vm::VM::new();
        let entry = first_live_global(&mut vm);
        let original = unsafe { (*entry).value };

        unsafe { (*entry).value = Value::Obj(std::ptr::null_mut()) };
        let null_result = vm.build_snapshot(
            SnapshotRequest {
                reason: SnapshotReason::Faulted,
                active_offset: None,
                current_span: span(),
            },
            &SnapshotLimits::default(),
        );
        assert_eq!(null_result, Err(SnapshotBuildError::ObjectState));

        let alien = Box::into_raw(Box::new(Obj {
            obj_type: ObjType::List,
            is_marked: false,
            next: std::ptr::null_mut(),
        }));
        unsafe { (*entry).value = Value::Obj(alien) };
        let alien_result = vm.build_snapshot(
            SnapshotRequest {
                reason: SnapshotReason::Faulted,
                active_offset: None,
                current_span: span(),
            },
            &SnapshotLimits::default(),
        );
        unsafe {
            (*entry).value = original;
            drop(Box::from_raw(alien));
        }
        assert_eq!(alien_result, Err(SnapshotBuildError::ObjectState));
        assert!(!format!("{alien_result:?}").contains("0x"));
    }

    #[test]
    fn frame_kind_and_stack_base_are_validated_before_use() {
        let (mut vm, document) = prepared_vm("print 1;");
        let request = active_request(&vm, document.eof_span());
        let entry = first_live_global(&mut vm);
        let native = match unsafe { (*entry).value } {
            Value::Obj(object) => object,
            _ => panic!("clock is an object"),
        };
        let original_closure = vm.frames[0].closure;
        vm.frames[0].closure = native as *mut ObjClosure;
        assert_eq!(
            vm.build_snapshot(request, &SnapshotLimits::default()),
            Err(SnapshotBuildError::FrameState)
        );
        vm.frames[0].closure = original_closure;

        let original_slots = vm.frames[0].slots;
        vm.frames[0].slots = (vm.stack.as_mut_ptr() as usize + 1) as *mut Value;
        assert_eq!(
            vm.build_snapshot(request, &SnapshotLimits::default()),
            Err(SnapshotBuildError::StackState)
        );
        vm.frames[0].slots = original_slots;
        vm.cleanup_execution();
    }

    #[test]
    fn active_liveness_offset_must_be_an_opcode_start() {
        let (mut vm, document) = prepared_vm("print 1;");
        let request = active_request(&vm, document.eof_span());
        let function = unsafe { (*vm.frames[0].closure).function };
        assert!(unsafe { (*function).chunk.code.len() } > 1);
        assert!(!crate::vm::is_opcode_start(&vm, function, 1));

        let result = vm.build_snapshot(
            SnapshotRequest {
                active_offset: Some(1),
                ..request
            },
            &SnapshotLimits::default(),
        );

        assert_eq!(result, Err(SnapshotBuildError::FrameState));
        vm.cleanup_execution();
    }

    #[test]
    fn registered_string_length_is_authoritative_for_snapshot_reads() {
        let mut vm = crate::vm::VM::new();
        let entry = first_live_global(&mut vm);
        let key = unsafe { (*entry).key };
        let original_length = unsafe { (*key).length };
        unsafe { (*key).length = original_length + 1 };
        let result = vm.build_snapshot(
            SnapshotRequest {
                reason: SnapshotReason::Faulted,
                active_offset: None,
                current_span: span(),
            },
            &SnapshotLimits::default(),
        );
        unsafe { (*key).length = original_length };
        assert_eq!(result, Err(SnapshotBuildError::ObjectState));
    }

    #[test]
    fn alien_open_upvalue_locations_and_metadata_indexes_are_rejected() {
        let source = "fun outer() {\n  var value = 1;\n  fun inner() {\n    print value;\n  }\n  inner();\n}\nouter();";
        let (mut vm, document) = prepared_vm(source);
        let mut host = crate::RecordingHost::default();
        for _ in 0..128 {
            if vm.frame_count == 3
                && vm
                    .current_semantic_point()
                    .unwrap()
                    .is_some_and(|(_, point)| point.span.start.line == 4)
            {
                break;
            }
            match vm.dispatch_one(&mut host) {
                Ok(crate::vm::DispatchResult::Continue) => {}
                other => panic!("unexpected dispatch result: {other:?}"),
            }
        }
        assert_eq!(vm.frame_count, 3);
        let request = active_request(&vm, document.eof_span());
        let closure = vm.frames[2].closure;
        let cell = unsafe { (*closure).upvalues[0] };
        let original_location = unsafe { (*cell).location };
        unsafe { (*cell).location = std::ptr::NonNull::<Value>::dangling().as_ptr() };
        assert_eq!(
            vm.build_snapshot(request, &SnapshotLimits::default()),
            Err(SnapshotBuildError::UpvalueState)
        );
        unsafe { (*cell).location = original_location };

        let function = unsafe { (*closure).function };
        let original_index = unsafe { (&(*function).debug_info.upvalues)[0].index };
        unsafe { (&mut (*function).debug_info.upvalues)[0].index = u8::MAX };
        assert_eq!(
            vm.build_snapshot(request, &SnapshotLimits::default()),
            Err(SnapshotBuildError::UpvalueState)
        );
        unsafe { (&mut (*function).debug_info.upvalues)[0].index = original_index };
        vm.cleanup_execution();
    }

    #[test]
    fn repeated_builds_restart_ids_and_preserve_the_same_snapshot() {
        let source = "var leaf = [1];\nvar graph = [leaf, leaf];\nprint 0;";
        let (mut vm, document) = prepared_vm(source);
        let mut host = crate::RecordingHost::default();
        for _ in 0..64 {
            if vm
                .current_semantic_point()
                .unwrap()
                .is_some_and(|(_, point)| point.span.start.line == 3)
            {
                break;
            }
            assert_eq!(
                vm.dispatch_one(&mut host).unwrap(),
                crate::vm::DispatchResult::Continue
            );
        }
        let request = active_request(&vm, document.eof_span());

        let first = vm
            .build_snapshot(request, &SnapshotLimits::default())
            .unwrap();
        let second = vm
            .build_snapshot(request, &SnapshotLimits::default())
            .unwrap();

        assert_eq!(first, second);
        assert!(matches!(
            first
                .globals
                .iter()
                .find(|value| value.name == "graph")
                .unwrap()
                .value,
            DebugValue::List { object_id: 1, .. }
        ));
        vm.cleanup_execution();
    }

    #[test]
    fn global_table_history_does_not_change_sorted_values_or_ids() {
        fn install_graph(vm: &mut crate::vm::VM, order: &[&str]) {
            let a = crate::object::allocate_list(vm, vec![Value::Number(1.0)]);
            let z = crate::object::allocate_list(vm, vec![Value::Number(2.0)]);
            let root = crate::object::allocate_list(
                vm,
                vec![
                    Value::Obj(a as *mut Obj),
                    Value::Obj(z as *mut Obj),
                    Value::Obj(a as *mut Obj),
                ],
            );
            for name in order {
                let value = match *name {
                    "a" => Value::Obj(a as *mut Obj),
                    "root" => Value::Obj(root as *mut Obj),
                    "z" => Value::Obj(z as *mut Obj),
                    _ => unreachable!(),
                };
                let key = crate::object::copy_string(vm, name);
                vm.globals.set(key, value);
            }
        }

        let mut first = crate::vm::VM::new();
        install_graph(&mut first, &["z", "a", "root"]);
        let mut temporary_keys = Vec::new();
        for index in 0..16 {
            let key = crate::object::copy_string(&mut first, &format!("temporary{index:02}"));
            first.globals.set(key, Value::Nil);
            temporary_keys.push(key);
        }
        for key in temporary_keys {
            assert!(first.globals.delete(key));
        }

        let mut second = crate::vm::VM::new();
        install_graph(&mut second, &["root", "a", "z"]);
        assert_ne!(first.globals.capacity, second.globals.capacity);
        let request = SnapshotRequest {
            reason: SnapshotReason::Faulted,
            active_offset: None,
            current_span: span(),
        };

        let first = first
            .build_snapshot(request, &SnapshotLimits::default())
            .unwrap();
        let second = second
            .build_snapshot(request, &SnapshotLimits::default())
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first
                .globals
                .iter()
                .map(|binding| binding.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "clock", "root", "z"]
        );
        assert!(matches!(
            first.globals[0].value,
            DebugValue::List { object_id: 1, .. }
        ));
        assert!(matches!(
            first.globals[2].value,
            DebugValue::List { object_id: 2, .. }
        ));
        assert!(matches!(
            first.globals[3].value,
            DebugValue::List { object_id: 3, .. }
        ));
    }

    #[test]
    fn aggregate_node_exhaustion_omits_an_upvalue_without_reading_its_cell() {
        let source = "fun outer() {\n  var captured = 1;\n  fun inner(p) {\n    print captured;\n  }\n  inner(2);\n}\nouter();";
        let (mut vm, document) = prepared_vm(source);
        let mut host = crate::RecordingHost::default();
        for _ in 0..128 {
            if vm.frame_count == 3
                && vm
                    .current_semantic_point()
                    .unwrap()
                    .is_some_and(|(_, point)| point.span.start.line == 4)
            {
                break;
            }
            assert_eq!(
                vm.dispatch_one(&mut host).unwrap(),
                crate::vm::DispatchResult::Continue
            );
        }
        let closure = vm.frames[2].closure;
        let cell = unsafe { (*closure).upvalues[0] };
        unsafe { (*closure).upvalues[0] = std::ptr::null_mut() };
        let mut limits = SnapshotLimits::default();
        limits.max_value_nodes = 1;

        let snapshot = vm
            .build_snapshot(active_request(&vm, document.eof_span()), &limits)
            .unwrap();

        assert_eq!(snapshot.frames[0].parameters.len(), 1);
        assert!(snapshot.frames[0].upvalues.is_empty());
        assert!(snapshot.frames[0].upvalues_truncated);
        assert!(snapshot.frames_truncated);
        unsafe { (*closure).upvalues[0] = cell };
        vm.cleanup_execution();
    }

    #[test]
    fn snapshot_uses_canonical_registry_and_table_allocation_pointers() {
        let mut vm = crate::vm::VM::new();
        let entry = first_live_global(&mut vm);
        let original_value = unsafe { (*entry).value };
        let Value::Obj(object) = original_value else {
            panic!("clock is an object")
        };
        let original_entries = vm.globals.entries;
        vm.globals.entries = std::ptr::without_provenance_mut(original_entries.addr());
        unsafe {
            (*entry).value = Value::Obj(std::ptr::without_provenance_mut(object.addr()));
        }

        let snapshot = vm
            .build_snapshot(
                SnapshotRequest {
                    reason: SnapshotReason::Faulted,
                    active_offset: None,
                    current_span: span(),
                },
                &SnapshotLimits::default(),
            )
            .unwrap();

        vm.globals.entries = original_entries;
        unsafe { (*entry).value = original_value };
        assert_eq!(snapshot.globals[0].name, "clock");
        assert_eq!(
            snapshot.globals[0].value,
            DebugValue::Native("clock".into())
        );
    }
}
