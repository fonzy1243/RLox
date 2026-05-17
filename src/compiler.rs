use crate::scanner::{Scanner, TokenType};

pub fn compile(source: &str) {
    let mut scanner = Scanner::new(source);
    let mut line: i64 = -1;

    loop {
        let token = scanner.scan_token();

        if token.line as i64 != line {
            print!("{:4} ", token.line);
            line = token.line as i64;
        } else {
            print!("   | ");
        }

        println!(
            "{:2} '{}'",
            token.token_type as i32,
            &token.start[..token.length]
        );

        if token.token_type == TokenType::Eof {
            break;
        }
    }
}
