use rlox::{
    BindingSnapshot, DebugValue, ExecutionState, InterpreterSession, PauseReason, RecordingHost,
    ResumeMode, RevisionId, RunOutcome, SnapshotLimitError, SnapshotLimitField, SnapshotLimits,
    SnapshotReason, SourceDocument, SourceId, ValueKind, VmSnapshot,
};

fn session(source: &str) -> InterpreterSession<RecordingHost> {
    InterpreterSession::new(
        SourceDocument::new(SourceId(80), RevisionId(3), "snapshot.lox", source),
        RecordingHost::default(),
    )
}

fn pause_at_line(session: &mut InterpreterSession<RecordingHost>, line: usize) -> VmSnapshot {
    loop {
        let location = *session.pause_location().expect("session is paused");
        if location.span.start.line == line {
            return session.snapshot().expect("paused snapshot").clone();
        }
        assert!(matches!(
            session.resume(ResumeMode::StepInto),
            RunOutcome::Paused(_)
        ));
    }
}

fn binding<'a>(bindings: &'a [BindingSnapshot], name: &str) -> &'a BindingSnapshot {
    bindings
        .iter()
        .find(|binding| binding.name == name)
        .unwrap_or_else(|| panic!("missing binding {name}: {bindings:#?}"))
}

fn global<'a>(snapshot: &'a VmSnapshot, name: &str) -> &'a BindingSnapshot {
    binding(&snapshot.globals, name)
}

#[test]
fn paused_snapshot_owns_frames_spans_bindings_and_sorted_globals() {
    let source = "fun inner(p) {\n  var local = p + 1;\n  print local;\n}\nfun outer() {\n  var kept = 40;\n  inner(kept);\n}\nouter();";
    let mut session = session(source);

    assert!(matches!(
        session.start_debugging(),
        RunOutcome::Paused(PauseReason::DebugPoint)
    ));
    let snapshot = pause_at_line(&mut session, 3);
    let location = *session.pause_location().unwrap();

    assert_eq!(snapshot.reason, SnapshotReason::Paused(PauseReason::Step));
    assert_eq!(snapshot.current_span, location.span);
    assert!(!snapshot.frames_truncated);
    assert_eq!(
        snapshot
            .frames
            .iter()
            .map(|frame| frame.function.as_str())
            .collect::<Vec<_>>(),
        ["inner", "outer", "<script>"]
    );
    let mut activations = snapshot
        .frames
        .iter()
        .map(|frame| frame.activation_id)
        .collect::<Vec<_>>();
    activations.sort_by_key(|id| id.0);
    activations.dedup();
    assert_eq!(activations.len(), 3);

    let inner = &snapshot.frames[0];
    assert_eq!(inner.current_span, snapshot.current_span);
    assert_eq!(inner.call_site.unwrap().start.line, 7);
    assert_eq!(
        binding(&inner.parameters, "p").value,
        DebugValue::Number("40".into())
    );
    assert_eq!(binding(&inner.parameters, "p").binding_kind, "parameter");
    assert_eq!(
        binding(&inner.locals, "local").value,
        DebugValue::Number("41".into())
    );
    assert_eq!(binding(&inner.locals, "local").binding_kind, "local");

    let outer = &snapshot.frames[1];
    assert_eq!(outer.current_span, inner.call_site.unwrap());
    assert_eq!(outer.call_site.unwrap().start.line, 9);
    assert_eq!(
        binding(&outer.locals, "kept").value,
        DebugValue::Number("40".into())
    );
    assert_eq!(snapshot.frames[2].current_span, outer.call_site.unwrap());
    assert!(snapshot.frames[2].call_site.is_none());

    assert_eq!(
        snapshot
            .globals
            .iter()
            .map(|binding| binding.name.as_str())
            .collect::<Vec<_>>(),
        ["clock", "inner", "outer"]
    );
    assert_eq!(
        global(&snapshot, "clock").value,
        DebugValue::Native("clock".into())
    );
    assert_eq!(global(&snapshot, "clock").value_kind, ValueKind::Native);
    assert_eq!(global(&snapshot, "inner").value_kind, ValueKind::Closure);
    assert_eq!(global(&snapshot, "outer").binding_kind, "global");
}

