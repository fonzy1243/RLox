use rlox::{
    AnalysisError, AnalysisLimit, HighlightKind, Interpreter, MAX_ANALYSIS_DIAGNOSTICS,
    MAX_ANALYSIS_LEXICAL_ITEMS, MAX_ANALYSIS_NESTING_DEPTH, MAX_ANALYSIS_SOURCE_BYTES,
    RecordingHost, RevisionId, SemanticStatus, SourceDocument, SourceId, analyze,
};

fn document(text: impl AsRef<str>) -> SourceDocument {
    SourceDocument::new(SourceId(31), RevisionId(7), "analysis.ox", text)
}

fn kinds(text: &str) -> Vec<HighlightKind> {
    analyze(&document(text))
        .expect("source should remain within analysis limits")
        .highlights
        .into_iter()
        .map(|highlight| highlight.kind)
        .collect()
}

fn assert_limit(text: impl AsRef<str>, expected: AnalysisLimit, actual: usize) {
    assert_eq!(
        analyze(&document(text)),
        Err(AnalysisError::LimitExceeded {
            limit: expected,
            max: match expected {
                AnalysisLimit::SourceBytes => MAX_ANALYSIS_SOURCE_BYTES,
                AnalysisLimit::LexicalItems => MAX_ANALYSIS_LEXICAL_ITEMS,
                AnalysisLimit::Diagnostics => MAX_ANALYSIS_DIAGNOSTICS,
                AnalysisLimit::NestingDepth => MAX_ANALYSIS_NESTING_DEPTH,
            },
            actual,
        })
    );
}

#[test]
fn invalid_source_returns_owned_lexical_analysis_without_partial_semantics() {
    let document = SourceDocument::new(
        SourceId(17),
        RevisionId(23),
        "broken.ox",
        "var answer = \"ok\"; // retained\nprint ;",
    );

    let analysis = analyze(&document).expect("ordinary compile errors remain analyzable");

    assert_eq!(analysis.source_id, SourceId(17));
    assert_eq!(analysis.revision, RevisionId(23));
    assert!(!analysis.diagnostics.is_empty());
    assert_eq!(analysis.semantic_status, SemanticStatus::Unavailable);
    assert!(analysis.symbol_occurrences.is_empty());
    assert!(
        analysis
            .highlights
            .iter()
            .any(|highlight| highlight.kind == HighlightKind::Comment)
    );
    assert!(analysis.highlights.iter().all(|highlight| {
        highlight.span.source_id == document.id
            && highlight.span.revision == document.revision
            && highlight.span.start.byte_offset <= highlight.span.end.byte_offset
            && highlight.span.end.byte_offset <= document.text.len()
            && document
                .text
                .is_char_boundary(highlight.span.start.byte_offset)
            && document
                .text
                .is_char_boundary(highlight.span.end.byte_offset)
    }));
    assert!(analysis.highlights.windows(2).all(|pair| {
        pair[0].span.start.byte_offset <= pair[1].span.start.byte_offset
            && pair[0].span.end.byte_offset <= pair[1].span.start.byte_offset
    }));
}

#[test]
fn highlights_every_lexical_category_including_inactive_keywords() {
    assert_eq!(
        kinds("case note = \"text\" + 12.5; // comment"),
        [
            HighlightKind::Keyword,
            HighlightKind::Identifier,
            HighlightKind::Operator,
            HighlightKind::String,
            HighlightKind::Operator,
            HighlightKind::Number,
            HighlightKind::Punctuation,
            HighlightKind::Comment,
        ]
    );
}

#[test]
fn all_scanned_keywords_remain_lexical_keywords() {
    let keywords = "and class case default else false for fun if nil or print return super switch this true var while";

    assert_eq!(kinds(keywords), vec![HighlightKind::Keyword; 19],);
}

#[test]
fn comments_are_distinct_from_slashes_and_include_the_eof_comment_bytes() {
    let text = "/ // first\n//second";
    let analysis = analyze(&document(text)).unwrap();

    assert_eq!(
        analysis
            .highlights
            .iter()
            .map(|highlight| highlight.kind)
            .collect::<Vec<_>>(),
        [
            HighlightKind::Operator,
            HighlightKind::Comment,
            HighlightKind::Comment,
        ]
    );
    let eof_comment = analysis.highlights.last().unwrap();
    assert_eq!(eof_comment.span.start.byte_offset, 11);
    assert_eq!(eof_comment.span.end.byte_offset, text.len());
}

