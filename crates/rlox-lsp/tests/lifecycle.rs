use std::thread;

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::{
    ClientCapabilities, GeneralClientCapabilities, InitializeParams, InitializeResult,
    InitializedParams, PositionEncodingKind, TextDocumentClientCapabilities,
    notification::{Exit, Initialized, Notification as _},
    request::{GotoDefinition, Request as _, Shutdown},
};
use rlox_lsp::{ServerError, ServerOutcome, run_connection};

struct TestServer {
    client: Connection,
    thread: thread::JoinHandle<ServerOutcome>,
}

impl TestServer {
    fn start() -> Self {
        let (server, client) = Connection::memory();
        let thread = thread::spawn(move || run_connection(server).expect("server run succeeds"));
        Self { client, thread }
    }

    fn send(&self, message: impl Into<Message>) {
        self.client.sender.send(message.into()).unwrap();
    }

    fn receive(&self) -> Message {
        self.client.receiver.recv().unwrap()
    }

    fn initialize(&self, capabilities: ClientCapabilities) -> InitializeResult {
        self.send(Request::new(
            1.into(),
            "initialize".to_owned(),
            InitializeParams {
                capabilities,
                ..InitializeParams::default()
            },
        ));
        let Message::Response(Response {
            id,
            response_result: Ok(value),
        }) = self.receive()
        else {
            panic!("expected initialize response");
        };
        assert_eq!(id, 1.into());
        let result = serde_json::from_value(value).unwrap();
        self.send(Notification::new(
            Initialized::METHOD.to_owned(),
            InitializedParams {},
        ));
        result
    }

    fn shutdown_and_exit(self) -> ServerOutcome {
        self.send(Request::new(2.into(), Shutdown::METHOD.to_owned(), ()));
        let Message::Response(response) = self.receive() else {
            panic!("expected shutdown response");
        };
        assert_eq!(response.id, 2.into());
        assert_eq!(response.response_result.unwrap(), serde_json::Value::Null);
        self.send(Notification::new(Exit::METHOD.to_owned(), ()));
        drop(self.client);
        self.thread.join().unwrap()
    }
}

#[test]
fn initialize_advertises_only_the_supported_utf16_language_features() {
    let server = TestServer::start();
    let capabilities = ClientCapabilities {
        general: Some(GeneralClientCapabilities {
            position_encodings: Some(vec![
                PositionEncodingKind::UTF8,
                PositionEncodingKind::UTF16,
            ]),
            ..GeneralClientCapabilities::default()
        }),
        text_document: Some(TextDocumentClientCapabilities::default()),
        ..ClientCapabilities::default()
    };

    let result = server.initialize(capabilities);
    let server_info = result.server_info.unwrap();
    assert_eq!(server_info.name, "rlox-lsp");
    assert_eq!(
        server_info.version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        result.capabilities.position_encoding,
        Some(PositionEncodingKind::UTF16)
    );

    let serialized = serde_json::to_value(result.capabilities).unwrap();
    assert_eq!(
        serialized["textDocumentSync"],
        serde_json::json!({"openClose": true, "change": 1})
    );
    assert_eq!(serialized["definitionProvider"], true);
    assert!(serialized["textDocumentSync"].get("save").is_none());
    assert!(serialized["semanticTokensProvider"].get("range").is_none());
    assert_eq!(serialized["semanticTokensProvider"]["full"], true);
    assert_eq!(
        serialized["semanticTokensProvider"]["legend"]["tokenTypes"],
        serde_json::json!([
            "keyword", "comment", "string", "number", "variable", "operator"
        ])
    );

    assert_eq!(server.shutdown_and_exit(), ServerOutcome::CleanExit);
}

#[test]
fn omission_of_position_encodings_defaults_to_utf16() {
    let server = TestServer::start();
    let result = server.initialize(ClientCapabilities::default());
    assert_eq!(
        result.capabilities.position_encoding,
        Some(PositionEncodingKind::UTF16)
    );
    assert_eq!(server.shutdown_and_exit(), ServerOutcome::CleanExit);
}

#[test]
fn exit_before_shutdown_and_channel_close_are_unsuccessful_outcomes() {
    let early_exit = TestServer::start();
    early_exit.initialize(ClientCapabilities::default());
    early_exit.send(Notification::new(Exit::METHOD.to_owned(), ()));
    drop(early_exit.client);
    assert_eq!(
        early_exit.thread.join().unwrap(),
        ServerOutcome::ExitWithoutShutdown
    );

    let closed = TestServer::start();
    closed.initialize(ClientCapabilities::default());
    drop(closed.client);
    assert_eq!(closed.thread.join().unwrap(), ServerOutcome::ChannelClosed);
}

#[test]
fn exit_during_either_initialization_phase_is_unsuccessful() {
    let before_initialize = TestServer::start();
    before_initialize.send(Notification::new(Exit::METHOD.to_owned(), ()));
    drop(before_initialize.client);
    assert_eq!(
        before_initialize.thread.join().unwrap(),
        ServerOutcome::ExitWithoutShutdown
    );

    let before_initialized = TestServer::start();
    before_initialized.send(Request::new(
        1.into(),
        "initialize".to_owned(),
        InitializeParams::default(),
    ));
    assert!(matches!(before_initialized.receive(), Message::Response(_)));
    before_initialized.send(Notification::new(Exit::METHOD.to_owned(), ()));
    drop(before_initialized.client);
    assert_eq!(
        before_initialized.thread.join().unwrap(),
        ServerOutcome::ExitWithoutShutdown
    );
}

