use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn rlox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rlox"))
}

fn source_file(source: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rlox-cli-contract-{}-{unique}.lox",
        std::process::id()
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

#[test]
fn successful_file_prints_result_and_exits_zero() {
    let output = run_file("print 2 + 3;");

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "5\n");
    assert!(output.stderr.is_empty());
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
    assert!(output.stderr.is_empty());
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
