use rlox::{InterpretResult, Interpreter};

#[test]
fn library_interprets_a_source_string() {
    let mut interpreter = Interpreter::new();

    assert_eq!(
        interpreter.interpret("var answer = 42;"),
        InterpretResult::Ok
    );
}
