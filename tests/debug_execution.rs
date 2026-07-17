use rlox::{
    Diagnostic, ExecutionControl, ExecutionState, InterpreterSession, PauseReason, RecordingHost,
    ResumeMode, RevisionId, RunOutcome, RuntimeHost, SessionOperation, SourceDocument, SourceId,
};
use std::sync::mpsc;

fn session(source: &str) -> InterpreterSession<RecordingHost> {
    InterpreterSession::new(
        SourceDocument::new(SourceId(70), RevisionId(1), "debug.lox", source),
        RecordingHost::default(),
    )
}

#[test]
fn start_debugging_pauses_before_the_first_statement_and_resume_makes_progress() {
    let mut session = session("print 1; print 2;");

    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let first = session.pause_location().unwrap().clone();
    assert_eq!(first.span.start.line, 1);
    assert_eq!(first.span.start.column, 1);
    assert_eq!(first.span.end.byte_offset, "print 1;".len());
    assert!(session.host().output().is_empty());

    assert!(matches!(
        session.resume(ResumeMode::StepInto),
        RunOutcome::Paused(_)
    ));
    let second = session.pause_location().unwrap();
    assert_ne!(second.dynamic_event, first.dynamic_event);
    assert_eq!(second.span.start.column, 10);
    assert_eq!(session.host().output(), ["1"]);
}

#[test]
fn empty_function_uses_its_entry_without_a_zero_progress_pause_chain() {
    let mut session = session("fun empty() {} empty(); print 1;");
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let call = advance_to_column(&mut session, 16);

    assert!(matches!(
        session.resume(ResumeMode::StepInto),
        RunOutcome::Paused(PauseReason::Step)
    ));
    let entry = *session.pause_location().unwrap();
    assert_ne!(entry.activation_id, call.activation_id);
    assert_eq!(entry.span.start.column, 1);

    assert!(matches!(
        session.resume(ResumeMode::StepInto),
        RunOutcome::Paused(PauseReason::Step)
    ));
    assert_eq!(session.pause_location().unwrap().span.start.column, 25);
    assert!(session.host().output().is_empty());
}

#[test]
fn run_all_completes_without_an_initial_pause() {
    let mut session = session("print 1;");

    assert!(matches!(session.run_all(), RunOutcome::Completed));
    assert_eq!(session.host().output(), ["1"]);
    assert!(session.pause_location().is_none());
}

#[test]
fn invalid_calls_are_rejected_without_advancing_or_restarting() {
    let mut session = session("print 1;");

    assert!(matches!(
        session.resume(ResumeMode::StepInto),
        RunOutcome::Rejected(error)
            if error.state == ExecutionState::Ready
                && error.operation == SessionOperation::Resume(ResumeMode::StepInto)
    ));
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let location = *session.pause_location().unwrap();
    assert!(matches!(
        session.start_debugging(),
        RunOutcome::Rejected(error) if error.state == ExecutionState::Paused
    ));
    assert!(matches!(
        session.run_all(),
        RunOutcome::Rejected(error) if error.state == ExecutionState::Paused
    ));
    assert_eq!(session.pause_location(), Some(&location));
    assert!(session.host().output().is_empty());

    assert!(matches!(
        session.resume(ResumeMode::Continue),
        RunOutcome::Completed
    ));
    let output = session.host().output().to_vec();
    assert!(matches!(
        session.resume(ResumeMode::Continue),
        RunOutcome::Rejected(error) if error.state == ExecutionState::Completed
    ));
    assert_eq!(session.host().output(), output);
}

#[test]
fn compile_failure_returns_the_first_diagnostic_and_retains_all_diagnostics() {
    let mut session = session("var = 1;\nprint ;");

    let RunOutcome::Faulted(first) = session.run_all() else {
        panic!("expected compilation fault");
    };
    assert_eq!(session.execution_state(), ExecutionState::Faulted);
    assert!(session.host().diagnostics().len() >= 2);
    assert_eq!(&first, &session.host().diagnostics()[0]);
    let snapshot = session.host().diagnostics().to_vec();
    assert!(matches!(session.run_all(), RunOutcome::Rejected(_)));
    assert_eq!(session.host().diagnostics(), snapshot);
}

