use std::fs;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static SOURCE_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn rlox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rlox"))
}

fn source_file(source: &str) -> std::path::PathBuf {
    let sequence = SOURCE_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rlox-cli-contract-{}-{unique}-{sequence}.lox",
        std::process::id(),
    ));
    fs::write(&path, source).unwrap();
    path
}

fn run_file(source: &str) -> std::process::Output {
    let path = source_file(source);
    let output = rlox().arg(&path).output().unwrap();
    fs::remove_file(path).unwrap();
    output
}

fn run_file_with_timeout(source: &str, timeout: Duration) -> std::process::Output {
    let path = source_file(source);
    let mut child = rlox()
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stdout_reader = thread::spawn(|| drain_capped(stdout, 64 * 1024));
    let stderr_reader = thread::spawn(|| drain_capped(stderr, 1024 * 1024));
    let started = Instant::now();

    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let output = std::process::Output {
                status,
                stdout: stdout_reader.join().unwrap(),
                stderr: stderr_reader.join().unwrap(),
            };
            fs::remove_file(path).unwrap();
            return output;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let status = child.wait().unwrap();
            let output = std::process::Output {
                status,
                stdout: stdout_reader.join().unwrap(),
                stderr: stderr_reader.join().unwrap(),
            };
            fs::remove_file(path).unwrap();
            panic!(
                "subprocess exceeded {timeout:?}; captured {} stdout bytes",
                output.stdout.len()
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn drain_capped(mut reader: impl Read, capture_limit: usize) -> Vec<u8> {
    let mut captured = Vec::new();
    let mut buffer = [0; 8 * 1024];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            return captured;
        }
        let remaining = capture_limit.saturating_sub(captured.len());
        captured.extend_from_slice(&buffer[..count.min(remaining)]);
    }
}

#[cfg(not(any(
    feature = "debug_print_code",
    feature = "debug_trace_execution",
    feature = "debug_print_tokens",
    feature = "debug_log_gc"
)))]
fn assert_no_unexpected_stderr(stderr: &[u8]) {
    assert!(
        stderr.is_empty(),
        "unexpected stderr: {}",
        String::from_utf8_lossy(stderr)
    );
}

#[cfg(any(
    feature = "debug_print_code",
    feature = "debug_trace_execution",
    feature = "debug_print_tokens",
    feature = "debug_log_gc"
))]
fn assert_no_unexpected_stderr(_stderr: &[u8]) {}

#[test]
fn successful_file_prints_result_and_exits_zero() {
    let output = run_file("print 2 + 3;");

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "5\n");
    assert_no_unexpected_stderr(&output.stderr);
}

#[test]
fn compile_failure_exits_65() {
    let output = run_file("var =;");

    assert_eq!(output.status.code(), Some(65));
}

#[test]
fn runtime_failure_exits_70() {
    let output = run_file("print 1 + true;");

    assert_eq!(output.status.code(), Some(70));
}

#[test]
fn missing_file_exits_74() {
    let path = std::env::temp_dir().join(format!(
        "rlox-cli-contract-missing-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let output = rlox().arg(path).output().unwrap();

    assert_eq!(output.status.code(), Some(74));
}

#[test]
fn extra_argument_exits_64() {
    let output = rlox().args(["one", "two"]).output().unwrap();

    assert_eq!(output.status.code(), Some(64));
}

#[test]
fn exact_debug_worker_bootstrap_emits_protocol_hello() {
    let output = rlox()
        .args(["--debug-worker", "--worker-session", "42"])
        .stdin(Stdio::null())
        .output()
        .expect("run debug worker bootstrap");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"version\":1,\"worker_session_id\":42,\"run_id\":0,\"source_revision\":0,\"request_id\":0,\"sequence\":1,\"payload\":{\"kind\":\"hello\"}}\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn repl_keeps_globals_between_lines() {
    let mut child = rlox()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"var x = 7;\nprint x;\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout).unwrap().contains("7\n"));
    assert_no_unexpected_stderr(&output.stderr);
}

#[test]
fn repl_recovers_after_runtime_error() {
    let mut child = rlox()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"print missing;\nprint 1;\n").unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let repl_output = stdout.replace("> ", "");

    assert!(output.status.success());
    assert!(repl_output.trim_end().ends_with('1'));
}

#[test]
fn cyclic_list_prints_a_cycle_marker() {
    let output = run_file("var a=[nil]; a[0]=a; print a;");
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(output.status.success());
    assert!(stdout.contains("<cycle>"));
}

#[test]
fn aliased_lists_are_not_reported_as_cycles() {
    let output = run_file("var child=[1]; print [child,child];");
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(output.status.success());
    assert_eq!(stdout, "[[1], [1]]\n");
}

#[test]
fn shared_list_dag_printing_is_globally_bounded() {
    let mut source = String::from("var a0=[0];");
    for level in 1..=30 {
        source.push_str(&format!("var a{level}=[a{},a{}];", level - 1, level - 1));
    }
    source.push_str("print a30;");

    let output = run_file_with_timeout(&source, Duration::from_secs(2));
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(output.status.success());
    assert!(stdout.contains("<truncated>"));
    assert!(
        stdout.len() <= 8 * 1024 + 1,
        "{} stdout bytes",
        stdout.len()
    );
}

#[test]
fn long_string_printing_respects_the_global_byte_budget() {
    let value = "x".repeat(20 * 1024);
    let source = format!("print \"{value}\";");
    let output = run_file(&source);
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(output.status.success());
    assert!(stdout.contains("<truncated>"));
    assert!(
        stdout.len() <= 8 * 1024 + 1,
        "{} stdout bytes",
        stdout.len()
    );
}

#[cfg(not(any(feature = "debug_trace_execution", feature = "debug_log_gc")))]
#[test]
fn stack_heavy_recursion_exits_with_runtime_error() {
    let declarations = (0..255)
        .map(|index| format!("var local{index};"))
        .collect::<String>();
    let source = format!("fun recurse() {{{declarations} return 1 + recurse();}} recurse();");
    let output = run_file(&source);
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert_eq!(output.status.code(), Some(70));
    assert!(stderr.contains("Stack overflow."));
}

#[cfg(any(
    feature = "debug_print_code",
    feature = "debug_trace_execution",
    feature = "debug_print_tokens",
    feature = "debug_stress_gc",
    feature = "debug_log_gc"
))]
#[test]
fn developer_tracing_does_not_change_program_stdout() {
    let output = run_file("print 2 + 3;");

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "5\n");
    #[cfg(any(
        feature = "debug_print_code",
        feature = "debug_trace_execution",
        feature = "debug_print_tokens",
        feature = "debug_log_gc"
    ))]
    assert!(!output.stderr.is_empty());
}
