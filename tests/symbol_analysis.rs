use std::sync::Arc;

use rlox::{
    Interpreter, RecordingHost, RevisionId, SemanticStatus, SourceDocument, SourceId, SymbolKind,
    SymbolOccurrence, SymbolOccurrenceKind, SymbolResolution, analyze,
};

const SOURCE_ID: SourceId = SourceId(701);
const REVISION: RevisionId = RevisionId(29);

fn document(source: &str) -> SourceDocument {
    SourceDocument::new(SOURCE_ID, REVISION, "symbols.ox", source)
}

fn occurrences(source: &str) -> Vec<SymbolOccurrence> {
    let analysis = analyze(&document(source)).expect("source is within analysis limits");
    assert_eq!(analysis.semantic_status, SemanticStatus::Available);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:#?}",
        analysis.diagnostics
    );
    analysis.symbol_occurrences
}

fn named<'a>(values: &'a [SymbolOccurrence], name: &str) -> Vec<&'a SymbolOccurrence> {
    values.iter().filter(|value| value.name == name).collect()
}

fn offset(source: &str, needle: &str) -> usize {
    source.find(needle).unwrap()
}

fn assert_target(value: &SymbolOccurrence, start: usize) {
    assert_eq!(value.declaration_targets.len(), 1, "{value:#?}");
    assert_eq!(value.declaration_targets[0].start.byte_offset, start);
}

#[test]
fn locals_keep_textual_resolution_across_shadowing_slot_reuse_and_late_declarations() {
    let source = concat!(
        "var item = 0;\n",
        "{ print item; var item = 1; print item; }\n",
        "{ var item = 2; print item; }\n",
        "{ fun before() { print later; } var later = 3; before(); }\n",
        "var later = 4;"
    );
    let values = occurrences(source);
    let items = named(&values, "item");
    assert_eq!(items.len(), 6);

    let global_declaration = offset(source, "item = 0");
    let first_read = items
        .iter()
        .find(|value| value.span.start.byte_offset == offset(source, "item; var"))
        .unwrap();
    assert_eq!(first_read.resolution, SymbolResolution::Global);
    assert_target(first_read, global_declaration);

    let local_declarations = items
        .iter()
        .filter(|value| value.kind == SymbolOccurrenceKind::Declaration)
        .filter(|value| value.resolution == SymbolResolution::Local)
        .collect::<Vec<_>>();
    assert_eq!(local_declarations.len(), 2);
    assert_ne!(
        local_declarations[0].span.start.byte_offset,
        local_declarations[1].span.start.byte_offset
    );
    for declaration in local_declarations {
        assert_target(declaration, declaration.span.start.byte_offset);
        let local_read = items
            .iter()
            .find(|value| {
                value.kind == SymbolOccurrenceKind::Read
                    && value.declaration_targets == declaration.declaration_targets
            })
            .unwrap();
        assert_eq!(local_read.resolution, SymbolResolution::Local);
    }

    let late_reference = named(&values, "later")
        .into_iter()
        .find(|value| value.kind == SymbolOccurrenceKind::Read)
        .unwrap();
    assert_eq!(late_reference.resolution, SymbolResolution::Global);
    assert_target(late_reference, offset(source, "later = 4"));
}

#[test]
fn parameters_and_local_assignments_report_exact_roles_and_targets() {
    let source = "fun update(value) { value = value + 1; var local = value; local = value; }";
    let values = occurrences(source);

    let parameter = named(&values, "value")
        .into_iter()
        .find(|value| value.kind == SymbolOccurrenceKind::Declaration)
        .unwrap();
    assert_eq!(parameter.symbol_kind, SymbolKind::Parameter);
    assert_eq!(parameter.resolution, SymbolResolution::Local);
    assert_target(parameter, offset(source, "value)"));

    let parameter_uses = named(&values, "value")
        .into_iter()
        .filter(|value| value.kind != SymbolOccurrenceKind::Declaration)
        .collect::<Vec<_>>();
    assert_eq!(parameter_uses.len(), 4);
    assert_eq!(parameter_uses[0].kind, SymbolOccurrenceKind::Write);
    assert!(
        parameter_uses[1..]
            .iter()
            .all(|value| value.kind == SymbolOccurrenceKind::Read)
    );
    assert!(parameter_uses.iter().all(|value| {
        value.symbol_kind == SymbolKind::Parameter
            && value.resolution == SymbolResolution::Local
            && value.declaration_targets == parameter.declaration_targets
    }));

    let local = named(&values, "local");
    assert_eq!(local.len(), 2);
    assert_eq!(local[0].kind, SymbolOccurrenceKind::Declaration);
    assert_eq!(local[1].kind, SymbolOccurrenceKind::Write);
    assert_target(local[1], local[0].span.start.byte_offset);
}