#[test]
fn stepped_function_snapshot_has_protocol_safe_shape_and_size() {
    let mut session =
        session("fun add(a, b) {\n  var total = a + b;\n  print total;\n}\n\nadd(2, 3);\n");
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    assert!(matches!(
        session.resume(ResumeMode::StepOver),
        RunOutcome::Paused(PauseReason::Step)
    ));
    assert!(matches!(
        session.resume(ResumeMode::StepInto),
        RunOutcome::Paused(PauseReason::Step)
    ));

    let location = *session.pause_location().expect("function pause location");
    let snapshot = session.snapshot().expect("function pause snapshot");
    assert_eq!(snapshot.current_span, location.span);
    assert_eq!(snapshot.frames[0].activation_id, location.activation_id);
    assert_eq!(snapshot.frames[0].function, "add");
    assert_eq!(snapshot.frames[0].parameters[0].binding_id.unwrap().0, 0);
    let estimated = snapshot
        .conservative_json_size()
        .expect("snapshot size estimate");
    let encoded = serde_json::to_vec(snapshot).expect("serialize snapshot");
    assert!(
        encoded.len() <= estimated,
        "encoded snapshot uses {} bytes but estimate is {estimated}",
        encoded.len()
    );
}

#[test]
fn suspended_caller_uses_the_child_call_site_for_liveness() {
    let source = "fun inner() {\n  print 0;\n}\nfun outer() {\n  {\n    var keep = 42;\n    inner();\n  }\n}\nouter();";
    let mut session = session(source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));

    let snapshot = pause_at_line(&mut session, 2);
    let outer = &snapshot.frames[1];
    assert_eq!(outer.function, "outer");
    assert_eq!(outer.current_span.start.line, 7);
    assert_eq!(
        binding(&outer.locals, "keep").value,
        DebugValue::Number("42".into())
    );
}

#[test]
fn open_upvalues_are_read_from_the_shared_cell() {
    let source = "fun outer() {\n  var captured = 41;\n  fun inner(p) {\n    print captured + p;\n  }\n  inner(1);\n}\nouter();";
    let mut session = session(source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));

    let snapshot = pause_at_line(&mut session, 4);
    let inner_capture = binding(&snapshot.frames[0].upvalues, "captured");
    let outer_local = binding(&snapshot.frames[1].locals, "captured");
    assert_eq!(inner_capture.binding_kind, "upvalue");
    assert_eq!(inner_capture.binding_id, outer_local.binding_id);
    assert_eq!(inner_capture.value, DebugValue::Number("41".into()));
    assert_eq!(inner_capture.value, outer_local.value);
}

