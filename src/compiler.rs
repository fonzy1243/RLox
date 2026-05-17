use crate::scanner::Scanner;

pub fn compile(source: &str) {
    let mut scanner = Scanner::new(source);
    let mut line = usize::MAX;

    loop {
        let token = scanner.scan_token();
    }
}