#[test]
fn recursive_local_functions_and_closures_retain_original_provenance() {
    let source = concat!(
        "{ var captured = 1;\n",
        "  fun recurse(n) { if (n > 0) recurse(n - 1); }\n",
        "  fun direct() { print captured; }\n",
        "  fun middle() { fun inner() { print captured; } inner(); }\n",
        "  direct(); middle();\n",
        "}"
    );
    let values = occurrences(source);

    let recurse = named(&values, "recurse");
    assert_eq!(recurse.len(), 2);
    assert_eq!(recurse[0].symbol_kind, SymbolKind::Function);
    assert_eq!(recurse[0].resolution, SymbolResolution::Local);
    assert_eq!(recurse[1].symbol_kind, SymbolKind::Function);
    assert_eq!(recurse[1].resolution, SymbolResolution::CapturedUpvalue);
    assert_eq!(
        recurse[1].declaration_targets,
        recurse[0].declaration_targets
    );

    let captured = named(&values, "captured");
    assert_eq!(captured.len(), 3);
    for reference in &captured[1..] {
        assert_eq!(reference.resolution, SymbolResolution::CapturedUpvalue);
        assert_target(reference, captured[0].span.start.byte_offset);
    }

    let inner = named(&values, "inner");
    assert_eq!(inner[0].kind, SymbolOccurrenceKind::Declaration);
    assert_eq!(inner[1].resolution, SymbolResolution::Local);
    assert_target(inner[1], inner[0].span.start.byte_offset);
}

#[test]
fn for_and_switch_scopes_follow_the_compilers_actual_local_resolution() {
    let source = concat!(
        "for (var i = 0; i < 1; i = i + 1) { print i; }\n",
        "print i;\n",
        "switch (1) { case 1: var chosen = 2; print chosen; default: print chosen; }\n",
        "print chosen;"
    );
    let values = occurrences(source);

    let i = named(&values, "i");
    assert_eq!(i[0].kind, SymbolOccurrenceKind::Declaration);
    assert!(i[1..5].iter().all(|value| {
        value.resolution == SymbolResolution::Local
            && value.declaration_targets == i[0].declaration_targets
    }));
    assert_eq!(i[5].resolution, SymbolResolution::Unresolved);
    assert!(i[5].declaration_targets.is_empty());

    let chosen = named(&values, "chosen");
    assert_eq!(chosen[0].kind, SymbolOccurrenceKind::Declaration);
    assert!(chosen[1..3].iter().all(|value| {
        value.resolution == SymbolResolution::Local
            && value.declaration_targets == chosen[0].declaration_targets
    }));
    assert_eq!(chosen[3].resolution, SymbolResolution::Unresolved);
}

