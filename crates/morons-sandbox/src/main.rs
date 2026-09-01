use std::{
    io::{self, Read},
    process::ExitCode,
    thread,
};

use morons_sandbox::{
    Cancellation, SandboxResult, SandboxStatus, execute, read_request, write_result,
};

fn main() -> ExitCode {
    let request = match read_request(&mut io::stdin().lock()) {
        Ok(request) => request,
        Err(_) => {
            let result = SandboxResult::failure([0; 16], SandboxStatus::RequestRejected);
            return write_result(&mut io::stdout().lock(), &result)
                .map(|()| ExitCode::SUCCESS)
                .unwrap_or(ExitCode::FAILURE);
        }
    };
    let cancellation = Cancellation::new();
    let watchdog = cancellation.clone();
    thread::spawn(move || {
        let mut byte = [0_u8; 1];
        let _ = io::stdin().read(&mut byte);
        watchdog.cancel();
    });
    let result = execute(request, &cancellation);
    write_result(&mut io::stdout().lock(), &result)
        .map(|()| ExitCode::SUCCESS)
        .unwrap_or(ExitCode::FAILURE)
}
