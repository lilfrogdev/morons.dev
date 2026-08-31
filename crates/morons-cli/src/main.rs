use std::process::ExitCode;

use morons_cli::run_terminal_application;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run_terminal_application().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