#[test]
fn normalized_multiline_and_unicode_spans_use_bytes_and_scalar_columns() {
    let source = document("\u{feff}\"a\r\n😀\"\tβ@");
    let analysis = analyze(&source).unwrap();

    assert_eq!(&*source.text, "\"a\n😀\"\tβ@");
    assert_eq!(
        analysis
            .highlights
            .iter()
            .map(|highlight| highlight.kind)
            .collect::<Vec<_>>(),
        [
            HighlightKind::String,
            HighlightKind::Invalid,
            HighlightKind::Invalid,
        ]
    );
    assert_eq!(analysis.highlights[0].span.start.byte_offset, 0);
    assert_eq!(analysis.highlights[0].span.end.byte_offset, 8);
    assert_eq!(
        (
            analysis.highlights[0].span.end.line,
            analysis.highlights[0].span.end.column
        ),
        (2, 3)
    );
    assert_eq!(analysis.highlights[1].span.start.byte_offset, 9);
    assert_eq!(analysis.highlights[1].span.end.byte_offset, 11);
    assert_eq!(analysis.highlights[1].span.start.column, 4);
    assert_eq!(analysis.highlights[1].span.end.column, 5);
}

#[test]
fn unterminated_strings_keep_string_highlighting_and_scanner_diagnostics() {
    let analysis = analyze(&document("print \"unfinished\nline")).unwrap();

    assert_eq!(
        analysis.highlights.last().unwrap().kind,
        HighlightKind::String
    );
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "scanner.error")
    );
    assert_eq!(analysis.semantic_status, SemanticStatus::Unavailable);
    assert!(analysis.symbol_occurrences.is_empty());
}

#[test]
fn valid_analysis_compiles_without_executing_the_program() {
    let analysis = analyze(&document("print missing;")).unwrap();

    assert!(analysis.diagnostics.is_empty());
    assert_eq!(analysis.semantic_status, SemanticStatus::Available);
    assert!(analysis.symbol_occurrences.is_empty());
}

#[test]
fn analysis_diagnostics_match_the_ordinary_compiler_path() {
    let document = document("var = 1;\nprint ;");
    let analysis = analyze(&document).unwrap();
    let mut host = RecordingHost::default();

    let status = Interpreter::new().run(document, &mut host);

    assert_eq!(status, rlox::InterpretResult::CompileError);
    assert_eq!(analysis.diagnostics, host.diagnostics());
    assert!(host.output().is_empty());
}

#[test]
fn source_byte_limit_accepts_the_exact_boundary_and_rejects_one_more() {
    let at_limit = format!("//{}", "a".repeat(MAX_ANALYSIS_SOURCE_BYTES - 2));
    assert_eq!(at_limit.len(), MAX_ANALYSIS_SOURCE_BYTES);
    assert!(analyze(&document(&at_limit)).is_ok());

    let over_limit = format!("{at_limit}a");
    assert_limit(
        &over_limit,
        AnalysisLimit::SourceBytes,
        MAX_ANALYSIS_SOURCE_BYTES + 1,
    );
}

#[test]
fn lexical_item_limit_accepts_the_exact_boundary_and_rejects_one_more() {
    assert_eq!(MAX_ANALYSIS_LEXICAL_ITEMS % 2, 0);
    let at_limit = "nil;".repeat(MAX_ANALYSIS_LEXICAL_ITEMS / 2);
    assert_eq!(
        analyze(&document(&at_limit)).unwrap().highlights.len(),
        MAX_ANALYSIS_LEXICAL_ITEMS
    );

    let over_limit = format!("{at_limit}nil");
    assert_limit(
        &over_limit,
        AnalysisLimit::LexicalItems,
        MAX_ANALYSIS_LEXICAL_ITEMS + 1,
    );
}

#[test]
fn diagnostic_limit_accepts_the_exact_boundary_and_rejects_one_more() {
    let at_limit = "return;\n".repeat(MAX_ANALYSIS_DIAGNOSTICS);
    assert_eq!(
        analyze(&document(&at_limit)).unwrap().diagnostics.len(),
        MAX_ANALYSIS_DIAGNOSTICS
    );

    let over_limit = "return;\n".repeat(MAX_ANALYSIS_DIAGNOSTICS + 1);
    assert_limit(
        &over_limit,
        AnalysisLimit::Diagnostics,
        MAX_ANALYSIS_DIAGNOSTICS + 1,
    );
    assert_limit(
        &over_limit,
        AnalysisLimit::Diagnostics,
        MAX_ANALYSIS_DIAGNOSTICS + 1,
    );
}

#[test]
fn nesting_limit_accepts_the_exact_boundary_and_rejects_one_more() {
    let at_limit = format!(
        "{}nil{};",
        "(".repeat(MAX_ANALYSIS_NESTING_DEPTH),
        ")".repeat(MAX_ANALYSIS_NESTING_DEPTH)
    );
    assert!(analyze(&document(&at_limit)).is_ok());

    let over_limit = format!(
        "{}nil{};",
        "(".repeat(MAX_ANALYSIS_NESTING_DEPTH + 1),
        ")".repeat(MAX_ANALYSIS_NESTING_DEPTH + 1)
    );
    assert_limit(
        &over_limit,
        AnalysisLimit::NestingDepth,
        MAX_ANALYSIS_NESTING_DEPTH + 1,
    );
}