#[test]
fn old_snapshots_remain_owned_as_a_closed_capture_changes() {
    let source = "fun makeCounter() {\n  var i = 0;\n  fun count() {\n    print i;\n    i = i + 1;\n  }\n  return count;\n}\nvar counter = makeCounter();\ncounter();\ncounter();";
    let mut session = session(source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let first = pause_at_line(&mut session, 4);
    assert_eq!(
        binding(&first.frames[0].upvalues, "i").value,
        DebugValue::Number("0".into())
    );

    assert!(matches!(
        session.resume(ResumeMode::StepInto),
        RunOutcome::Paused(_)
    ));
    loop {
        let current = session.snapshot().unwrap();
        if current.current_span.start.line == 4
            && binding(&current.frames[0].upvalues, "i").value == DebugValue::Number("1".into())
        {
            break;
        }
        assert!(matches!(
            session.resume(ResumeMode::StepInto),
            RunOutcome::Paused(_)
        ));
    }
    let second = session.snapshot().unwrap().clone();
    assert_eq!(
        binding(&first.frames[0].upvalues, "i").value,
        DebugValue::Number("0".into())
    );
    assert_eq!(
        binding(&second.frames[0].upvalues, "i").value,
        DebugValue::Number("1".into())
    );
}

#[test]
fn aliases_are_full_values_but_active_recursion_is_a_cycle() {
    let mut aliases = session("var a = [1];\nvar b = [a, a];\nprint b;");
    assert!(matches!(aliases.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = pause_at_line(&mut aliases, 3);
    let DebugValue::List {
        object_id: a_id, ..
    } = global(&snapshot, "a").value
    else {
        panic!("a is not a list")
    };
    let DebugValue::List { items, .. } = &global(&snapshot, "b").value else {
        panic!("b is not a list")
    };
    assert_eq!(items.len(), 2);
    for item in items {
        assert!(matches!(
            item,
            DebugValue::List { object_id, .. } if *object_id == a_id
        ));
    }

    let mut cycle = session("var a = [nil];\na[0] = a;\nprint a;");
    assert!(matches!(cycle.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = pause_at_line(&mut cycle, 3);
    let DebugValue::List {
        object_id,
        items,
        truncated: false,
    } = &global(&snapshot, "a").value
    else {
        panic!("a is not a complete list")
    };
    assert_eq!(
        items,
        &[DebugValue::Cycle {
            object_id: *object_id
        }]
    );
}

#[test]
fn fault_cancel_compile_and_completion_snapshots_have_honest_terminal_reasons() {
    let mut runtime = session("fun f() {\n  var x = 1 + true;\n}\nf();");
    let RunOutcome::Faulted(diagnostic) = runtime.run_all() else {
        panic!("expected runtime fault")
    };
    let fault = runtime.snapshot().expect("runtime fault snapshot");
    assert_eq!(fault.reason, SnapshotReason::Faulted);
    assert_eq!(fault.current_span, diagnostic.span);
    assert_eq!(fault.frames[0].function, "f");
    assert!(fault.frames[0].locals.iter().all(|local| local.name != "x"));

    let mut compile = session("var = 1;");
    let RunOutcome::Faulted(diagnostic) = compile.run_all() else {
        panic!("expected compile fault")
    };
    let fault = compile.snapshot().expect("compile fault snapshot");
    assert_eq!(fault.reason, SnapshotReason::Faulted);
    assert_eq!(fault.current_span, diagnostic.span);
    assert!(fault.frames.is_empty());
    assert_eq!(
        fault
            .globals
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        ["clock"]
    );

    let mut cancelled = session("print 1;");
    assert!(matches!(cancelled.start_debugging(), RunOutcome::Paused(_)));
    cancelled.control().request_cancel();
    assert!(matches!(
        cancelled.resume(ResumeMode::Continue),
        RunOutcome::Cancelled
    ));
    let frozen = cancelled.snapshot().expect("cancel snapshot").clone();
    assert_eq!(frozen.reason, SnapshotReason::Cancelled);
    assert!(!frozen.frames.is_empty());
    assert_eq!(cancelled.snapshot(), Some(&frozen));

    let mut completed = session("print 1;");
    assert!(matches!(completed.run_all(), RunOutcome::Completed));
    assert_eq!(completed.execution_state(), ExecutionState::Completed);
    assert!(completed.snapshot().is_none());
}

#[test]
fn rejected_operations_preserve_the_latest_snapshot() {
    let mut session = session("print 1;");
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let before = session.snapshot().unwrap().clone();
    assert!(matches!(session.start_debugging(), RunOutcome::Rejected(_)));
    assert_eq!(session.snapshot(), Some(&before));
}

#[test]
fn public_limits_reject_extremes_and_bound_compound_values() {
    let defaults = SnapshotLimits::default();
    assert_eq!(defaults.max_depth, 4);
    assert_eq!(defaults.max_collection_items, 64);
    assert_eq!(defaults.max_string_bytes, 4_096);
    assert_eq!(defaults.max_total_string_bytes, 1_048_576);
    assert_eq!(defaults.max_value_nodes, 10_000);
    assert_eq!(defaults.max_estimated_json_bytes, 5 * 1_048_576);
    defaults.clone().validate().unwrap();

    let mut extreme = defaults.clone();
    extreme.max_depth = usize::MAX;
    assert_eq!(
        extreme.validate(),
        Err(SnapshotLimitError::AboveMaximum {
            field: SnapshotLimitField::Depth,
            requested: usize::MAX,
            maximum: 16,
        })
    );

    let mut too_small = defaults.clone();
    too_small.max_estimated_json_bytes = 1_023;
    assert_eq!(
        too_small.validate(),
        Err(SnapshotLimitError::BelowMinimum {
            field: SnapshotLimitField::EstimatedJsonBytes,
            requested: 1_023,
            minimum: 1_024,
        })
    );

    let mut limits = defaults;
    limits.max_collection_items = 1;
    let document = SourceDocument::new(
        SourceId(81),
        RevisionId(1),
        "limited.lox",
        "var values = [1, 2, 3];\nprint values;",
    );
    let mut session =
        InterpreterSession::with_snapshot_limits(document, RecordingHost::default(), limits)
            .unwrap();
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = pause_at_line(&mut session, 2);
    assert!(matches!(
        global(&snapshot, "values").value,
        DebugValue::List {
            ref items,
            truncated: true,
            ..
        } if items == &[DebugValue::Number("1".into())]
    ));
    assert!(snapshot.conservative_json_size().unwrap() <= 5 * 1_048_576);
    assert!(snapshot.conservative_json_size().unwrap() < 6 * 1_048_576);
}

#[test]
fn snapshot_dtos_are_owned_thread_safe_values() {
    fn assert_owned<T: Clone + Send + Sync + 'static>() {}
    assert_owned::<VmSnapshot>();
    assert_owned::<BindingSnapshot>();
    assert_owned::<DebugValue>();
}

#[test]
fn slot_reuse_and_shadowing_keep_distinct_live_bindings() {
    let source = "fun f() {\n  {\n    var a = 1;\n    print a;\n  }\n  {\n    var a = 2;\n    print a;\n  }\n  var a = 3;\n  {\n    var a = 4;\n    print a;\n  }\n}\nf();";
    let mut session = session(source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));

    let first = pause_at_line(&mut session, 4);
    let first_a = binding(&first.frames[0].locals, "a").clone();
    assert_eq!(first.frames[0].locals.len(), 1);
    assert_eq!(first_a.value, DebugValue::Number("1".into()));

    let second = pause_at_line(&mut session, 8);
    let second_a = binding(&second.frames[0].locals, "a").clone();
    assert_eq!(second.frames[0].locals.len(), 1);
    assert_eq!(second_a.value, DebugValue::Number("2".into()));
    assert_ne!(first_a.binding_id, second_a.binding_id);

    let nested = pause_at_line(&mut session, 13);
    let visible = nested.frames[0]
        .locals
        .iter()
        .filter(|local| local.name == "a")
        .collect::<Vec<_>>();
    assert_eq!(visible.len(), 2);
    assert_eq!(visible[0].value, DebugValue::Number("3".into()));
    assert_eq!(visible[1].value, DebugValue::Number("4".into()));
    assert_ne!(visible[0].binding_id, visible[1].binding_id);
}

#[test]
fn zero_record_and_node_limits_produce_an_honestly_truncated_skeleton() {
    let limits = SnapshotLimits {
        max_depth: 0,
        max_collection_items: 0,
        max_string_bytes: 0,
        max_total_string_bytes: 0,
        max_value_nodes: 0,
        max_frames: 0,
        max_bindings_per_frame: 0,
        max_total_bindings: 0,
        max_globals: 0,
        max_estimated_json_bytes: 1_024,
    };
    let mut session = InterpreterSession::with_snapshot_limits(
        SourceDocument::new(SourceId(82), RevisionId(1), "zero.lox", "print 1;"),
        RecordingHost::default(),
        limits,
    )
    .unwrap();
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = session.snapshot().unwrap();
    assert!(snapshot.frames.is_empty());
    assert!(snapshot.frames_truncated);
    assert!(snapshot.globals.is_empty());
    assert!(snapshot.globals_truncated);
    assert!(snapshot.conservative_json_size().unwrap() <= 1_024);
}

#[test]
fn recursive_activations_are_not_deduplicated() {
    let source = "fun rec(n) {\n  if (n > 0) rec(n - 1);\n  print n;\n}\nrec(2);";
    let mut session = session(source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = pause_at_line(&mut session, 3);
    assert_eq!(
        snapshot
            .frames
            .iter()
            .map(|frame| frame.function.as_str())
            .collect::<Vec<_>>(),
        ["rec", "rec", "rec", "<script>"]
    );
    let mut ids = snapshot
        .frames
        .iter()
        .map(|frame| frame.activation_id)
        .collect::<Vec<_>>();
    ids.sort_by_key(|id| id.0);
    ids.dedup();
    assert_eq!(ids.len(), snapshot.frames.len());
    assert_eq!(
        snapshot.frames[0].parameters[0].value,
        DebugValue::Number("0".into())
    );
    assert_eq!(
        snapshot.frames[1].parameters[0].value,
        DebugValue::Number("1".into())
    );
}

#[test]
fn call_faults_do_not_invent_callee_frames() {
    let mut wrong_arity = session("fun f(a) {\n  print a;\n}\nf();");
    let RunOutcome::Faulted(diagnostic) = wrong_arity.run_all() else {
        panic!("expected wrong-arity fault")
    };
    let snapshot = wrong_arity.snapshot().unwrap();
    assert_eq!(snapshot.current_span, diagnostic.span);
    assert_eq!(
        snapshot
            .frames
            .iter()
            .map(|frame| frame.function.as_str())
            .collect::<Vec<_>>(),
        ["<script>"]
    );
    assert_eq!(snapshot.current_span.start.line, 4);

    let mut non_callable = session("var value = 1;\nvalue();");
    let RunOutcome::Faulted(diagnostic) = non_callable.run_all() else {
        panic!("expected non-callable fault")
    };
    let snapshot = non_callable.snapshot().unwrap();
    assert_eq!(snapshot.current_span, diagnostic.span);
    assert_eq!(snapshot.frames.len(), 1);
    assert_eq!(snapshot.frames[0].function, "<script>");
}

#[test]
fn numbers_use_protocol_safe_canonical_strings_and_native_names_survive_aliasing() {
    let source = "var nan = 0 / 0;\nvar negative = -1 / 0;\nvar negativeZero = -0;\nvar positive = 1 / 0;\nvar saved = clock;\nclock = 1;\nprint 0;";
    let mut session = session(source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = pause_at_line(&mut session, 7);
    assert_eq!(
        global(&snapshot, "nan").value,
        DebugValue::Number("nan".into())
    );
    assert_eq!(
        global(&snapshot, "negative").value,
        DebugValue::Number("-infinity".into())
    );
    assert_eq!(
        global(&snapshot, "negativeZero").value,
        DebugValue::Number("-0".into())
    );
    assert_eq!(
        global(&snapshot, "positive").value,
        DebugValue::Number("infinity".into())
    );
    assert_eq!(
        global(&snapshot, "clock").value,
        DebugValue::Number("1".into())
    );
    assert_eq!(
        global(&snapshot, "saved").value,
        DebugValue::Native("clock".into())
    );
}

#[test]
fn a_truncated_list_child_marks_the_container_incomplete() {
    let large = "x".repeat(4_097);
    let source = format!("var values = [\"{large}\"];\nprint values;");
    let mut session = session(&source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = pause_at_line(&mut session, 2);
    assert_eq!(
        global(&snapshot, "values").value,
        DebugValue::List {
            object_id: 1,
            items: vec![DebugValue::Truncated],
            truncated: true,
        }
    );
    assert_eq!(global(&snapshot, "values").value_kind, ValueKind::List);
}

#[test]
fn two_closures_observe_the_same_closed_capture() {
    let source = "var increment;\nvar inspect;\nfun make() {\n  var value = 0;\n  fun inc() {\n    print value;\n    value = value + 1;\n  }\n  fun read() {\n    print value;\n  }\n  increment = inc;\n  inspect = read;\n}\nmake();\nincrement();\ninspect();";
    let mut session = session(source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let before = pause_at_line(&mut session, 6);
    let before_capture = binding(&before.frames[0].upvalues, "value").clone();
    assert_eq!(before_capture.value, DebugValue::Number("0".into()));

    let after = pause_at_line(&mut session, 10);
    let after_capture = binding(&after.frames[0].upvalues, "value");
    assert_eq!(after_capture.binding_id, before_capture.binding_id);
    assert_eq!(after_capture.value, DebugValue::Number("1".into()));
    assert_eq!(before_capture.value, DebugValue::Number("0".into()));
}

#[test]
fn function_entry_keeps_parameters_live_even_when_the_body_is_empty() {
    let mut session = session("fun empty(p) {}\nempty(42);");
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    for _ in 0..32 {
        if session.snapshot().unwrap().frames[0].function == "empty" {
            break;
        }
        assert!(matches!(
            session.resume(ResumeMode::StepInto),
            RunOutcome::Paused(_)
        ));
    }
    let snapshot = session.snapshot().unwrap();
    assert_eq!(snapshot.frames[0].function, "empty");
    assert_eq!(
        snapshot.frames[0].parameters,
        vec![BindingSnapshot {
            binding_id: snapshot.frames[0]
                .parameters
                .first()
                .and_then(|p| p.binding_id),
            name: "p".to_string(),
            name_truncated: false,
            binding_kind: "parameter".to_string(),
            value_kind: ValueKind::Number,
            value: DebugValue::Number("42".to_string()),
        }]
    );
}

#[test]
fn fixed_binding_kinds_ignore_the_dynamic_string_limit() {
    let source = "fun outer(p) {\n  var local = p;\n  fun inner() {\n    print local;\n  }\n  inner();\n}\nouter(1);";
    let mut limits = SnapshotLimits::default();
    limits.max_string_bytes = 0;
    let mut session = InterpreterSession::with_snapshot_limits(
        SourceDocument::new(SourceId(83), RevisionId(1), "labels.lox", source),
        RecordingHost::default(),
        limits,
    )
    .unwrap();
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = pause_at_line(&mut session, 4);

    assert_eq!(snapshot.frames[0].upvalues.len(), 1);
    assert_eq!(snapshot.frames[0].upvalues[0].binding_kind, "upvalue");
    assert!(snapshot.frames[0].upvalues[0].name_truncated);
    assert_eq!(snapshot.frames[1].parameters.len(), 1);
    assert_eq!(snapshot.frames[1].parameters[0].binding_kind, "parameter");
    assert!(snapshot.frames[1].parameters[0].name_truncated);
    assert_eq!(snapshot.frames[1].locals.len(), 2);
    assert!(
        snapshot.frames[1]
            .locals
            .iter()
            .all(|binding| binding.binding_kind == "local" && binding.name_truncated)
    );
    assert!(!snapshot.globals.is_empty());
    assert!(
        snapshot
            .globals
            .iter()
            .all(|binding| binding.binding_kind == "global" && binding.name_truncated)
    );
    assert!(!snapshot.globals_truncated);
}

#[test]
fn aggregate_string_exhaustion_stops_later_categories_frames_and_globals() {
    let source = "fun f(p) {\n  var x = p;\n  print x;\n}\nf(1);";
    let mut limits = SnapshotLimits::default();
    limits.max_total_string_bytes = 6;
    let mut session = InterpreterSession::with_snapshot_limits(
        SourceDocument::new(SourceId(84), RevisionId(1), "aggregate.lox", source),
        RecordingHost::default(),
        limits,
    )
    .unwrap();
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = pause_at_line(&mut session, 3);

    assert_eq!(snapshot.frames.len(), 1);
    assert_eq!(snapshot.frames[0].function, "f");
    assert!(snapshot.frames[0].parameters.is_empty());
    assert!(snapshot.frames[0].parameters_truncated);
    assert!(snapshot.frames[0].locals.is_empty());
    assert!(snapshot.frames[0].locals_truncated);
    assert!(snapshot.frames_truncated);
    assert!(snapshot.globals.is_empty());
    assert!(snapshot.globals_truncated);
}

#[test]
fn aggregate_binding_exhaustion_preserves_parameter_first_priority() {
    let source = "fun outer() {\n  var captured = 1;\n  fun inner(p) {\n    var local = p;\n    print captured + local;\n  }\n  inner(2);\n}\nouter();";
    let mut limits = SnapshotLimits::default();
    limits.max_total_bindings = 1;
    let mut session = InterpreterSession::with_snapshot_limits(
        SourceDocument::new(SourceId(88), RevisionId(1), "bindings.lox", source),
        RecordingHost::default(),
        limits,
    )
    .unwrap();
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = pause_at_line(&mut session, 5);

    assert_eq!(snapshot.frames.len(), 1);
    assert_eq!(snapshot.frames[0].function, "inner");
    assert_eq!(snapshot.frames[0].parameters.len(), 1);
    assert!(!snapshot.frames[0].parameters_truncated);
    assert!(snapshot.frames[0].locals.is_empty());
    assert!(snapshot.frames[0].locals_truncated);
    assert!(snapshot.frames[0].upvalues.is_empty());
    assert!(snapshot.frames[0].upvalues_truncated);
    assert!(snapshot.frames_truncated);
    assert!(snapshot.globals.is_empty());
    assert!(snapshot.globals_truncated);
}

#[test]
fn minimum_encoded_budget_returns_a_truncated_snapshot_not_a_fault() {
    let mut limits = SnapshotLimits::default();
    limits.max_estimated_json_bytes = 1_024;
    let mut session = InterpreterSession::with_snapshot_limits(
        SourceDocument::new(SourceId(85), RevisionId(1), "small.lox", "print 1;"),
        RecordingHost::default(),
        limits,
    )
    .unwrap();

    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = session.snapshot().unwrap();
    assert!(snapshot.frames.is_empty());
    assert!(snapshot.frames_truncated);
    assert!(snapshot.globals.is_empty());
    assert!(snapshot.globals_truncated);
    assert!(snapshot.conservative_json_size().unwrap() <= 1_024);
}

#[test]
fn indirect_cycles_use_only_the_active_recursion_path() {
    let mut session = session("var a = [nil];\nvar b = [a];\na[0] = b;\nprint 0;");
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = pause_at_line(&mut session, 4);

    let DebugValue::List {
        object_id: a_id,
        items: a_items,
        truncated: false,
    } = &global(&snapshot, "a").value
    else {
        panic!("a is not a complete list")
    };
    let DebugValue::List {
        object_id: b_id,
        items: b_items,
        truncated: false,
    } = &a_items[0]
    else {
        panic!("a does not contain b")
    };
    assert_eq!(b_items, &[DebugValue::Cycle { object_id: *a_id }]);

    let DebugValue::List {
        object_id: root_b,
        items: root_b_items,
        truncated: false,
    } = &global(&snapshot, "b").value
    else {
        panic!("b is not a complete list")
    };
    assert_eq!(root_b, b_id);
    let DebugValue::List {
        object_id: nested_a,
        items: nested_a_items,
        truncated: false,
    } = &root_b_items[0]
    else {
        panic!("b does not contain a")
    };
    assert_eq!(nested_a, a_id);
    assert_eq!(nested_a_items, &[DebugValue::Cycle { object_id: *b_id }]);
}

#[test]
fn diamond_aliases_expand_with_shared_identity() {
    let source = "var shared = [1];\nvar left = [shared];\nvar right = [shared];\nvar aDiamond = [left, right];\nprint 0;";
    let mut session = session(source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = pause_at_line(&mut session, 5);

    let DebugValue::List {
        object_id: 1,
        items,
        truncated: false,
    } = &global(&snapshot, "aDiamond").value
    else {
        panic!("diamond root has the wrong shape")
    };
    let DebugValue::List {
        object_id: 2,
        items: left,
        truncated: false,
    } = &items[0]
    else {
        panic!("left branch has the wrong shape")
    };
    let DebugValue::List {
        object_id: 4,
        items: right,
        truncated: false,
    } = &items[1]
    else {
        panic!("right branch has the wrong shape")
    };
    assert!(matches!(
        left.as_slice(),
        [DebugValue::List {
            object_id: 3,
            truncated: false,
            ..
        }]
    ));
    assert!(matches!(
        right.as_slice(),
        [DebugValue::List {
            object_id: 3,
            truncated: false,
            ..
        }]
    ));
    assert!(!matches!(left[0], DebugValue::Cycle { .. }));
    assert!(!matches!(right[0], DebugValue::Cycle { .. }));
}

#[test]
fn omitted_children_do_not_consume_object_ids() {
    let mut limits = SnapshotLimits::default();
    limits.max_collection_items = 1;
    let source = "var zOmitted = [2];\nvar aRoot = [[1], zOmitted];\nvar yLater = [3];\nprint 0;";
    let mut session = InterpreterSession::with_snapshot_limits(
        SourceDocument::new(SourceId(86), RevisionId(1), "ids.lox", source),
        RecordingHost::default(),
        limits,
    )
    .unwrap();
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = pause_at_line(&mut session, 4);

    assert!(matches!(
        global(&snapshot, "aRoot").value,
        DebugValue::List {
            object_id: 1,
            ref items,
            truncated: true,
        } if matches!(items.as_slice(), [DebugValue::List { object_id: 2, .. }])
    ));
    assert!(matches!(
        global(&snapshot, "yLater").value,
        DebugValue::List { object_id: 3, .. }
    ));
    assert!(matches!(
        global(&snapshot, "zOmitted").value,
        DebugValue::List { object_id: 4, .. }
    ));
}

#[test]
fn global_insertion_order_does_not_change_snapshot_order_or_ids() {
    let mut first = session("var a = [1];\nvar z = [2];\nprint 0;");
    let mut second = session("var z = [2];\nvar a = [1];\nprint 0;");
    assert!(matches!(first.start_debugging(), RunOutcome::Paused(_)));
    assert!(matches!(second.start_debugging(), RunOutcome::Paused(_)));
    let first = pause_at_line(&mut first, 3);
    let second = pause_at_line(&mut second, 3);

    assert_eq!(first, second);
}

#[test]
fn global_limit_keeps_the_same_lexicographic_prefix_and_ids() {
    let mut limits = SnapshotLimits::default();
    limits.max_globals = 2;
    let make_session = |source| {
        InterpreterSession::with_snapshot_limits(
            SourceDocument::new(SourceId(88), RevisionId(1), "prefix.lox", source),
            RecordingHost::default(),
            limits.clone(),
        )
        .unwrap()
    };
    let mut first = make_session("var z = [9];\nvar b = [2];\nvar a = [1];\nprint 0;");
    let mut second = make_session("var a = [1];\nvar b = [2];\nvar z = [9];\nprint 0;");
    assert!(matches!(first.start_debugging(), RunOutcome::Paused(_)));
    assert!(matches!(second.start_debugging(), RunOutcome::Paused(_)));

    let first = pause_at_line(&mut first, 4);
    let second = pause_at_line(&mut second, 4);

    assert_eq!(first, second);
    assert!(first.globals_truncated);
    assert_eq!(
        first
            .globals
            .iter()
            .map(|binding| binding.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(matches!(
        first.globals[0].value,
        DebugValue::List { object_id: 1, .. }
    ));
    assert!(matches!(
        first.globals[1].value,
        DebugValue::List { object_id: 2, .. }
    ));
}

#[test]
fn control_heavy_values_truncate_below_the_encoded_cap() {
    let payload = "\u{0001}".repeat(4_096);
    let aliases = std::iter::repeat_n("payload", 255)
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "fun fill() {{\n  var payload = \"{payload}\";\n  var values = [{aliases}];\n  print 0;\n}}\nfill();"
    );
    let mut limits = SnapshotLimits::default();
    limits.max_collection_items = 256;
    let mut session = InterpreterSession::with_snapshot_limits(
        SourceDocument::new(SourceId(87), RevisionId(1), "encoded.lox", source),
        RecordingHost::default(),
        limits,
    )
    .unwrap();
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let snapshot = pause_at_line(&mut session, 4);
    let values = binding(&snapshot.frames[0].locals, "values");
    let DebugValue::List {
        items,
        truncated: true,
        ..
    } = &values.value
    else {
        panic!("control-heavy list was not explicitly truncated")
    };
    let full_prefix = items
        .iter()
        .take_while(|value| matches!(value, DebugValue::String(text) if text == &payload))
        .count();
    assert!(full_prefix > 0 && full_prefix < 255);
    assert!(
        items[full_prefix..]
            .iter()
            .all(|value| *value == DebugValue::Truncated)
    );
    let estimate = snapshot.conservative_json_size().unwrap();
    assert!(estimate > 4 * 1_048_576);
    assert!(estimate <= 5 * 1_048_576);
    assert!(estimate < 6 * 1_048_576);
}
