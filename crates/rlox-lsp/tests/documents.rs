mod support;

use lsp_server::Notification;
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams, Position,
    Range, TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    VersionedTextDocumentIdentifier,
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    },
};
use rlox_lsp::ServerOutcome;
use support::TestServer;

fn uri(name: &str) -> lsp_types::Uri {
    format!("file:///workspace/{name}").parse().unwrap()
}

fn has_code(diagnostic: &lsp_types::Diagnostic, expected: &str) -> bool {
    matches!(
        diagnostic.code.as_ref(),
        Some(lsp_types::NumberOrString::String(code)) if code == expected
    )
}

fn open(server: &TestServer, name: &str, version: i32, text: impl Into<String>) {
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
}

fn change(
    server: &TestServer,
    name: &str,
    version: i32,
    changes: Vec<TextDocumentContentChangeEvent>,
) {
    server.send(Notification::new(
        DidChangeTextDocument::METHOD.to_owned(),
        DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri(name),
                version,
            },
            content_changes: changes,
        },
    ));
}

fn full(text: impl Into<String>) -> TextDocumentContentChangeEvent {
    TextDocumentContentChangeEvent {
        range: None,
        range_length: None,
        text: text.into(),
    }
}

#[test]
fn full_sync_is_versioned_and_rejected_events_leave_state_untouched() {
    let server = TestServer::start(None);
    open(&server, "sync.lox", 4, "var = 1;");
    let opened = server.receive_diagnostics();
    assert_eq!(opened.uri, uri("sync.lox"));
    assert_eq!(opened.version, Some(4));
    assert!(has_code(&opened.diagnostics[0], "parser.error"));

    open(&server, "sync.lox", 99, "var duplicate = 1;");
    change(&server, "sync.lox", 4, vec![full("var stale = 1;")]);
    change(
        &server,
        "sync.lox",
        5,
        vec![TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(0, 0), Position::new(0, 1))),
            range_length: None,
            text: "v".to_owned(),
        }],
    );
    change(
        &server,
        "sync.lox",
        6,
        vec![full("var one = 1;"), full("var two = 2;")],
    );
    change(&server, "missing.lox", 1, vec![full("var missing = 1;")]);
    server.send(Notification::new(
        DidCloseTextDocument::METHOD.to_owned(),
        DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier {
                uri: uri("missing.lox"),
            },
        },
    ));
    server.assert_no_message();

    change(&server, "sync.lox", 5, vec![full("var ready = 1;")]);
    let changed = server.receive_diagnostics();
    assert_eq!(changed.version, Some(5));
    assert!(changed.diagnostics.is_empty());
    change(&server, "sync.lox", 5, vec![full("return;")]);
    server.assert_no_message();

    change(&server, "sync.lox", 6, vec![full("return;")]);
    let changed = server.receive_diagnostics();
    assert_eq!(changed.version, Some(6));
    assert!(has_code(&changed.diagnostics[0], "compiler.error"));

    server.send(Notification::new(
        DidCloseTextDocument::METHOD.to_owned(),
        DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier {
                uri: uri("sync.lox"),
            },
        },
    ));
    let cleared = server.receive_diagnostics();
    assert_eq!(cleared.version, Some(6));
    assert!(cleared.diagnostics.is_empty());

    open(&server, "sync.lox", 1, "var reopened = 1;");
    assert!(server.receive_diagnostics().diagnostics.is_empty());
    assert_eq!(server.shutdown(), ServerOutcome::CleanExit);
}

#[test]
fn open_document_count_is_bounded_and_capacity_recovers_after_close() {
    let server = TestServer::start(None);
    for index in 0..32 {
        open(&server, &format!("{index}.lox"), 1, "var value = 1;");
        assert!(server.receive_diagnostics().diagnostics.is_empty());
    }
    open(&server, "overflow.lox", 1, "var overflow = 1;");
    server.assert_no_message();

    server.send(Notification::new(
        DidCloseTextDocument::METHOD.to_owned(),
        DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri: uri("0.lox") },
        },
    ));
    assert!(server.receive_diagnostics().diagnostics.is_empty());
    open(&server, "overflow.lox", 1, "var overflow = 1;");
    assert!(server.receive_diagnostics().diagnostics.is_empty());
    assert_eq!(server.shutdown(), ServerOutcome::CleanExit);
}