#[test]
fn globals_are_finalized_after_compile_and_duplicate_candidates_are_observable() {
    let source = concat!(
        "print future; future = 1; var future = 2; print future;\n",
        "print duplicate; var duplicate = 1; fun duplicate() {} print duplicate;\n",
        "print missing; assigned = 3;"
    );
    let values = occurrences(source);

    let future = named(&values, "future");
    let declaration = future
        .iter()
        .find(|value| value.kind == SymbolOccurrenceKind::Declaration)
        .unwrap();
    assert_eq!(declaration.symbol_kind, SymbolKind::Variable);
    for reference in future
        .iter()
        .filter(|value| value.kind != SymbolOccurrenceKind::Declaration)
    {
        assert_eq!(reference.resolution, SymbolResolution::Global);
        assert_eq!(reference.symbol_kind, SymbolKind::Variable);
        assert_eq!(
            reference.declaration_targets,
            declaration.declaration_targets
        );
    }

    let duplicate = named(&values, "duplicate");
    let declarations = duplicate
        .iter()
        .filter(|value| value.kind == SymbolOccurrenceKind::Declaration)
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 2);
    assert_eq!(declarations[0].symbol_kind, SymbolKind::Variable);
    assert_eq!(declarations[1].symbol_kind, SymbolKind::Function);
    assert!(declarations.iter().all(|value| {
        value.declaration_targets.as_ref() == [value.span]
            && value.resolution == SymbolResolution::Global
    }));
    for reference in duplicate
        .iter()
        .filter(|value| value.kind != SymbolOccurrenceKind::Declaration)
    {
        assert_eq!(reference.resolution, SymbolResolution::Global);
        assert_eq!(reference.symbol_kind, SymbolKind::Unknown);
        assert_eq!(
            reference.declaration_targets.as_ref(),
            declarations
                .iter()
                .map(|value| value.span)
                .collect::<Vec<_>>()
        );
    }

    for name in ["missing", "assigned"] {
        let value = named(&values, name)[0];
        assert_eq!(value.resolution, SymbolResolution::Unresolved);
        assert_eq!(value.symbol_kind, SymbolKind::Unknown);
        assert!(value.declaration_targets.is_empty());
    }
    assert_eq!(
        named(&values, "assigned")[0].kind,
        SymbolOccurrenceKind::Write
    );
}

#[test]
fn duplicate_global_references_share_one_source_ordered_target_allocation() {
    let declaration_count = 96;
    let reference_count = 96;
    let mut source = String::new();
    for index in 0..declaration_count {
        source.push_str(&format!("var shared = {index};\n"));
    }
    for _ in 0..reference_count {
        source.push_str("print shared;\n");
    }

    let values = occurrences(&source);
    let shared = named(&values, "shared");
    let declarations = shared
        .iter()
        .filter(|value| value.kind == SymbolOccurrenceKind::Declaration)
        .copied()
        .collect::<Vec<_>>();
    let references = shared
        .iter()
        .filter(|value| value.kind == SymbolOccurrenceKind::Read)
        .copied()
        .collect::<Vec<_>>();

    assert_eq!(declarations.len(), declaration_count);
    assert_eq!(references.len(), reference_count);
    let expected = declarations
        .iter()
        .map(|value| value.span)
        .collect::<Vec<_>>();
    assert!(
        declarations
            .iter()
            .all(|value| value.declaration_targets.as_ref() == [value.span])
    );
    assert!(
        references
            .iter()
            .all(|value| value.declaration_targets.as_ref() == expected)
    );
    assert!(references[1..].iter().all(|value| {
        Arc::ptr_eq(
            &references[0].declaration_targets,
            &value.declaration_targets,
        )
    }));
}

