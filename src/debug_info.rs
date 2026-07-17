use crate::{RevisionId, SourceId, SourceSpan, TextPosition};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DebugPointId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugPointKind {
    FunctionEntry,
    Statement,
    LoopInitializer,
    LoopCondition,
    LoopIncrement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugPoint {
    pub id: DebugPointId,
    pub offset: usize,
    pub kind: DebugPointKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BindingId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    Parameter,
    Local,
    Implicit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingDebugInfo {
    pub id: BindingId,
    pub name: String,
    pub kind: BindingKind,
    pub slot: u16,
    pub scope_depth: i32,
    pub declaration: SourceSpan,
    pub live_start: usize,
    pub live_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpvalueDebugInfo {
    pub binding_id: BindingId,
    pub name: String,
    pub index: u8,
    pub declaration: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDebugInfo {
    pub source_id: SourceId,
    pub revision: RevisionId,
    pub declaration: SourceSpan,
    pub points: Vec<DebugPoint>,
    pub bindings: Vec<BindingDebugInfo>,
    pub upvalues: Vec<UpvalueDebugInfo>,
}

impl Default for FunctionDebugInfo {
    fn default() -> Self {
        let position = TextPosition {
            byte_offset: 0,
            line: 1,
            column: 1,
        };
        Self {
            source_id: SourceId(0),
            revision: RevisionId(0),
            declaration: SourceSpan {
                source_id: SourceId(0),
                revision: RevisionId(0),
                start: position,
                end: position,
            },
            points: Vec::new(),
            bindings: Vec::new(),
            upvalues: Vec::new(),
        }
    }
}
