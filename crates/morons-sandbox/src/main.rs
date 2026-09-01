use std::{
    env,
    io::{self, Read},
    process::ExitCode,
    thread,
};

use morons_sandbox::{
    Cancellation, SandboxResult, SandboxStatus, execute, read_request, write_result,
};

fn main() -> ExitCode {
    let mut arguments = env::args_os();
    let _program = arguments.next();
    match arguments.next() {
        None => run_helper(),
        #[cfg(target_os = "linux")]
        Some(mode) if mode == "--linux-namespace-stage" => {
            let Some(parent) = parse_parent(arguments.next()) else {
                return ExitCode::FAILURE;
            };
            if arguments.next().is_some() {
                return ExitCode::FAILURE;
            }
            let Ok(request) = read_request(&mut io::stdin().lock()) else {
                return ExitCode::FAILURE;
            };
            morons_sandbox::run_namespace_stage(request, parent)
        }
        #[cfg(target_os = "linux")]
        Some(mode) if mode == "--linux-pid-stage" => {
            if arguments.next().is_some() {
                return ExitCode::FAILURE;
            }
            let Ok(request) = read_request(&mut io::stdin().lock()) else {
                return ExitCode::FAILURE;
            };
            morons_sandbox::run_pid_stage(request)
        }
        #[cfg(target_os = "linux")]
        Some(mode) if mode == "--linux-command-stage" => {
            let Some(parent) = parse_parent(arguments.next()) else {
                return ExitCode::FAILURE;
            };
            if arguments.next().is_some() {
                return ExitCode::FAILURE;
            }
            let Ok(request) = read_request(&mut io::stdin().lock()) else {
                return ExitCode::FAILURE;
            };
            morons_sandbox::run_command_stage(request, parent)
        }
        Some(_) => ExitCode::FAILURE,
    }
}

fn run_helper() -> ExitCode {
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

#[cfg(target_os = "linux")]
fn parse_parent(value: Option<std::ffi::OsString>) -> Option<u32> {
    value?.to_str()?.parse().ok()
}