fn advance_to_column(
    session: &mut InterpreterSession<RecordingHost>,
    column: usize,
) -> rlox::PauseLocation {
    loop {
        let location = *session.pause_location().unwrap();
        if location.span.start.column == column {
            return location;
        }
        assert!(matches!(
            session.resume(ResumeMode::StepInto),
            RunOutcome::Paused(_)
        ));
    }
}

fn advance_to_line(
    session: &mut InterpreterSession<RecordingHost>,
    line: usize,
) -> rlox::PauseLocation {
    loop {
        let location = *session.pause_location().unwrap();
        if location.span.start.line == line {
            return location;
        }
        assert!(matches!(
            session.resume(ResumeMode::StepInto),
            RunOutcome::Paused(_)
        ));
    }
}

#[test]
fn step_into_enters_a_callee_and_step_out_returns_to_the_exact_caller() {
    let mut session = session("fun inner(){ print 1; } inner(); print 2;");
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let call = advance_to_column(&mut session, 25);

    assert!(matches!(
        session.resume(ResumeMode::StepInto),
        RunOutcome::Paused(PauseReason::Step)
    ));
    let inside = *session.pause_location().unwrap();
    assert_ne!(inside.activation_id, call.activation_id);
    assert_eq!(inside.span.start.column, 14);
    assert!(session.host().output().is_empty());

    assert!(matches!(
        session.resume(ResumeMode::StepOut),
        RunOutcome::Paused(PauseReason::Step)
    ));
    assert_eq!(session.pause_location().unwrap().span.start.column, 34);
    assert_eq!(session.host().output(), ["1"]);
}

#[test]
fn step_over_skips_descendant_activations() {
    let mut session = session("fun inner(){ print 1; } inner(); print 2;");
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    advance_to_column(&mut session, 25);

    assert!(matches!(
        session.resume(ResumeMode::StepOver),
        RunOutcome::Paused(PauseReason::Step)
    ));
    assert_eq!(session.pause_location().unwrap().span.start.column, 34);
    assert_eq!(session.host().output(), ["1"]);
}

