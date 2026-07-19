use rlox::{
    ActivationId, BindingId, BindingSnapshot, DebugPointId, DebugValue, Diagnostic,
    DiagnosticPhase, DiagnosticSeverity, FrameSnapshot, MAX_SNAPSHOT_JSON_BYTES, PauseLocation,
    PauseReason, RevisionId, RuntimeFrame, SnapshotReason, SourceId, SourceSpan, TextPosition,
    ValueKind, VmSnapshot,
};
use serde::{Deserialize, Serialize};

fn span() -> SourceSpan {
    SourceSpan {
        source_id: SourceId(7),
        revision: RevisionId(11),
        start: TextPosition {
            byte_offset: 12,
            line: 3,
            column: 5,
        },
        end: TextPosition {
            byte_offset: 18,
            line: 3,
            column: 11,
        },
    }
}

fn assert_rejects<T>(json: &str)
where
    T: for<'de> Deserialize<'de>,
{
    assert!(
        serde_json::from_str::<T>(json).is_err(),
        "unexpectedly accepted {json}"
    );
}

fn round_trip<T>(value: &T)
where
    T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).unwrap();
    assert_eq!(serde_json::from_str::<T>(&json).unwrap(), *value);
}

#[test]
fn identifiers_and_source_spans_have_canonical_wire_shapes() {
    assert_eq!(serde_json::to_string(&SourceId(7)).unwrap(), "7");
    assert_eq!(serde_json::to_string(&RevisionId(11)).unwrap(), "11");
    assert_eq!(serde_json::to_string(&DebugPointId(13)).unwrap(), "13");
    assert_eq!(serde_json::to_string(&BindingId(17)).unwrap(), "17");
    assert_eq!(serde_json::to_string(&ActivationId(19)).unwrap(), "19");

    let source_span = span();
    let json = serde_json::to_string(&source_span).unwrap();
    assert_eq!(
        json,
        r#"{"source_id":7,"revision":11,"start":{"byte_offset":12,"line":3,"column":5},"end":{"byte_offset":18,"line":3,"column":11}}"#
    );
    round_trip(&source_span);

    let reordered = r#"{
        "end":{"column":11,"line":3,"byte_offset":18},
        "revision":11,
        "source_id":7,
        "start":{"line":3,"byte_offset":12,"column":5}
    }"#;
    assert_eq!(
        serde_json::from_str::<SourceSpan>(reordered).unwrap(),
        source_span
    );
}

