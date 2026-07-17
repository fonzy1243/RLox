use rlox::{
    DiagnosticPhase, InterpretResult, Interpreter, RecordingHost, RevisionId, SourceDocument,
    SourceId,
};

#[test]
fn runtime_errors_report_the_identifier_span() {
    let document = SourceDocument::new(SourceId(1), RevisionId(1), "sample.ox", "print 1 == nope;");
    let mut host = RecordingHost::default();

    let status = Interpreter::new().run(document, &mut host);

    assert_eq!(status, InterpretResult::RuntimeError);
    assert!(host.output().is_empty());
    assert_eq!(host.diagnostics().len(), 1);
    assert_eq!(host.diagnostics()[0].phase, DiagnosticPhase::Runtime);
    assert_eq!(host.diagnostics()[0].span.start.line, 1);
    assert_eq!(host.diagnostics()[0].span.start.column, 12);
    assert_eq!(host.diagnostics()[0].span.end.column, 16);
}

#[test]
fn print_uses_program_output_instead_of_diagnostics() {
    let document = SourceDocument::new(SourceId(2), RevisionId(1), "output.ox", "print \"hello\";");
    let mut host = RecordingHost::default();

    let status = Interpreter::new().run(document, &mut host);

    assert_eq!(status, InterpretResult::Ok);
    assert_eq!(host.output(), &["hello"]);
    assert!(host.diagnostics().is_empty());
}

#[test]
fn compile_errors_include_a_nonempty_source_span() {
    let document = SourceDocument::new(SourceId(3), RevisionId(4), "broken.ox", "var = 1;");
    let mut host = RecordingHost::default();

    let status = Interpreter::new().run(document, &mut host);

    assert_eq!(status, InterpretResult::CompileError);
    let diagnostic = &host.diagnostics()[0];
    assert!(matches!(
        diagnostic.phase,
        DiagnosticPhase::Scanner | DiagnosticPhase::Parser | DiagnosticPhase::Compiler
    ));
    assert_eq!(diagnostic.span.start.line, 1);
    assert_eq!(diagnostic.span.start.column, 5);
    assert!(diagnostic.span.start.byte_offset < diagnostic.span.end.byte_offset);
}

#[test]
fn source_documents_normalize_text_before_coordinates_are_measured() {
    let document = SourceDocument::new(
        SourceId(9),
        RevisionId(7),
        "normalized.ox",
        "\u{feff}print 1;\r\nprint 2;\rprint 3;",
    );

    assert_eq!(&*document.text, "print 1;\nprint 2;\nprint 3;");
}

fn run(source: &str) -> (InterpretResult, RecordingHost) {
    let document = SourceDocument::new(SourceId(20), RevisionId(3), "regression.ox", source);
    let mut host = RecordingHost::default();
    let status = Interpreter::new().run(document, &mut host);
    (status, host)
}

#[test]
fn multiline_strings_leave_following_diagnostics_on_the_correct_line() {
    let (status, host) = run("print \"first\nβ\";\nprint missing;");

    assert_eq!(status, InterpretResult::RuntimeError);
    let diagnostic = &host.diagnostics()[0];
    assert_eq!(
        (diagnostic.span.start.line, diagnostic.span.start.column),
        (3, 7)
    );
    assert_eq!(diagnostic.span.start.byte_offset, 24);
}

#[test]
fn if_condition_errors_name_the_closing_delimiter() {
    let (status, host) = run("if (true { print 1; }");

    assert_eq!(status, InterpretResult::CompileError);
    assert!(
        host.diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("Expect ')' after condition."))
    );
}

#[test]
fn parser_recovery_reports_multiple_useful_errors() {
    let (status, host) = run("var = 1;\nprint ;");

    assert_eq!(status, InterpretResult::CompileError);
    assert!(host.diagnostics().len() >= 2, "{:#?}", host.diagnostics());
    assert!(
        host.diagnostics().iter().all(|diagnostic| {
            diagnostic.span.start.byte_offset < diagnostic.span.end.byte_offset
        })
    );
}

#[test]
fn wrong_arity_uses_the_call_site_and_owned_frames() {
    let (status, host) = run("fun f(a) {}\nf();");

    assert_eq!(status, InterpretResult::RuntimeError);
    let diagnostic = &host.diagnostics()[0];
    assert!(
        diagnostic
            .message
            .contains("Expected 1 arguments but got 0.")
    );
    assert_eq!(
        (diagnostic.span.start.line, diagnostic.span.start.column),
        (2, 2)
    );
    assert_eq!(
        (diagnostic.span.end.line, diagnostic.span.end.column),
        (2, 4)
    );
    assert_eq!(diagnostic.frames[0].function, "<script>");
}

#[test]
fn nested_runtime_frames_are_innermost_first() {
    let source = "fun inner() { print 1 + true; }\nfun outer() { inner(); }\nouter();";
    let (status, host) = run(source);

    assert_eq!(status, InterpretResult::RuntimeError);
    let diagnostic = &host.diagnostics()[0];
    assert_eq!(diagnostic.span.start.column, 23);
    assert_eq!(
        diagnostic
            .frames
            .iter()
            .map(|frame| frame.function.as_str())
            .collect::<Vec<_>>(),
        ["inner", "outer", "<script>"]
    );
}

#[test]
fn list_index_errors_use_the_bracket_expression() {
    let (status, host) = run("var values = [1]; print values[2];");

    assert_eq!(status, InterpretResult::RuntimeError);
    let diagnostic = &host.diagnostics()[0];
    assert_eq!(
        (diagnostic.span.start.column, diagnostic.span.end.column),
        (31, 34)
    );
}

#[test]
fn consecutive_scanner_errors_are_not_suppressed_by_parser_recovery() {
    let (status, host) = run("β@");

    assert_eq!(status, InterpretResult::CompileError);
    let scanner_diagnostics = host
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.phase == DiagnosticPhase::Scanner)
        .collect::<Vec<_>>();
    assert_eq!(scanner_diagnostics.len(), 2);
    assert_eq!(scanner_diagnostics[0].span.start.column, 1);
    assert_eq!(scanner_diagnostics[1].span.start.column, 2);
}
