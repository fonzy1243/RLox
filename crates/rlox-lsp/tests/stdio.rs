use std::{
    io::{BufReader, Cursor, Write},
    process::{Child, ChildStdin, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use lsp_server::Message;

fn frame(value: serde_json::Value) -> Vec<u8> {
    let body = serde_json::to_vec(&value).unwrap();
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend_from_slice(&body);
    framed
}

fn parse_stdout(stdout: Vec<u8>) -> Vec<Message> {
    let mut reader = BufReader::new(Cursor::new(stdout));
    let mut messages = Vec::new();
    while let Some(message) = Message::read(&mut reader).expect("stdout contains only LSP frames") {
        messages.push(message);
    }
    messages
}

fn wait_for_child(mut child: Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().expect("child status is readable") {
            Some(_) => return child.wait_with_output().expect("child output is readable"),
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            None => {
                let _ = child.kill();
                let output = child
                    .wait_with_output()
                    .expect("timed-out child output is readable");
                panic!(
                    "child did not exit within five seconds; stdout: {}; stderr: {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
}

fn spawn_child(input: &[u8]) -> (Child, ChildStdin) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rlox-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input).unwrap();
    (child, stdin)
}

fn run_child(input: &[u8]) -> Output {
    let (child, stdin) = spawn_child(input);
    drop(stdin);
    wait_for_child(child)
}

#[test]
fn child_process_completes_a_framed_language_session_without_raw_stdout() {
    let uri = "file:///workspace/child.lox";
    let input = [
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"capabilities": {}}
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "lox",
                    "version": 1,
                    "text": "var name = \"😀\"; print name;"
                }
            }
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": "tokens",
            "method": "textDocument/semanticTokens/full",
            "params": {"textDocument": {"uri": uri}}
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "shutdown",
            "params": null
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }),
    ]
    .into_iter()
    .flat_map(frame)
    .collect::<Vec<_>>();

    let output = run_child(&input);
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());

    let messages = parse_stdout(output.stdout);
    assert_eq!(messages.len(), 4, "{messages:#?}");
    let serialized = messages
        .iter()
        .map(|message| serde_json::to_value(message).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(serialized[0]["id"], 1);
    assert_eq!(
        serialized[0]["result"]["capabilities"]["positionEncoding"],
        "utf-16"
    );
    assert_eq!(serialized[1]["method"], "textDocument/publishDiagnostics");
    assert_eq!(serialized[1]["params"]["version"], 1);
    assert_eq!(serialized[2]["id"], "tokens");
    assert!(serialized[2]["result"]["data"].as_array().unwrap().len() >= 5);
    assert_eq!(serialized[3]["id"], 3);
    assert_eq!(serialized[3]["result"], serde_json::Value::Null);
}

#[test]
fn malformed_frame_exits_nonzero_without_emitting_stdout_noise() {
    let output = run_child(b"Content-Length: nope\r\n\r\n");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    assert!(!output.stderr.is_empty());
}

#[test]
fn rejected_initialization_response_is_flushed_before_child_exit() {
    for iteration in 0..24 {
        let (id, params) = if iteration % 2 == 0 {
            (41, serde_json::json!({"capabilities": 7}))
        } else {
            (
                42,
                serde_json::json!({
                    "capabilities": {
                        "general": {"positionEncodings": ["utf-8"]}
                    }
                }),
            )
        };
        let input = frame(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": params
        }));

        let output = run_child(&input);
        assert!(!output.status.success(), "iteration {iteration}");
        assert!(!output.stderr.is_empty(), "iteration {iteration}");
        let messages = parse_stdout(output.stdout);
        assert_eq!(messages.len(), 1, "iteration {iteration}: {messages:#?}");
        let serialized = serde_json::to_value(&messages[0]).unwrap();
        assert_eq!(serialized["id"], id, "iteration {iteration}");
        assert_eq!(serialized["error"]["code"], -32602, "iteration {iteration}");
    }
}

#[test]
fn request_named_exit_during_initialization_exits_while_stdin_remains_open() {
    let input = [
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"capabilities": {}}
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "exit",
            "params": null
        }),
    ]
    .into_iter()
    .flat_map(frame)
    .collect::<Vec<_>>();
    let (child, _open_stdin) = spawn_child(&input);

    let output = wait_for_child(child);
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "rlox-lsp: expected initialized notification, got request method: exit\n"
    );
    let messages = parse_stdout(output.stdout);
    assert_eq!(messages.len(), 1, "{messages:#?}");
    let response = serde_json::to_value(&messages[0]).unwrap();
    assert_eq!(response["id"], 1);
    assert!(response.get("result").is_some());
}

#[test]
fn initialization_protocol_errors_escape_request_methods_on_stderr() {
    let input = [
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"capabilities": {}}
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "exit\nINJECTED\u{1b}[31m",
            "params": null
        }),
    ]
    .into_iter()
    .flat_map(frame)
    .collect::<Vec<_>>();

    let output = run_child(&input);
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "rlox-lsp: expected initialized notification, got request method: exit\\nINJECTED\\u{1b}[31m\n"
    );
}