#[test]
fn source_contract_rejects_unknown_duplicate_and_non_u64_positions() {
    assert_rejects::<TextPosition>(r#"{"byte_offset":0,"line":1,"column":1,"extra":0}"#);
    assert_rejects::<TextPosition>(r#"{"byte_offset":0,"byte_offset":1,"line":1,"column":1}"#);
    assert_rejects::<TextPosition>(r#"{"byte_offset":-1,"line":1,"column":1}"#);
    assert_rejects::<TextPosition>(r#"{"byte_offset":1.0,"line":1,"column":1}"#);
    assert_rejects::<SourceSpan>(
        r#"{
        "source_id":7,"revision":11,
        "start":{"byte_offset":0,"line":1,"column":1},
        "end":{"byte_offset":0,"line":1,"column":1},
        "extra":false
    }"#,
    );
    assert_rejects::<SourceSpan>(
        r#"{
        "source_id":7,"source_id":8,"revision":11,
        "start":{"byte_offset":0,"line":1,"column":1},
        "end":{"byte_offset":0,"line":1,"column":1}
    }"#,
    );
}

#[cfg(target_pointer_width = "64")]
#[test]
fn source_positions_preserve_the_full_u64_domain() {
    let position = TextPosition {
        byte_offset: usize::MAX,
        line: usize::MAX,
        column: usize::MAX,
    };
    let expected = r#"{"byte_offset":18446744073709551615,"line":18446744073709551615,"column":18446744073709551615}"#;
    assert_eq!(serde_json::to_string(&position).unwrap(), expected);
    assert_eq!(
        serde_json::from_str::<TextPosition>(expected).unwrap(),
        position
    );
}

#[cfg(any(target_pointer_width = "16", target_pointer_width = "32"))]
#[test]
fn source_positions_reject_values_that_do_not_fit_usize() {
    let too_large = usize::MAX as u64 + 1;
    let json = format!(r#"{{"byte_offset":{too_large},"line":1,"column":1}}"#);
    assert_rejects::<TextPosition>(&json);
}

#[test]
fn diagnostics_and_pause_locations_round_trip_strictly() {
    let runtime_frame = RuntimeFrame {
        function: "main".to_string(),
        span: span(),
    };
    let diagnostic = Diagnostic {
        phase: DiagnosticPhase::Runtime,
        severity: DiagnosticSeverity::Error,
        code: "runtime.type".to_string(),
        message: "Operands must be numbers.".to_string(),
        span: span(),
        frames: vec![runtime_frame],
    };
    let json = serde_json::to_string(&diagnostic).unwrap();
    assert!(json.starts_with(r#"{"phase":"runtime","severity":"error""#));
    round_trip(&diagnostic);

    let pause = PauseLocation {
        source_id: SourceId(7),
        revision: RevisionId(11),
        span: span(),
        debug_point_id: DebugPointId(13),
        activation_id: ActivationId(19),
        dynamic_event: 23,
    };
    round_trip(&pause);
    assert_rejects::<PauseLocation>(&format!(
        r#"{{"source_id":7,"revision":11,"span":{},"debug_point_id":13,"activation_id":19,"dynamic_event":23,"extra":0}}"#,
        serde_json::to_string(&span()).unwrap()
    ));

    assert_eq!(
        serde_json::to_string(&PauseReason::DebugPoint).unwrap(),
        r#""debug_point""#
    );
    assert_eq!(
        serde_json::from_str::<PauseReason>(r#""explicit""#).unwrap(),
        PauseReason::Explicit
    );
}

#[test]
fn diagnostics_reject_unknown_duplicate_and_unknown_enum_values() {
    let span = serde_json::to_string(&span()).unwrap();
    assert_rejects::<RuntimeFrame>(&format!(r#"{{"function":"main","span":{span},"extra":0}}"#));
    assert_rejects::<Diagnostic>(&format!(
        r#"{{"phase":"runtime","severity":"error","code":"x","code":"y","message":"m","span":{span},"frames":[]}}"#
    ));
    assert_rejects::<Diagnostic>(&format!(
        r#"{{"phase":"other","severity":"error","code":"x","message":"m","span":{span},"frames":[]}}"#
    ));
}

#[test]
fn diagnostic_enums_and_nonfinite_numbers_have_stable_wire_names() {
    let phases = [
        (DiagnosticPhase::Scanner, r#""scanner""#),
        (DiagnosticPhase::Parser, r#""parser""#),
        (DiagnosticPhase::Compiler, r#""compiler""#),
        (DiagnosticPhase::Runtime, r#""runtime""#),
        (DiagnosticPhase::Worker, r#""worker""#),
    ];
    for (value, expected) in phases {
        assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        assert_eq!(
            serde_json::from_str::<DiagnosticPhase>(expected).unwrap(),
            value
        );
    }

    for (value, expected) in [
        (DiagnosticSeverity::Error, r#""error""#),
        (DiagnosticSeverity::Warning, r#""warning""#),
    ] {
        assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        assert_eq!(
            serde_json::from_str::<DiagnosticSeverity>(expected).unwrap(),
            value
        );
    }

    for value in ["nan", "infinity", "-infinity", "-0"] {
        let debug_value = DebugValue::Number(value.to_string());
        let expected = format!(r#"{{"kind":"number","payload":"{value}"}}"#);
        assert_eq!(serde_json::to_string(&debug_value).unwrap(), expected);
        assert_eq!(
            serde_json::from_str::<DebugValue>(&expected).unwrap(),
            debug_value
        );
    }
}

#[test]
fn snapshot_reason_has_exact_strict_adjacent_tagging() {
    let cases = [
        (
            SnapshotReason::Paused(PauseReason::DebugPoint),
            r#"{"kind":"paused","payload":"debug_point"}"#,
        ),
        (SnapshotReason::Faulted, r#"{"kind":"faulted"}"#),
        (SnapshotReason::Cancelled, r#"{"kind":"cancelled"}"#),
    ];

    for (value, expected) in cases {
        assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        assert_eq!(
            serde_json::from_str::<SnapshotReason>(expected).unwrap(),
            value
        );
    }

    assert_eq!(
        serde_json::from_str::<SnapshotReason>(r#"{"payload":"step","kind":"paused"}"#).unwrap(),
        SnapshotReason::Paused(PauseReason::Step)
    );

    for json in [
        r#"{"kind":"faulted","payload":null}"#,
        r#"{"kind":"faulted","payload":{}}"#,
        r#"{"kind":"cancelled","payload":null}"#,
        r#"{"kind":"paused"}"#,
        r#"{"kind":"paused","payload":null}"#,
        r#"{"kind":"paused","payload":"step","extra":0}"#,
        r#"{"kind":"paused","kind":"faulted","payload":"step"}"#,
        r#"{"kind":"unknown"}"#,
    ] {
        assert_rejects::<SnapshotReason>(json);
    }
}

#[test]
fn debug_values_have_exact_strict_adjacent_tagging() {
    let cases = vec![
        (DebugValue::Nil, r#"{"kind":"nil"}"#.to_string()),
        (
            DebugValue::Bool(true),
            r#"{"kind":"bool","payload":true}"#.to_string(),
        ),
        (
            DebugValue::Number("1.25".to_string()),
            r#"{"kind":"number","payload":"1.25"}"#.to_string(),
        ),
        (
            DebugValue::String("hi".to_string()),
            r#"{"kind":"string","payload":"hi"}"#.to_string(),
        ),
        (
            DebugValue::Function("f".to_string()),
            r#"{"kind":"function","payload":"f"}"#.to_string(),
        ),
        (
            DebugValue::Closure("c".to_string()),
            r#"{"kind":"closure","payload":"c"}"#.to_string(),
        ),
        (
            DebugValue::Native("clock".to_string()),
            r#"{"kind":"native","payload":"clock"}"#.to_string(),
        ),
        (
            DebugValue::List {
                object_id: 1,
                items: vec![DebugValue::Nil, DebugValue::Bool(false)],
                truncated: true,
            },
            r#"{"kind":"list","payload":{"object_id":1,"items":[{"kind":"nil"},{"kind":"bool","payload":false}],"truncated":true}}"#.to_string(),
        ),
        (
            DebugValue::Cycle { object_id: 1 },
            r#"{"kind":"cycle","payload":{"object_id":1}}"#.to_string(),
        ),
        (
            DebugValue::Truncated,
            r#"{"kind":"truncated"}"#.to_string(),
        ),
    ];

    for (value, expected) in cases {
        assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        assert_eq!(
            serde_json::from_str::<DebugValue>(&expected).unwrap(),
            value
        );
    }

    assert_eq!(
        serde_json::from_str::<DebugValue>(r#"{"payload":false,"kind":"bool"}"#).unwrap(),
        DebugValue::Bool(false)
    );
}

#[test]
fn debug_values_reject_noncanonical_payloads() {
    for json in [
        r#"{"kind":"nil","payload":null}"#,
        r#"{"kind":"truncated","payload":null}"#,
        r#"{"kind":"bool"}"#,
        r#"{"kind":"bool","payload":null}"#,
        r#"{"kind":"bool","payload":"false"}"#,
        r#"{"kind":"number","payload":1}"#,
        r#"{"kind":"string","payload":"x","extra":0}"#,
        r#"{"kind":"list","payload":{"object_id":1,"items":[],"truncated":false,"extra":0}}"#,
        r#"{"kind":"list","payload":{"object_id":1,"object_id":2,"items":[],"truncated":false}}"#,
        r#"{"kind":"cycle","payload":{"object_id":1,"extra":0}}"#,
        r#"{"kind":"cycle","payload":{"object_id":1,"object_id":2}}"#,
        r#"{"kind":"cycle","payload":{"object_id":1},"payload":{"object_id":2}}"#,
        r#"{"kind":"unknown"}"#,
    ] {
        assert_rejects::<DebugValue>(json);
    }
}

#[test]
fn snapshot_records_round_trip_and_reject_unknown_fields() {
    assert_eq!(MAX_SNAPSHOT_JSON_BYTES, 5 * 1_048_576);

    let binding = BindingSnapshot {
        binding_id: Some(BindingId(17)),
        name: "answer".to_string(),
        name_truncated: false,
        binding_kind: "local".to_string(),
        value_kind: ValueKind::Number,
        value: DebugValue::Number("42".to_string()),
    };
    let frame = FrameSnapshot {
        activation_id: ActivationId(19),
        function: "main".to_string(),
        function_truncated: false,
        current_span: span(),
        call_site: None,
        parameters: Vec::new(),
        parameters_truncated: false,
        locals: vec![binding.clone()],
        locals_truncated: false,
        upvalues: Vec::new(),
        upvalues_truncated: false,
    };
    let snapshot = VmSnapshot {
        reason: SnapshotReason::Paused(PauseReason::Step),
        current_span: span(),
        frames: vec![frame],
        frames_truncated: false,
        globals: vec![binding],
        globals_truncated: false,
    };

    round_trip(&snapshot);
    let actual = serde_json::to_vec(&snapshot).unwrap().len();
    assert!(actual <= snapshot.conservative_json_size().unwrap());

    let mut json = serde_json::to_value(&snapshot).unwrap();
    json.as_object_mut()
        .unwrap()
        .insert("extra".to_string(), serde_json::Value::Bool(false));
    assert!(serde_json::from_value::<VmSnapshot>(json).is_err());

    assert_eq!(
        serde_json::to_string(&ValueKind::Cycle).unwrap(),
        r#""cycle""#
    );
    assert_rejects::<ValueKind>(r#""other""#);
}

#[test]
fn snapshot_estimator_covers_every_schema_variant_and_exact_cap_edges() {
    let escaped = "\0\"\\é😀\u{007f}".to_string();
    let values = vec![
        (ValueKind::Nil, DebugValue::Nil),
        (ValueKind::Bool, DebugValue::Bool(true)),
        (ValueKind::Number, DebugValue::Number(escaped.clone())),
        (ValueKind::String, DebugValue::String(escaped.clone())),
        (ValueKind::Function, DebugValue::Function(escaped.clone())),
        (ValueKind::Closure, DebugValue::Closure(escaped.clone())),
        (ValueKind::Native, DebugValue::Native(escaped.clone())),
        (
            ValueKind::List,
            DebugValue::List {
                object_id: u64::MAX,
                items: vec![DebugValue::Bool(false), DebugValue::String(escaped)],
                truncated: true,
            },
        ),
        (
            ValueKind::Cycle,
            DebugValue::Cycle {
                object_id: u64::MAX,
            },
        ),
        (ValueKind::Truncated, DebugValue::Truncated),
    ];
    let bindings = values
        .into_iter()
        .enumerate()
        .map(|(index, (value_kind, value))| BindingSnapshot {
            binding_id: (index % 2 == 0).then_some(BindingId(u64::MAX)),
            name: format!("binding-{index}"),
            name_truncated: index % 2 == 0,
            binding_kind: "local".to_string(),
            value_kind,
            value,
        })
        .collect::<Vec<_>>();

    for reason in [
        SnapshotReason::Paused(PauseReason::DebugPoint),
        SnapshotReason::Paused(PauseReason::Step),
        SnapshotReason::Paused(PauseReason::Explicit),
        SnapshotReason::Faulted,
        SnapshotReason::Cancelled,
    ] {
        for call_site in [None, Some(span())] {
            let snapshot = VmSnapshot {
                reason,
                current_span: span(),
                frames: vec![FrameSnapshot {
                    activation_id: ActivationId(u64::MAX),
                    function: "\0\"\\é😀".to_string(),
                    function_truncated: true,
                    current_span: span(),
                    call_site,
                    parameters: bindings.clone(),
                    parameters_truncated: true,
                    locals: bindings.clone(),
                    locals_truncated: true,
                    upvalues: bindings.clone(),
                    upvalues_truncated: true,
                }],
                frames_truncated: true,
                globals: bindings.clone(),
                globals_truncated: true,
            };
            let estimate = snapshot.conservative_json_size().unwrap();
            let actual = serde_json::to_vec(&snapshot).unwrap().len();
            assert!(actual <= estimate, "actual={actual}, estimate={estimate}");
        }
    }

    for target in [
        MAX_SNAPSHOT_JSON_BYTES - 1,
        MAX_SNAPSHOT_JSON_BYTES,
        MAX_SNAPSHOT_JSON_BYTES + 1,
    ] {
        let mut snapshot = VmSnapshot {
            reason: SnapshotReason::Faulted,
            current_span: span(),
            frames: Vec::new(),
            frames_truncated: true,
            globals: vec![BindingSnapshot {
                binding_id: None,
                name: "boundary".to_string(),
                name_truncated: false,
                binding_kind: "global".to_string(),
                value_kind: ValueKind::String,
                value: DebugValue::String(String::new()),
            }],
            globals_truncated: true,
        };
        let base = snapshot.conservative_json_size().unwrap();
        let DebugValue::String(payload) = &mut snapshot.globals[0].value else {
            unreachable!()
        };
        *payload = "x".repeat(target - base);

        assert_eq!(snapshot.conservative_json_size().unwrap(), target);
        assert!(serde_json::to_vec(&snapshot).unwrap().len() <= target);
    }
}