#[test]
fn oversized_uris_are_ignored_across_document_and_request_operations() {
    let mut server = TestServer::start(None);
    let oversized_uri: lsp_types::Uri = format!("untitled:{}", "x".repeat(4 * 1024))
        .parse()
        .unwrap();
    assert!(oversized_uri.as_str().len() > 4 * 1024);

    server.send(Notification::new(
        DidOpenTextDocument::METHOD.to_owned(),
        DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: oversized_uri.clone(),
                language_id: "lox".to_owned(),
                version: 1,
                text: "var ignored = 1;".to_owned(),
            },
        },
    ));
    server.assert_no_message();
    server.send(Notification::new(
        DidChangeTextDocument::METHOD.to_owned(),
        DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: oversized_uri.clone(),
                version: 2,
            },
            content_changes: vec![full("var ignored = 2;")],
        },
    ));
    server.assert_no_message();
    server.send(Notification::new(
        DidCloseTextDocument::METHOD.to_owned(),
        DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier {
                uri: oversized_uri.clone(),
            },
        },
    ));
    server.assert_no_message();

    let tokens = server.request(
        "textDocument/semanticTokens/full",
        serde_json::json!({"textDocument": {"uri": oversized_uri}}),
    );
    assert_eq!(
        tokens.response_result.unwrap(),
        serde_json::json!({"data": []})
    );
    let definition = server.request(
        "textDocument/definition",
        serde_json::json!({
            "textDocument": {"uri": format!("untitled:{}", "y".repeat(4 * 1024))},
            "position": {"line": 0, "character": 0}
        }),
    );
    assert_eq!(definition.response_result.unwrap(), serde_json::json!([]));

    let synthetic_uri = "untitled:oxide-buffer".parse().unwrap();
    server.send(Notification::new(
        DidOpenTextDocument::METHOD.to_owned(),
        DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: synthetic_uri,
                language_id: "lox".to_owned(),
                version: 1,
                text: "var synthetic = 1;".to_owned(),
            },
        },
    ));
    assert!(server.receive_diagnostics().diagnostics.is_empty());
    open(&server, "bounded-file.lox", 1, "var file = 1;");
    assert!(server.receive_diagnostics().diagnostics.is_empty());
    assert_eq!(server.shutdown(), ServerOutcome::CleanExit);
}

#[test]
fn diagnostics_preserve_phase_severity_code_version_and_raw_utf16_ranges() {
    let server = TestServer::start(Some(true));
    let cases = [
        (
            "scanner.lox",
            "\u{feff}\"😀\" @\r\n",
            "scanner",
            "scanner.error",
            Range::new(Position::new(0, 6), Position::new(0, 7)),
        ),
        (
            "parser.lox",
            "\u{feff}//😀\r\nvar = 1;",
            "parser",
            "parser.error",
            Range::new(Position::new(1, 4), Position::new(1, 5)),
        ),
        (
            "compiler.lox",
            "//😀\rreturn;",
            "compiler",
            "compiler.error",
            Range::new(Position::new(1, 0), Position::new(1, 6)),
        ),
    ];
    for (name, text, phase, code, range) in cases {
        open(&server, name, 17, text);
        let published = server.receive_diagnostics();
        let diagnostic = published
            .diagnostics
            .iter()
            .find(|diagnostic| has_code(diagnostic, code))
            .unwrap_or_else(|| panic!("missing {code}: {:#?}", published.diagnostics));
        assert_eq!(published.version, Some(17));
        assert_eq!(diagnostic.range, range);
        assert_eq!(
            diagnostic.severity,
            Some(lsp_types::DiagnosticSeverity::ERROR)
        );
        assert_eq!(diagnostic.source.as_deref(), Some("rlox"));
        assert_eq!(diagnostic.data, Some(serde_json::json!({"phase": phase})));
    }
    assert_eq!(server.shutdown(), ServerOutcome::CleanExit);

    let without_data = TestServer::start(Some(false));
    open(&without_data, "plain.lox", 1, "var = 1;");
    assert!(
        without_data
            .receive_diagnostics()
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.data.is_none())
    );
    assert_eq!(without_data.shutdown(), ServerOutcome::CleanExit);
}
