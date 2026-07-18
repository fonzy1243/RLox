mod support;

use lsp_server::Notification;
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    GotoDefinitionResponse, Location, Position, SemanticToken, SemanticTokens,
    SemanticTokensResult, TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    VersionedTextDocumentIdentifier,
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    },
    request::{GotoDefinition, Request as _, SemanticTokensFullRequest},
};
use rlox::{
    MAX_ANALYSIS_DIAGNOSTICS, MAX_ANALYSIS_LEXICAL_ITEMS, MAX_ANALYSIS_NESTING_DEPTH,
    MAX_ANALYSIS_SOURCE_BYTES,
};
use rlox_lsp::ServerOutcome;
use support::TestServer;

fn uri(name: &str) -> lsp_types::Uri {
    format!("file:///workspace/{name}").parse().unwrap()
}

fn open(
    server: &TestServer,
    name: &str,
    version: i32,
    text: impl Into<String>,
) -> lsp_types::PublishDiagnosticsParams {
    server.send(Notification::new(
        DidOpenTextDocument::METHOD.to_owned(),
        DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri(name),
                language_id: "lox".to_owned(),
                version,
                text: text.into(),
            },
        },
    ));
    let published = server.receive_diagnostics();
    assert_eq!(published.version, Some(version));
    published
}

fn change(server: &TestServer, name: &str, version: i32, text: impl Into<String>) {
    server.send(Notification::new(
        DidChangeTextDocument::METHOD.to_owned(),
        DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri(name),
                version,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.into(),
            }],
        },
    ));
    assert_eq!(server.receive_diagnostics().version, Some(version));
}

fn semantic_tokens(server: &mut TestServer, name: &str) -> SemanticTokens {
    let response = server.request(
        SemanticTokensFullRequest::METHOD,
        serde_json::json!({"textDocument": {"uri": uri(name)}}),
    );
    let result: Option<SemanticTokensResult> =
        serde_json::from_value(response.response_result.unwrap()).unwrap();
    let Some(SemanticTokensResult::Tokens(tokens)) = result else {
        panic!("expected full semantic tokens");
    };
    tokens
}

fn definition(server: &mut TestServer, name: &str, position: Position) -> Vec<Location> {
    let response = server.request(
        GotoDefinition::METHOD,
        serde_json::json!({
            "textDocument": {"uri": uri(name)},
            "position": position,
        }),
    );
    let result: Option<GotoDefinitionResponse> =
        serde_json::from_value(response.response_result.unwrap()).unwrap();
    let Some(GotoDefinitionResponse::Array(locations)) = result else {
        panic!("definition must always return an array");
    };
    locations
}

fn position_of(source: &str, needle: &str, occurrence: usize) -> Position {
    let mut start = 0usize;
    let mut byte = 0usize;
    for _ in 0..=occurrence {
        let relative = source[start..].find(needle).unwrap();
        byte = start + relative;
        start = byte + needle.len();
    }
    position_at(source, byte)
}

fn position_at(source: &str, target_byte: usize) -> Position {
    let mut byte = 0usize;
    let mut line = 0u32;
    let mut character = 0u32;
    while byte < target_byte {
        let remaining = &source[byte..target_byte];
        if remaining.starts_with("\r\n") {
            byte += 2;
            line += 1;
            character = 0;
        } else if remaining.starts_with('\r') || remaining.starts_with('\n') {
            byte += 1;
            line += 1;
            character = 0;
        } else {
            let scalar = remaining.chars().next().unwrap();
            byte += scalar.len_utf8();
            character += u32::try_from(scalar.len_utf16()).unwrap();
        }
    }
    Position::new(line, character)
}

fn target_range(source: &str, needle: &str, occurrence: usize) -> lsp_types::Range {
    let start = position_of(source, needle, occurrence);
    let length = needle.encode_utf16().count() as u32;
    lsp_types::Range::new(start, Position::new(start.line, start.character + length))
}

#[test]
fn semantic_tokens_use_stable_utf16_deltas_and_omit_punctuation() {
    let mut server = TestServer::start(None);
    let source = "\u{feff}var greeting = \"😀\";\r\n// note\rprint greeting + 42;";
    open(&server, "tokens.lox", 1, source);
    let tokens = semantic_tokens(&mut server, "tokens.lox");
    assert_eq!(tokens.result_id, None);
    assert_eq!(
        tokens.data,
        vec![
            SemanticToken {
                delta_line: 0,
                delta_start: 1,
                length: 3,
                token_type: 0,
                token_modifiers_bitset: 0
            },
            SemanticToken {
                delta_line: 0,
                delta_start: 4,
                length: 8,
                token_type: 4,
                token_modifiers_bitset: 0
            },
            SemanticToken {
                delta_line: 0,
                delta_start: 9,
                length: 1,
                token_type: 5,
                token_modifiers_bitset: 0
            },
            SemanticToken {
                delta_line: 0,
                delta_start: 2,
                length: 4,
                token_type: 2,
                token_modifiers_bitset: 0
            },
            SemanticToken {
                delta_line: 1,
                delta_start: 0,
                length: 7,
                token_type: 1,
                token_modifiers_bitset: 0
            },
            SemanticToken {
                delta_line: 1,
                delta_start: 0,
                length: 5,
                token_type: 0,
                token_modifiers_bitset: 0
            },
            SemanticToken {
                delta_line: 0,
                delta_start: 6,
                length: 8,
                token_type: 4,
                token_modifiers_bitset: 0
            },
            SemanticToken {
                delta_line: 0,
                delta_start: 9,
                length: 1,
                token_type: 5,
                token_modifiers_bitset: 0
            },
            SemanticToken {
                delta_line: 0,
                delta_start: 2,
                length: 2,
                token_type: 3,
                token_modifiers_bitset: 0
            },
        ]
    );
    assert_eq!(server.shutdown(), ServerOutcome::CleanExit);
}

