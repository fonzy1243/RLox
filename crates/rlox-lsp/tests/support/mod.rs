#![allow(dead_code)]

use std::{thread, time::Duration};

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::{
    ClientCapabilities, InitializeParams, InitializedParams, PublishDiagnosticsParams,
    TextDocumentClientCapabilities,
    notification::{Exit, Initialized, Notification as _, PublishDiagnostics},
    request::{Request as _, Shutdown},
};
use rlox_lsp::{ServerOutcome, run_connection};

pub struct TestServer {
    pub client: Connection,
    thread: thread::JoinHandle<ServerOutcome>,
    next_request_id: i32,
}

impl TestServer {
    pub fn start(diagnostic_data_support: Option<bool>) -> Self {
        let (server, client) = Connection::memory();
        let thread = thread::spawn(move || run_connection(server).expect("server run succeeds"));
        let result = Self {
            client,
            thread,
            next_request_id: 10,
        };
        let capabilities = ClientCapabilities {
            text_document: Some(TextDocumentClientCapabilities {
                publish_diagnostics: Some(lsp_types::PublishDiagnosticsClientCapabilities {
                    data_support: diagnostic_data_support,
                    ..lsp_types::PublishDiagnosticsClientCapabilities::default()
                }),
                ..TextDocumentClientCapabilities::default()
            }),
            ..ClientCapabilities::default()
        };
        result.send(Request::new(
            1.into(),
            "initialize".to_owned(),
            InitializeParams {
                capabilities,
                ..InitializeParams::default()
            },
        ));
        let Message::Response(Response {
            response_result: Ok(_),
            ..
        }) = result.receive()
        else {
            panic!("expected successful initialize response");
        };
        result.send(Notification::new(
            Initialized::METHOD.to_owned(),
            InitializedParams {},
        ));
        result
    }

    pub fn send(&self, message: impl Into<Message>) {
        self.client.sender.send(message.into()).unwrap();
    }

    pub fn receive(&self) -> Message {
        self.client
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("server response within timeout")
    }

    pub fn receive_diagnostics(&self) -> PublishDiagnosticsParams {
        let Message::Notification(notification) = self.receive() else {
            panic!("expected diagnostics notification");
        };
        assert_eq!(notification.method, PublishDiagnostics::METHOD);
        serde_json::from_value(notification.params).unwrap()
    }

    pub fn assert_no_message(&self) {
        assert!(
            self.client
                .receiver
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "unexpected server message"
        );
    }

    pub fn request(&mut self, method: &str, params: impl serde::Serialize) -> Response {
        let id = self.next_request_id;
        self.next_request_id += 1;
        self.send(Request::new(id.into(), method.to_owned(), params));
        let Message::Response(response) = self.receive() else {
            panic!("expected response");
        };
        assert_eq!(response.id, id.into());
        response
    }

    pub fn shutdown(self) -> ServerOutcome {
        self.send(Request::new(2.into(), Shutdown::METHOD.to_owned(), ()));
        let Message::Response(response) = self.receive() else {
            panic!("expected shutdown response");
        };
        assert!(response.response_result.is_ok());
        self.send(Notification::new(Exit::METHOD.to_owned(), ()));
        drop(self.client);
        self.thread.join().unwrap()
    }
}
