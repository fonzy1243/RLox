mod documents;
mod text_index;

use std::{error::Error, fmt};

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    GotoDefinitionParams, InitializeParams, InitializeResult, OneOf, PositionEncodingKind,
    SemanticTokenType, SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions,
    SemanticTokensParams, SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    WorkDoneProgressOptions,
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Exit, Initialized,
        Notification as _, PublishDiagnostics,
    },
    request::{GotoDefinition, Request as _, SemanticTokensFullRequest, Shutdown},
};

pub const SERVER_NAME: &str = "rlox-lsp";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerOutcome {
    CleanExit,
    ExitWithoutShutdown,
    ChannelClosed,
    OutputClosed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerError {
    InvalidInitializeParams(String),
    UnsupportedPositionEncoding,
    Protocol(String),
}

impl fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInitializeParams(message) => {
                write!(formatter, "invalid initialize parameters: {message}")
            }
            Self::UnsupportedPositionEncoding => {
                formatter.write_str("the client does not advertise UTF-16 position support")
            }
            Self::Protocol(message) => formatter.write_str(message),
        }
    }
}

impl Error for ServerError {}

enum InitializeFinish {
    Initialized,
    Stop(ServerOutcome),
    Error(ServerError),
}

pub fn run_connection(connection: Connection) -> Result<ServerOutcome, ServerError> {
    let (initialize_id, raw_params) = match connection.initialize_start() {
        Ok(value) => value,
        Err(error) => return classify_handshake_error(error),
    };
    let params = match serde_json::from_value::<InitializeParams>(raw_params) {
        Ok(params) => params,
        Err(error) => {
            if connection
                .sender
                .send(
                    Response::new_err(
                        initialize_id,
                        ErrorCode::InvalidParams as i32,
                        error.to_string(),
                    )
                    .into(),
                )
                .is_err()
            {
                return Ok(ServerOutcome::OutputClosed);
            }
            return Err(ServerError::InvalidInitializeParams(error.to_string()));
        }
    };

    let supports_utf16 = !params
        .capabilities
        .general
        .as_ref()
        .and_then(|general| general.position_encodings.as_ref())
        .is_some_and(|encodings| {
            encodings
                .iter()
                .all(|encoding| encoding != &PositionEncodingKind::UTF16)
        });
    if !supports_utf16 {
        if connection
            .sender
            .send(
                Response::new_err(
                    initialize_id,
                    ErrorCode::InvalidParams as i32,
                    "rlox-lsp requires UTF-16 position support".to_owned(),
                )
                .into(),
            )
            .is_err()
        {
            return Ok(ServerOutcome::OutputClosed);
        }
        return Err(ServerError::UnsupportedPositionEncoding);
    }

    let diagnostic_data_support = params
        .capabilities
        .text_document
        .as_ref()
        .and_then(|text_document| text_document.publish_diagnostics.as_ref())
        .and_then(|diagnostics| diagnostics.data_support)
        == Some(true);
    drop(params);

    let result = InitializeResult {
        capabilities: server_capabilities(),
        server_info: Some(ServerInfo {
            name: SERVER_NAME.to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
    };
    let result =
        serde_json::to_value(result).map_err(|error| ServerError::Protocol(error.to_string()))?;
    if send_response(&connection, Response::new_ok(initialize_id, result)).is_err() {
        return Ok(ServerOutcome::OutputClosed);
    }
    match receive_initialized(&connection) {
        InitializeFinish::Initialized => {}
        InitializeFinish::Stop(outcome) => return Ok(outcome),
        InitializeFinish::Error(error) => return Err(error),
    }

    run_initialized(connection, diagnostic_data_support)
}

fn classify_handshake_error(
    error: lsp_server::ProtocolError,
) -> Result<ServerOutcome, ServerError> {
    if error.channel_is_disconnected() {
        Ok(ServerOutcome::ChannelClosed)
    } else if is_exit_notification_handshake_error(&error.to_string()) {
        Ok(ServerOutcome::ExitWithoutShutdown)
    } else {
        Err(ServerError::Protocol(error.to_string()))
    }
}

fn is_exit_notification_handshake_error(message: &str) -> bool {
    message.starts_with(
        "expected initialize request, got Notification(Notification { method: \"exit\", params: ",
    )
}

fn receive_initialized(connection: &Connection) -> InitializeFinish {
    match connection.receiver.recv() {
        Ok(Message::Notification(notification)) if notification.method == Initialized::METHOD => {
            InitializeFinish::Initialized
        }
        Ok(Message::Notification(notification)) if notification.method == Exit::METHOD => {
            InitializeFinish::Stop(ServerOutcome::ExitWithoutShutdown)
        }
        Ok(Message::Notification(notification)) => {
            InitializeFinish::Error(ServerError::Protocol(format!(
                "expected initialized notification, got notification method: {}",
                notification.method.escape_default()
            )))
        }
        Ok(Message::Request(request)) => InitializeFinish::Error(ServerError::Protocol(format!(
            "expected initialized notification, got request method: {}",
            request.method.escape_default()
        ))),
        Ok(Message::Response(_)) => InitializeFinish::Error(ServerError::Protocol(
            "expected initialized notification, got response".to_owned(),
        )),
        Err(_) => InitializeFinish::Stop(ServerOutcome::ChannelClosed),
    }
}

fn run_initialized(
    connection: Connection,
    diagnostic_data_support: bool,
) -> Result<ServerOutcome, ServerError> {
    let mut shutdown_received = false;
    let mut documents = documents::DocumentStore::new(diagnostic_data_support);
    while let Ok(message) = connection.receiver.recv() {
        match message {
            Message::Notification(notification) if notification.method == Exit::METHOD => {
                return Ok(if shutdown_received {
                    ServerOutcome::CleanExit
                } else {
                    ServerOutcome::ExitWithoutShutdown
                });
            }
            Message::Request(request) if shutdown_received => {
                if send_response(
                    &connection,
                    Response::new_err(
                        request.id,
                        ErrorCode::InvalidRequest as i32,
                        "the server has already received shutdown".to_owned(),
                    ),
                )
                .is_err()
                {
                    return Ok(ServerOutcome::OutputClosed);
                }
            }
            Message::Notification(_) if shutdown_received => {}
            Message::Request(request) if request.method == Shutdown::METHOD => {
                let response = match serde_json::from_value::<()>(request.params) {
                    Ok(()) => {
                        shutdown_received = true;
                        Response::new_ok(request.id, serde_json::Value::Null)
                    }
                    Err(error) => Response::new_err(
                        request.id,
                        ErrorCode::InvalidParams as i32,
                        error.to_string(),
                    ),
                };
                if send_response(&connection, response).is_err() {
                    return Ok(ServerOutcome::OutputClosed);
                }
            }
            Message::Notification(notification)
                if notification.method == DidOpenTextDocument::METHOD =>
            {
                if let Ok(params) =
                    serde_json::from_value::<DidOpenTextDocumentParams>(notification.params)
                    && let Some(diagnostics) = documents.open(params)
                    && send_notification(
                        &connection,
                        Notification::new(PublishDiagnostics::METHOD.to_owned(), diagnostics),
                    )
                    .is_err()
                {
                    return Ok(ServerOutcome::OutputClosed);
                }
            }
            Message::Notification(notification)
                if notification.method == DidChangeTextDocument::METHOD =>
            {
                if let Ok(params) =
                    serde_json::from_value::<DidChangeTextDocumentParams>(notification.params)
                    && let Some(diagnostics) = documents.change(params)
                    && send_notification(
                        &connection,
                        Notification::new(PublishDiagnostics::METHOD.to_owned(), diagnostics),
                    )
                    .is_err()
                {
                    return Ok(ServerOutcome::OutputClosed);
                }
            }
            Message::Notification(notification)
                if notification.method == DidCloseTextDocument::METHOD =>
            {
                if let Ok(params) =
                    serde_json::from_value::<DidCloseTextDocumentParams>(notification.params)
                    && let Some(diagnostics) = documents.close(params)
                    && send_notification(
                        &connection,
                        Notification::new(PublishDiagnostics::METHOD.to_owned(), diagnostics),
                    )
                    .is_err()
                {
                    return Ok(ServerOutcome::OutputClosed);
                }
            }
            Message::Notification(_) => {}
            Message::Request(request) if request.method == GotoDefinition::METHOD => {
                let response =
                    deserialize_request::<GotoDefinitionParams, _, _>(request, |params| {
                        Some(documents.definition(params))
                    });
                if send_response(&connection, response).is_err() {
                    return Ok(ServerOutcome::OutputClosed);
                }
            }
            Message::Request(request) if request.method == SemanticTokensFullRequest::METHOD => {
                let response =
                    deserialize_request::<SemanticTokensParams, _, _>(request, |params| {
                        Some(documents.semantic_tokens(params))
                    });
                if send_response(&connection, response).is_err() {
                    return Ok(ServerOutcome::OutputClosed);
                }
            }
            Message::Request(request) => {
                if send_response(&connection, method_not_found(request)).is_err() {
                    return Ok(ServerOutcome::OutputClosed);
                }
            }
            Message::Response(_) => {}
        }
    }
    Ok(ServerOutcome::ChannelClosed)
}

fn send_notification(connection: &Connection, notification: Notification) -> Result<(), ()> {
    connection.sender.send(notification.into()).map_err(|_| ())
}

fn deserialize_request<Params, ResultValue, Handle>(request: Request, handle: Handle) -> Response
where
    Params: serde::de::DeserializeOwned,
    ResultValue: serde::Serialize,
    Handle: FnOnce(Params) -> ResultValue,
{
    let id = request.id;
    match serde_json::from_value::<Params>(request.params) {
        Ok(params) => Response::new_ok(id, handle(params)),
        Err(error) => Response::new_err(id, ErrorCode::InvalidParams as i32, error.to_string()),
    }
}

fn send_response(connection: &Connection, response: Response) -> Result<(), ()> {
    connection.sender.send(response.into()).map_err(|_| ())
}

fn method_not_found(request: Request) -> Response {
    Response::new_err(
        request.id,
        ErrorCode::MethodNotFound as i32,
        format!("unknown request method: {}", request.method),
    )
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                will_save: None,
                will_save_wait_until: None,
                save: None,
            },
        )),
        definition_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                work_done_progress_options: WorkDoneProgressOptions::default(),
                legend: SemanticTokensLegend {
                    token_types: vec![
                        SemanticTokenType::KEYWORD,
                        SemanticTokenType::COMMENT,
                        SemanticTokenType::STRING,
                        SemanticTokenType::NUMBER,
                        SemanticTokenType::VARIABLE,
                        SemanticTokenType::OPERATOR,
                    ],
                    token_modifiers: Vec::new(),
                },
                range: None,
                full: Some(SemanticTokensFullOptions::Bool(true)),
            },
        )),
        ..ServerCapabilities::default()
    }
}