#[test]
fn multiline_strings_split_at_mixed_line_endings_and_invalid_programs_keep_lexical_tokens() {
    let mut server = TestServer::start(None);
    open(&server, "multiline.lox", 1, "\"a😀\r\nb\r\";");
    assert_eq!(
        semantic_tokens(&mut server, "multiline.lox").data,
        vec![
            SemanticToken {
                delta_line: 0,
                delta_start: 0,
                length: 4,
                token_type: 2,
                token_modifiers_bitset: 0
            },
            SemanticToken {
                delta_line: 1,
                delta_start: 0,
                length: 1,
                token_type: 2,
                token_modifiers_bitset: 0
            },
            SemanticToken {
                delta_line: 1,
                delta_start: 0,
                length: 1,
                token_type: 2,
                token_modifiers_bitset: 0
            },
        ]
    );

    change(&server, "multiline.lox", 2, "var = 1;");
    assert_eq!(
        semantic_tokens(&mut server, "multiline.lox").data,
        vec![
            SemanticToken {
                delta_line: 0,
                delta_start: 0,
                length: 3,
                token_type: 0,
                token_modifiers_bitset: 0
            },
            SemanticToken {
                delta_line: 0,
                delta_start: 4,
                length: 1,
                token_type: 5,
                token_modifiers_bitset: 0
            },
            SemanticToken {
                delta_line: 0,
                delta_start: 2,
                length: 1,
                token_type: 3,
                token_modifiers_bitset: 0
            },
        ]
    );

    change(&server, "multiline.lox", 3, "return;");
    assert_eq!(
        semantic_tokens(&mut server, "multiline.lox").data,
        vec![SemanticToken {
            delta_line: 0,
            delta_start: 0,
            length: 6,
            token_type: 0,
            token_modifiers_bitset: 0,
        }]
    );

    change(&server, "multiline.lox", 4, "@ var good = 1;\n");
    let invalid_scanner_tokens = semantic_tokens(&mut server, "multiline.lox").data;
    assert_eq!(invalid_scanner_tokens[0].delta_start, 2);
    assert!(invalid_scanner_tokens.iter().all(|token| token.length != 0));
    assert_eq!(server.shutdown(), ServerOutcome::CleanExit);
}

#[test]
fn shadowed_locals_resolve_to_the_declaration_visible_at_each_use() {
    let mut server = TestServer::start(None);
    let source = "var item = 0; { print item; var item = 1; print item; }";
    open(&server, "shadow.lox", 1, source);
    let outer = definition(&mut server, "shadow.lox", position_of(source, "item", 1));
    let inner = definition(&mut server, "shadow.lox", position_of(source, "item", 3));
    assert_eq!(outer[0].range, target_range(source, "item", 0));
    assert_eq!(inner[0].range, target_range(source, "item", 2));
    assert_eq!(server.shutdown(), ServerOutcome::CleanExit);
}

#[test]
fn definitions_follow_exact_local_parameter_capture_and_recursive_resolution() {
    let mut server = TestServer::start(None);
    let source = concat!(
        "{\n",
        "  var captured = 1;\n",
        "  fun recurse(n) {\n",
        "    if (n > 0) recurse(n - 1);\n",
        "    fun middle() { fun inner() { print captured; print n; } inner(); }\n",
        "    middle();\n",
        "  }\n",
        "  recurse(2);\n",
        "}\n",
    );
    open(&server, "locals.lox", 1, source);

    let cases = [
        ("recurse", 1, "recurse", 0, 7),
        ("recurse", 2, "recurse", 0, 7),
        ("captured", 1, "captured", 0, 8),
        ("n;", 0, "n)", 0, 1),
        ("captured", 0, "captured", 0, 8),
    ];
    for (cursor, cursor_occurrence, target, target_occurrence, target_length) in cases {
        let locations = definition(
            &mut server,
            "locals.lox",
            position_of(source, cursor, cursor_occurrence),
        );
        assert_eq!(
            locations.len(),
            1,
            "{cursor} occurrence {cursor_occurrence}"
        );
        assert_eq!(locations[0].uri, uri("locals.lox"));
        let target_start = position_of(source, target, target_occurrence);
        assert_eq!(locations[0].range.start, target_start);
        assert_eq!(
            locations[0].range.end,
            Position::new(target_start.line, target_start.character + target_length)
        );
    }
    assert_eq!(server.shutdown(), ServerOutcome::CleanExit);
}