#[test]
fn built_in_clock_yields_to_document_and_lexical_declarations() {
    let source = concat!(
        "print clock; clock = clock;\n",
        "{ var clock = 1; fun direct() { print clock; } direct(); }\n",
        "fun parameter(clock) { fun middle() { fun nested() { print clock; } nested(); } middle(); }"
    );
    let values = occurrences(source);
    let clock = named(&values, "clock");
    assert_eq!(clock[0].resolution, SymbolResolution::BuiltIn);
    assert_eq!(clock[0].symbol_kind, SymbolKind::BuiltIn);
    assert!(clock[0].declaration_targets.is_empty());
    assert_eq!(clock[1].kind, SymbolOccurrenceKind::Write);
    assert_eq!(clock[1].resolution, SymbolResolution::BuiltIn);
    assert_eq!(clock[2].resolution, SymbolResolution::BuiltIn);

    let local = clock
        .iter()
        .find(|value| {
            value.kind == SymbolOccurrenceKind::Declaration
                && value.symbol_kind == SymbolKind::Variable
        })
        .unwrap();
    let direct_capture = clock
        .iter()
        .find(|value| {
            value.kind == SymbolOccurrenceKind::Read
                && value.resolution == SymbolResolution::CapturedUpvalue
                && value.declaration_targets == local.declaration_targets
        })
        .unwrap();
    assert_eq!(direct_capture.symbol_kind, SymbolKind::Variable);

    let parameter = clock
        .iter()
        .find(|value| value.symbol_kind == SymbolKind::Parameter)
        .unwrap();
    assert!(clock.iter().any(|value| {
        value.kind == SymbolOccurrenceKind::Read
            && value.resolution == SymbolResolution::CapturedUpvalue
            && value.declaration_targets == parameter.declaration_targets
    }));

    let redefined = occurrences("print clock; var clock = 1; clock = clock;");
    let declaration = named(&redefined, "clock")
        .into_iter()
        .find(|value| value.kind == SymbolOccurrenceKind::Declaration)
        .unwrap();
    assert!(
        named(&redefined, "clock")
            .into_iter()
            .filter(|value| value.kind != SymbolOccurrenceKind::Declaration)
            .all(|value| {
                value.resolution == SymbolResolution::Global
                    && value.symbol_kind == SymbolKind::Variable
                    && value.declaration_targets == declaration.declaration_targets
            })
    );
}

#[test]
fn invalid_programs_discard_partial_semantics_and_phantom_declarations() {
    for source in [
        "var valid = 1; print valid; var = 2;",
        "{ var duplicate = 1; var duplicate = 2; print duplicate; }",
        "fun broken(ok, ) { print ok; }",
    ] {
        let analysis = analyze(&document(source)).unwrap();
        assert_eq!(analysis.semantic_status, SemanticStatus::Unavailable);
        assert!(!analysis.diagnostics.is_empty());
        assert!(analysis.symbol_occurrences.is_empty());
    }
}

#[test]
fn occurrence_and_target_spans_are_owned_source_ordered_and_exact() {
    let source =
        "print \"😀\"; var alpha = 1; fun use(beta) { alpha = beta; print alpha; } use(alpha);";
    let values = occurrences(source);
    assert!(
        values
            .windows(2)
            .all(|pair| { pair[0].span.start.byte_offset <= pair[1].span.start.byte_offset })
    );
    for value in &values {
        for span in std::iter::once(&value.span).chain(value.declaration_targets.iter()) {
            assert_eq!(span.source_id, SOURCE_ID);
            assert_eq!(span.revision, REVISION);
            assert!(span.start.byte_offset <= span.end.byte_offset);
            assert!(span.end.byte_offset <= source.len());
            assert!(source.is_char_boundary(span.start.byte_offset));
            assert!(source.is_char_boundary(span.end.byte_offset));
            assert_eq!(
                &source[span.start.byte_offset..span.end.byte_offset],
                value.name
            );
        }
    }
}

#[test]
fn collecting_analysis_does_not_change_interpreter_results_or_diagnostics() {
    let source = concat!(
        "var global = 1;\n",
        "fun outer(parameter) { var local = parameter; fun inner() { print local; } inner(); }\n",
        "outer(global);"
    );
    let analysis = analyze(&document(source)).unwrap();
    assert_eq!(analysis.semantic_status, SemanticStatus::Available);

    let mut host = RecordingHost::default();
    let result = Interpreter::new().run(document(source), &mut host);
    assert_eq!(result, rlox::InterpretResult::Ok);
    assert_eq!(host.output(), &["1"]);
    assert!(host.diagnostics().is_empty());

    let invalid = document("{ var same = 1; var same = 2; }");
    let analysis = analyze(&invalid).unwrap();
    let mut host = RecordingHost::default();
    let result = Interpreter::new().run(invalid, &mut host);
    assert_eq!(result, rlox::InterpretResult::CompileError);
    assert_eq!(analysis.diagnostics, host.diagnostics());
}
