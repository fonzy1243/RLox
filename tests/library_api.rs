use rlox::{InterpretResult, Interpreter};

#[test]
fn library_interprets_a_source_string() {
    let mut interpreter = Interpreter::new();

    assert_eq!(
        interpreter.interpret("var answer = 42;"),
        InterpretResult::Ok
    );
}

#[test]
fn invalid_list_get_indexes_return_runtime_errors() {
    for index in [
        "-1",
        "1.5",
        "2",
        "0/0",
        "1/0",
        "99999999999999999999999999999999999999999999",
    ] {
        let mut interpreter = Interpreter::new();
        let source = format!("var a=[10,20]; print a[{index}];");

        assert_eq!(
            interpreter.interpret(&source),
            InterpretResult::RuntimeError,
            "index {index}"
        );
    }
}

#[test]
fn invalid_list_set_indexes_return_runtime_errors() {
    for index in [
        "-1",
        "1.5",
        "2",
        "0/0",
        "1/0",
        "99999999999999999999999999999999999999999999",
    ] {
        let mut interpreter = Interpreter::new();
        let source = format!("var a=[10,20]; a[{index}]=30;");

        assert_eq!(
            interpreter.interpret(&source),
            InterpretResult::RuntimeError,
            "index {index}"
        );
    }
}

#[test]
fn list_index_boundaries_are_valid() {
    let mut interpreter = Interpreter::new();

    assert_eq!(
        interpreter.interpret("var a=[10,20]; print a[0]; print a[1]; a[0]=30; a[1]=40;"),
        InterpretResult::Ok
    );
}

#[test]
fn safety_limits_reject_excess_live_locals() {
    let declarations = (0..257)
        .map(|index| format!("var local{index};"))
        .collect::<String>();
    let source = format!("fun crowded() {{{declarations}}}");
    let mut interpreter = Interpreter::new();

    assert_eq!(
        interpreter.interpret(&source),
        InterpretResult::CompileError
    );
}

#[test]
fn safety_limits_recover_after_recursion_overflow() {
    let mut interpreter = Interpreter::new();

    assert_eq!(
        interpreter.interpret("fun recurse() { recurse(); } recurse();"),
        InterpretResult::RuntimeError
    );
    assert_eq!(interpreter.interpret("print 1;"), InterpretResult::Ok);
}

#[cfg(not(any(feature = "debug_trace_execution", feature = "debug_log_gc")))]
#[test]
fn runtime_stack_boundary_recovers_for_the_next_run() {
    let declarations = (0..255)
        .map(|index| format!("var local{index};"))
        .collect::<String>();
    let source = format!("fun recurse() {{{declarations} return 1 + recurse();}} recurse();");
    let mut interpreter = Interpreter::new();

    assert_eq!(
        interpreter.interpret(&source),
        InterpretResult::RuntimeError
    );
    assert_eq!(interpreter.interpret("print 1;"), InterpretResult::Ok);
}

#[test]
fn safety_limits_reject_excess_upvalues() {
    let middle_declarations = (0..254)
        .map(|index| format!("var middle{index};"))
        .collect::<String>();
    let middle_captures = (0..254)
        .map(|index| format!("print middle{index};"))
        .collect::<String>();
    let source = format!(
        "fun outer() {{
            var outer0;
            var outer1;
            var outer2;
            fun middle() {{
                {middle_declarations}
                fun inner() {{
                    {middle_captures}
                    print outer0;
                    print outer1;
                    print outer2;
                }}
            }}
        }}"
    );
    let mut interpreter = Interpreter::new();

    assert_eq!(
        interpreter.interpret(&source),
        InterpretResult::CompileError
    );
}