#[test]
fn definitions_cover_forward_and_duplicate_globals_builtins_and_unresolved_names() {
    let mut server = TestServer::start(None);
    let source = concat!(
        "print future; var future = 1; print future;\n",
        "var duplicate = 1; fun duplicate() {} print duplicate;\n",
        "print clock; print missing;\n",
    );
    open(&server, "globals.lox", 1, source);

    for occurrence in [0, 2] {
        let locations = definition(
            &mut server,
            "globals.lox",
            position_of(source, "future", occurrence),
        );
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].range, target_range(source, "future", 1));
    }

    let duplicate = definition(
        &mut server,
        "globals.lox",
        position_of(source, "duplicate", 2),
    );
    assert_eq!(
        duplicate
            .iter()
            .map(|location| location.range)
            .collect::<Vec<_>>(),
        vec![
            target_range(source, "duplicate", 0),
            target_range(source, "duplicate", 1),
        ]
    );
    for name in ["clock", "missing"] {
        assert!(definition(&mut server, "globals.lox", position_of(source, name, 0)).is_empty());
    }
    assert_eq!(server.shutdown(), ServerOutcome::CleanExit);
}

#[test]
fn invalid_positions_programs_limits_and_closed_documents_return_exact_empty_features() {
    let mut server = TestServer::start(Some(true));
    let source = "\u{feff}\"😀\"; var name = 1; print name;";
    open(&server, "edges.lox", 1, source);
    assert!(definition(&mut server, "edges.lox", Position::new(0, 0)).is_empty());
    assert!(definition(&mut server, "edges.lox", Position::new(0, 2)).is_empty());
    assert!(definition(&mut server, "edges.lox", Position::new(0, u32::MAX)).is_empty());
    assert!(definition(&mut server, "edges.lox", Position::new(99, 0)).is_empty());
    let valid = definition(&mut server, "edges.lox", position_of(source, "name", 1));
    assert_eq!(valid[0].range, target_range(source, "name", 0));

    change(
        &server,
        "edges.lox",
        2,
        "var valid = 1; print valid; var = 2;",
    );
    assert!(definition(&mut server, "edges.lox", Position::new(0, 21)).is_empty());
    assert!(!semantic_tokens(&mut server, "edges.lox").data.is_empty());

    change(
        &server,
        "edges.lox",
        3,
        "x".repeat(MAX_ANALYSIS_SOURCE_BYTES + 1),
    );
    let limit_tokens = semantic_tokens(&mut server, "edges.lox");
    assert_eq!(
        limit_tokens,
        SemanticTokens {
            result_id: None,
            data: Vec::new()
        }
    );
    assert!(definition(&mut server, "edges.lox", Position::new(0, 0)).is_empty());
    change(&server, "edges.lox", 4, "var recovered = 1;");
    assert!(semantic_tokens(&mut server, "edges.lox").data.len() >= 3);

    server.send(Notification::new(
        DidCloseTextDocument::METHOD.to_owned(),
        DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier {
                uri: uri("edges.lox"),
            },
        },
    ));
    assert!(server.receive_diagnostics().diagnostics.is_empty());
    assert_eq!(semantic_tokens(&mut server, "edges.lox").data, Vec::new());
    assert!(definition(&mut server, "edges.lox", Position::new(0, 0)).is_empty());
    assert_eq!(server.shutdown(), ServerOutcome::CleanExit);
}

#[test]
fn every_analysis_limit_becomes_one_diagnostic_without_stopping_service() {
    let server = TestServer::start(Some(true));
    let cases = [
        (
            "source.lox",
            "x".repeat(MAX_ANALYSIS_SOURCE_BYTES + 1),
            "analysis.limit.source_bytes",
        ),
        (
            "lexical.lox",
            "nil;".repeat(MAX_ANALYSIS_LEXICAL_ITEMS / 2 + 1),
            "analysis.limit.lexical_items",
        ),
        (
            "diagnostics.lox",
            "return;\n".repeat(MAX_ANALYSIS_DIAGNOSTICS + 1),
            "analysis.limit.diagnostics",
        ),
        (
            "nesting.lox",
            format!(
                "{}nil;{}",
                "(".repeat(MAX_ANALYSIS_NESTING_DEPTH + 1),
                ")".repeat(MAX_ANALYSIS_NESTING_DEPTH + 1)
            ),
            "analysis.limit.nesting_depth",
        ),
    ];
    for (name, source, expected_code) in cases {
        let published = open(&server, name, 1, source);
        assert_eq!(published.diagnostics.len(), 1);
        let diagnostic = &published.diagnostics[0];
        assert_eq!(
            diagnostic.code,
            Some(lsp_types::NumberOrString::String(expected_code.to_owned()))
        );
        assert_eq!(
            diagnostic.range,
            lsp_types::Range::new(Position::new(0, 0), Position::new(0, 0))
        );
        assert_eq!(diagnostic.data, None);
    }
    assert_eq!(server.shutdown(), ServerOutcome::CleanExit);
}