#[test]
fn request_named_exit_during_initialization_is_a_protocol_error() {
    let (server, client) = Connection::memory();
    let thread = thread::spawn(move || run_connection(server));
    client
        .sender
        .send(
            Request::new(
                1.into(),
                "initialize".to_owned(),
                InitializeParams::default(),
            )
            .into(),
        )
        .unwrap();
    assert!(matches!(
        client.receiver.recv().unwrap(),
        Message::Response(_)
    ));
    client
        .sender
        .send(Request::new(2.into(), Exit::METHOD.to_owned(), ()).into())
        .unwrap();
    drop(client);

    let result = thread.join().unwrap();
    assert_eq!(
        result,
        Err(ServerError::Protocol(
            "expected initialized notification, got request method: exit".to_owned()
        ))
    );
}

#[test]
fn explicit_position_encodings_without_utf16_are_rejected() {
    let (server, client) = Connection::memory();
    let thread = thread::spawn(move || run_connection(server));
    client
        .sender
        .send(
            Request::new(
                7.into(),
                "initialize".to_owned(),
                InitializeParams {
                    capabilities: ClientCapabilities {
                        general: Some(GeneralClientCapabilities {
                            position_encodings: Some(vec![PositionEncodingKind::UTF8]),
                            ..GeneralClientCapabilities::default()
                        }),
                        ..ClientCapabilities::default()
                    },
                    ..InitializeParams::default()
                },
            )
            .into(),
        )
        .unwrap();
    let Message::Response(response) = client.receiver.recv().unwrap() else {
        panic!("expected initialize error response");
    };
    assert_eq!(response.id, 7.into());
    assert_eq!(response.response_result.unwrap_err().code, -32602);
    drop(client);
    assert_eq!(
        thread.join().unwrap().unwrap_err(),
        ServerError::UnsupportedPositionEncoding
    );
}

#[test]
fn shutdown_is_single_use_and_blocks_all_further_language_work() {
    let server = TestServer::start();
    server.initialize(ClientCapabilities::default());
    server.send(Request::new(10.into(), Shutdown::METHOD.to_owned(), ()));
    let Message::Response(first) = server.receive() else {
        panic!("expected shutdown response");
    };
    assert_eq!(first.response_result.unwrap(), serde_json::Value::Null);

    server.send(Notification::new(
        "textDocument/didOpen".to_owned(),
        serde_json::json!({"ignored": true}),
    ));
    for (id, method) in [
        (11, Shutdown::METHOD),
        (12, GotoDefinition::METHOD),
        (13, "unknown/request"),
    ] {
        server.send(Request::new(
            id.into(),
            method.to_owned(),
            serde_json::Value::Null,
        ));
        let Message::Response(response) = server.receive() else {
            panic!("expected post-shutdown error");
        };
        assert_eq!(response.id, id.into());
        assert_eq!(response.response_result.unwrap_err().code, -32600);
    }

    server.send(Notification::new(Exit::METHOD.to_owned(), ()));
    drop(server.client);
    assert_eq!(server.thread.join().unwrap(), ServerOutcome::CleanExit);
}

#[test]
fn malformed_known_and_unknown_requests_return_correlated_errors_and_service_continues() {
    let server = TestServer::start();
    server.initialize(ClientCapabilities::default());

    server.send(Request::new(
        "bad-definition".to_owned().into(),
        GotoDefinition::METHOD.to_owned(),
        serde_json::json!({"textDocument": {"uri": 4}}),
    ));
    let Message::Response(malformed) = server.receive() else {
        panic!("expected malformed request response");
    };
    assert_eq!(malformed.id, "bad-definition".to_owned().into());
    assert_eq!(malformed.response_result.unwrap_err().code, -32602);

    server.send(Notification::new(
        "unknown/notification".to_owned(),
        serde_json::json!({"anything": true}),
    ));
    server.send(Request::new(
        22.into(),
        "unknown/request".to_owned(),
        serde_json::Value::Null,
    ));
    let Message::Response(unknown) = server.receive() else {
        panic!("expected unknown request response");
    };
    assert_eq!(unknown.id, 22.into());
    assert_eq!(unknown.response_result.unwrap_err().code, -32601);

    assert_eq!(server.shutdown_and_exit(), ServerOutcome::CleanExit);
}

#[test]
fn malformed_shutdown_does_not_enter_shutdown_state() {
    let server = TestServer::start();
    server.initialize(ClientCapabilities::default());
    server.send(Request::new(
        30.into(),
        Shutdown::METHOD.to_owned(),
        serde_json::json!({"unexpected": true}),
    ));
    let Message::Response(malformed) = server.receive() else {
        panic!("expected malformed shutdown response");
    };
    assert_eq!(malformed.response_result.unwrap_err().code, -32602);

    server.send(Request::new(31.into(), "unknown/request".to_owned(), ()));
    let Message::Response(still_running) = server.receive() else {
        panic!("expected method-not-found response");
    };
    assert_eq!(still_running.response_result.unwrap_err().code, -32601);
    assert_eq!(server.shutdown_and_exit(), ServerOutcome::CleanExit);
}