#[test]
fn nesting_preflight_counts_control_bodies_across_for_header_semicolons() {
    let at_limit = format!(
        "{}print 1;",
        "for (;;) ".repeat(MAX_ANALYSIS_NESTING_DEPTH - 1)
    );
    assert!(analyze(&document(&at_limit)).is_ok());

    let over_limit = format!("{}print 1;", "for (;;) ".repeat(MAX_ANALYSIS_NESTING_DEPTH));
    assert_limit(
        &over_limit,
        AnalysisLimit::NestingDepth,
        MAX_ANALYSIS_NESTING_DEPTH + 1,
    );
}

#[test]
fn nesting_preflight_combines_unary_and_grouping_depth() {
    let unary = MAX_ANALYSIS_NESTING_DEPTH / 2;
    let grouping = MAX_ANALYSIS_NESTING_DEPTH - unary;
    let at_limit = format!(
        "{}{}true{};",
        "!".repeat(unary),
        "(".repeat(grouping),
        ")".repeat(grouping)
    );
    assert!(analyze(&document(&at_limit)).is_ok());

    let over_limit = format!(
        "{}{}true{};",
        "!".repeat(unary),
        "(".repeat(grouping + 1),
        ")".repeat(grouping + 1)
    );
    assert_limit(
        &over_limit,
        AnalysisLimit::NestingDepth,
        MAX_ANALYSIS_NESTING_DEPTH + 1,
    );
}

#[test]
fn mismatched_closers_do_not_hide_open_delimiter_depth() {
    let braces = MAX_ANALYSIS_NESTING_DEPTH / 2;
    let parentheses = MAX_ANALYSIS_NESTING_DEPTH - braces + 1;
    let source = format!(
        "{}{}{}nil{};{}",
        "{".repeat(braces),
        ")".repeat(braces),
        "(".repeat(parentheses),
        ")".repeat(parentheses),
        "}".repeat(braces)
    );

    assert_limit(
        &source,
        AnalysisLimit::NestingDepth,
        MAX_ANALYSIS_NESTING_DEPTH + 1,
    );
}

#[test]
fn sequential_block_controls_do_not_accumulate_nesting_depth() {
    for count in [MAX_ANALYSIS_NESTING_DEPTH, MAX_ANALYSIS_NESTING_DEPTH + 1] {
        let source = "if (true) {}\n".repeat(count);
        let analysis = analyze(&document(&source))
            .unwrap_or_else(|error| panic!("{count} sequential if statements: {error:?}"));
        assert_eq!(analysis.semantic_status, SemanticStatus::Available);
    }

    let mixed = ["if (true) {}", "while (false) {}", "switch (nil) {}"]
        .into_iter()
        .cycle()
        .take(MAX_ANALYSIS_NESTING_DEPTH + 1)
        .collect::<Vec<_>>()
        .join("\n");
    let analysis = analyze(&document(mixed)).expect("sequential mixed controls are not nested");
    assert_eq!(analysis.semantic_status, SemanticStatus::Available);
}

#[test]
fn genuinely_nested_block_controls_still_enforce_the_combined_depth_limit() {
    let at_limit = MAX_ANALYSIS_NESTING_DEPTH / 2;
    let source = format!("{}{}", "if (true) {".repeat(at_limit), "}".repeat(at_limit));
    assert!(analyze(&document(source)).is_ok());

    let over_limit = at_limit + 1;
    let source = format!(
        "{}{}",
        "if (true) {".repeat(over_limit),
        "}".repeat(over_limit)
    );
    assert_limit(
        source,
        AnalysisLimit::NestingDepth,
        MAX_ANALYSIS_NESTING_DEPTH + 1,
    );
}

fn unbraced_else_if_chain(controls: usize) -> String {
    let mut source = String::from("if (true) nil;");
    for _ in 1..controls {
        source.push_str(" // completed body\nelse if (true) nil;");
    }
    source
}

#[test]
fn unbraced_else_if_chains_enforce_exact_combined_depth_128_and_129() {
    let at_limit_controls = MAX_ANALYSIS_NESTING_DEPTH - 1;
    let at_limit = unbraced_else_if_chain(at_limit_controls);
    assert!(analyze(&document(at_limit)).is_ok());

    let over_limit_controls = MAX_ANALYSIS_NESTING_DEPTH;
    let over_limit = unbraced_else_if_chain(over_limit_controls);
    assert_limit(
        over_limit,
        AnalysisLimit::NestingDepth,
        MAX_ANALYSIS_NESTING_DEPTH + 1,
    );
}

#[test]
fn sequential_unbraced_if_statements_do_not_accumulate_nesting_depth() {
    let source = "if (true) nil;\n".repeat(MAX_ANALYSIS_NESTING_DEPTH + 1);
    let analysis = analyze(&document(source)).expect("sequential controls are not nested");

    assert_eq!(analysis.semantic_status, SemanticStatus::Available);
}