#[test]
fn step_over_skips_every_recursive_descendant_activation() {
    let source = "fun rec(n) {\n  if (n > 0) rec(n-1);\n  print n;\n}\nrec(2);\nprint 9;";
    let mut session = session(source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let call = advance_to_line(&mut session, 5);

    assert!(matches!(
        session.resume(ResumeMode::StepOver),
        RunOutcome::Paused(PauseReason::Step)
    ));
    let after = *session.pause_location().unwrap();
    assert_eq!(after.span.start.line, 6);
    assert_eq!(after.activation_id, call.activation_id);
    assert_eq!(session.host().output(), ["0", "1", "2"]);
}

#[test]
fn step_out_from_deep_recursion_returns_to_the_immediate_activation() {
    let source = "fun rec(n) {\n  if (n > 0) rec(n-1);\n  print n;\n}\nrec(2);\nprint 9;";
    let mut session = session(source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let script_call = advance_to_line(&mut session, 5);

    assert!(matches!(
        session.resume(ResumeMode::StepInto),
        RunOutcome::Paused(_)
    ));
    let first = session.pause_location().unwrap().activation_id;
    assert_ne!(first, script_call.activation_id);
    let middle = loop {
        assert!(matches!(
            session.resume(ResumeMode::StepInto),
            RunOutcome::Paused(_)
        ));
        let activation = session.pause_location().unwrap().activation_id;
        if activation != first {
            break activation;
        }
    };
    let deepest = loop {
        assert!(matches!(
            session.resume(ResumeMode::StepInto),
            RunOutcome::Paused(_)
        ));
        let activation = session.pause_location().unwrap().activation_id;
        if activation != middle {
            break activation;
        }
    };
    assert_ne!(deepest, middle);

    assert!(matches!(
        session.resume(ResumeMode::StepOut),
        RunOutcome::Paused(PauseReason::Step)
    ));
    assert_eq!(session.pause_location().unwrap().activation_id, middle);
    assert_eq!(session.pause_location().unwrap().span.start.line, 3);
    assert_eq!(session.host().output(), ["0"]);
}

#[test]
fn mutual_recursion_uses_activation_identity_instead_of_function_identity() {
    let source = "fun a(n) {\n  if (n > 0) b(n-1);\n  print n;\n}\nfun b(n) {\n  if (n > 0) a(n-1);\n  print n;\n}\na(2);\nprint 9;";
    let mut session = session(source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let call = advance_to_line(&mut session, 9);

    assert!(matches!(
        session.resume(ResumeMode::StepOver),
        RunOutcome::Paused(PauseReason::Step)
    ));
    assert_eq!(session.pause_location().unwrap().span.start.line, 10);
    assert_eq!(
        session.pause_location().unwrap().activation_id,
        call.activation_id
    );
    assert_eq!(session.host().output(), ["0", "1", "2"]);
}

#[test]
fn step_out_skips_callers_that_return_without_another_semantic_point() {
    let source = "fun empty() {}\nfun wrapper() { empty(); }\nwrapper();\nprint 1;";
    let mut session = session(source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    advance_to_line(&mut session, 3);
    assert!(matches!(
        session.resume(ResumeMode::StepInto),
        RunOutcome::Paused(_)
    ));
    assert!(matches!(
        session.resume(ResumeMode::StepInto),
        RunOutcome::Paused(_)
    ));

    assert!(matches!(
        session.resume(ResumeMode::StepOut),
        RunOutcome::Paused(PauseReason::Step)
    ));
    assert_eq!(session.pause_location().unwrap().span.start.line, 4);
    assert!(session.host().output().is_empty());
}

#[test]
fn step_into_over_a_native_call_stays_in_the_current_activation() {
    let mut session = session("clock(); print 1;");
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let call = *session.pause_location().unwrap();

    assert!(matches!(
        session.resume(ResumeMode::StepInto),
        RunOutcome::Paused(PauseReason::Step)
    ));
    let after = *session.pause_location().unwrap();
    assert_eq!(after.activation_id, call.activation_id);
    assert_eq!(after.span.start.column, 10);
}

#[test]
fn same_line_statements_and_multiline_expressions_remain_semantic_units() {
    let source = "var a=1; a=a+1; print a;\nprint (3 +\n  4);\nprint 8;";
    let mut session = session(source);
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let mut spans = Vec::new();

    loop {
        spans.push(session.pause_location().unwrap().span);
        match session.resume(ResumeMode::StepInto) {
            RunOutcome::Paused(_) => {}
            RunOutcome::Completed => break,
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    assert_eq!(session.host().output(), ["2", "7", "8"]);
    let first_line = spans
        .iter()
        .filter(|span| span.start.line == 1)
        .collect::<Vec<_>>();
    assert_eq!(first_line.len(), 3);
    assert_eq!(
        first_line
            .iter()
            .map(|span| span.start.column)
            .collect::<Vec<_>>(),
        [1, 10, 17]
    );
    let multiline = spans
        .iter()
        .filter(|span| span.start.line == 2)
        .collect::<Vec<_>>();
    assert_eq!(multiline.len(), 1);
    assert_eq!(multiline[0].end.line, 3);
}

#[test]
fn script_level_step_out_runs_to_completion() {
    let mut session = session("print 1; print 2;");
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));

    assert!(matches!(
        session.resume(ResumeMode::StepOut),
        RunOutcome::Completed
    ));
    assert_eq!(session.host().output(), ["1", "2"]);
}

#[test]
fn loop_revisits_create_new_dynamic_events() {
    let mut session = session("for (var i=0; i<2; i=i+1) print i;");
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let mut visits = Vec::new();

    loop {
        let location = *session.pause_location().unwrap();
        visits.push((location.debug_point_id, location.dynamic_event));
        match session.resume(ResumeMode::StepInto) {
            RunOutcome::Paused(_) => {}
            RunOutcome::Completed => break,
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    assert_eq!(session.host().output(), ["0", "1"]);
    let mut repeated = false;
    for (index, (point, event)) in visits.iter().enumerate() {
        for (later_point, later_event) in &visits[index + 1..] {
            if point == later_point {
                assert!(later_event > event);
                repeated = true;
            }
        }
    }
    assert!(
        repeated,
        "expected a loop point to be revisited: {visits:?}"
    );
}

#[test]
fn explicit_pause_has_priority_over_a_completed_step() {
    let mut session = session("print 1; print 2;");
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    session.control().request_pause();

    assert!(matches!(
        session.resume(ResumeMode::StepInto),
        RunOutcome::Paused(PauseReason::Explicit)
    ));
    assert_eq!(session.host().output(), ["1"]);
}

#[test]
fn pre_start_pause_consumes_the_one_shot_initial_debug_stop() {
    let mut session = session("print 1; print 2;");
    session.control().request_pause();

    assert!(matches!(
        session.start_debugging(),
        RunOutcome::Paused(PauseReason::Explicit)
    ));
    assert!(session.host().output().is_empty());

    assert!(matches!(
        session.resume(ResumeMode::Continue),
        RunOutcome::Completed
    ));
    assert_eq!(session.host().output(), ["1", "2"]);
}

#[test]
fn cancellation_is_sticky_and_wins_over_pause() {
    let mut session = session("print 1;");
    assert!(matches!(session.start_debugging(), RunOutcome::Paused(_)));
    let control = session.control();
    control.request_pause();
    control.request_cancel();

    assert!(matches!(
        session.resume(ResumeMode::StepInto),
        RunOutcome::Cancelled
    ));
    assert_eq!(session.execution_state(), ExecutionState::Cancelled);
    assert!(session.host().output().is_empty());
    assert!(session.host().diagnostics().is_empty());
    assert!(matches!(session.run_all(), RunOutcome::Rejected(_)));
}

#[test]
fn runtime_fault_is_terminal_and_preserves_prior_output() {
    let mut session = session("print 1; print missing; print 3;");

    let RunOutcome::Faulted(diagnostic) = session.run_all() else {
        panic!("expected runtime fault");
    };
    assert_eq!(session.host().output(), ["1"]);
    assert_eq!(session.host().diagnostics(), [diagnostic]);
    assert_eq!(session.execution_state(), ExecutionState::Faulted);
    assert!(session.pause_location().is_none());
}

#[derive(Default)]
struct SignallingHost {
    output: Vec<String>,
    diagnostics: Vec<Diagnostic>,
    first_output: Option<mpsc::Sender<()>>,
}

impl RuntimeHost for SignallingHost {
    fn output(&mut self, text: String) {
        self.output.push(text);
        if let Some(sender) = self.first_output.take() {
            sender.send(()).unwrap();
        }
    }

    fn diagnostic(&mut self, value: Diagnostic) {
        self.diagnostics.push(value);
    }
}

#[test]
fn a_cloned_control_cancels_an_infinite_output_loop_without_sleeps() {
    let (sender, receiver) = mpsc::channel();
    let host = SignallingHost {
        first_output: Some(sender),
        ..SignallingHost::default()
    };
    let mut session = InterpreterSession::new(
        SourceDocument::new(
            SourceId(71),
            RevisionId(1),
            "loop.lox",
            "while (true) print 1;",
        ),
        host,
    );
    let control = session.control();
    let controller = std::thread::spawn(move || {
        receiver.recv().unwrap();
        control.request_cancel();
    });

    assert!(matches!(session.run_all(), RunOutcome::Cancelled));
    controller.join().unwrap();
    assert!(!session.host().output.is_empty());
    assert!(session.host().diagnostics.is_empty());
}

#[derive(Default)]
struct PausingHost {
    output: Vec<String>,
    control: Option<ExecutionControl>,
}

impl RuntimeHost for PausingHost {
    fn output(&mut self, text: String) {
        self.output.push(text);
        if self.output.len() == 1 {
            self.control.as_ref().unwrap().request_pause();
        }
    }

    fn diagnostic(&mut self, _value: Diagnostic) {}
}

#[test]
fn output_callback_pause_is_acknowledged_at_the_next_semantic_boundary() {
    let mut session = InterpreterSession::new(
        SourceDocument::new(
            SourceId(72),
            RevisionId(1),
            "pause.lox",
            "while (true) print 1;",
        ),
        PausingHost::default(),
    );
    session.host_mut().control = Some(session.control());

    assert!(matches!(
        session.run_all(),
        RunOutcome::Paused(PauseReason::Explicit)
    ));
    let snapshot = session.host().output.clone();
    assert_eq!(snapshot, ["1"]);
    assert_eq!(session.host().output, snapshot);

    session.control().request_cancel();
    assert!(matches!(
        session.resume(ResumeMode::Continue),
        RunOutcome::Cancelled
    ));
    assert_eq!(session.host().output, snapshot);
}

#[test]
fn empty_infinite_loop_honors_cancellation_and_does_not_leak_to_a_fresh_session() {
    let mut cancelled = session("for (;;) {}");
    assert!(matches!(cancelled.start_debugging(), RunOutcome::Paused(_)));
    let old_control = cancelled.control();
    let controller_control = old_control.clone();
    let (start_sender, start_receiver) = mpsc::channel();
    let controller = std::thread::spawn(move || {
        start_receiver.recv().unwrap();
        controller_control.request_cancel();
    });
    start_sender.send(()).unwrap();
    assert!(matches!(
        cancelled.resume(ResumeMode::Continue),
        RunOutcome::Cancelled
    ));
    controller.join().unwrap();

    let mut fresh = session("print 7;");
    assert!(matches!(fresh.run_all(), RunOutcome::Completed));
    assert_eq!(fresh.host().output(), ["7"]);
    old_control.request_pause();
    assert_eq!(fresh.execution_state(), ExecutionState::Completed);
}

#[test]
fn cancellation_closes_captured_locals_and_heavy_resume_preserves_values() {
    let source = "fun outer() { var value=\"a\"; fun inner(){ print [value+\"b\", value]; } inner(); } outer(); print \"done\";";
    let mut completed = session(source);
    assert!(matches!(completed.start_debugging(), RunOutcome::Paused(_)));
    loop {
        match completed.resume(ResumeMode::StepInto) {
            RunOutcome::Paused(_) => {}
            RunOutcome::Completed => break,
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
    assert_eq!(completed.host().output(), ["[ab, a]", "done"]);

    let mut cancelled =
        session("fun outer(){ var value=1; fun inner(){ print value; } while (true) {} } outer();");
    assert!(matches!(cancelled.start_debugging(), RunOutcome::Paused(_)));
    for _ in 0..4 {
        assert!(matches!(
            cancelled.resume(ResumeMode::StepInto),
            RunOutcome::Paused(_)
        ));
    }
    cancelled.control().request_cancel();
    assert!(matches!(
        cancelled.resume(ResumeMode::Continue),
        RunOutcome::Cancelled
    ));
    assert!(cancelled.host().diagnostics().is_empty());
}

#[test]
fn execution_control_is_the_thread_safe_shared_surface() {
    fn assert_send_sync_clone<T: Send + Sync + Clone>() {}
    assert_send_sync_clone::<ExecutionControl>();
}
