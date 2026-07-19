use std::{
    io::{self, Write},
    process::ExitCode,
};

use lsp_server::Connection;
use rlox_lsp::{ServerOutcome, run_connection};

fn main() -> ExitCode {
    let (connection, io_threads) = Connection::stdio();
    match run_connection(connection) {
        Ok(ServerOutcome::CleanExit) => match io_threads.join() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(format!("LSP transport failed: {error}")),
        },
        Ok(ServerOutcome::ExitWithoutShutdown) => {
            let join_error = io_threads.join().err();
            fail(match join_error {
                Some(error) => format!("client exited without shutdown; transport failed: {error}"),
                None => "client exited without shutdown".to_owned(),
            })
        }
        Ok(ServerOutcome::ChannelClosed) => {
            let join_error = io_threads.join().err();
            fail(match join_error {
                Some(error) => format!("LSP input closed unexpectedly: {error}"),
                None => "LSP input closed without exit".to_owned(),
            })
        }
        Ok(ServerOutcome::OutputClosed) => fail("LSP output closed unexpectedly".to_owned()),
        Err(error) => {
            let message = match flush_pending_output() {
                Ok(()) => error.to_string(),
                Err(flush_error) => format!("{error}; LSP output flush failed: {flush_error}"),
            };
            fail(message)
        }
    }
}

fn flush_pending_output() -> io::Result<()> {
    io::stdout().lock().flush()
}

fn fail(message: String) -> ExitCode {
    eprintln!("rlox-lsp: {message}");
    ExitCode::FAILURE
}
