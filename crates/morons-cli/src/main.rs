use std::process::ExitCode;

use morons_cli::run_terminal_application_with_debug;

const USAGE: &str = "Usage: morons [--debug | --debug-status | --help]";

fn parse_args(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<Option<bool>, &'static str> {
    let mode = match args.next() {
        None => Some(false),
        Some(arg) if arg == "--debug" => Some(true),
        Some(arg) if arg == "--help" => None,
        Some(_) => return Err(USAGE),
    };
    if args.next().is_some() {
        return Err(USAGE);
    }
    Ok(mode)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    if let Some(outcome) = morons_cli::run_login_link_helper() {
        return outcome;
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args == [std::ffi::OsString::from("--debug-status")] {
        return debug_status().await;
    }
    let debug = match parse_args(args.into_iter()) {
        Ok(Some(debug)) => debug,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    match run_terminal_application_with_debug(debug).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn debug_status() -> ExitCode {
    let query = async {
        let server = morons_cli::connect_existing()
            .await
            .map_err(|_| "debug status connection failed")?
            .ok_or("no ready server; debug status unavailable")?;
        let mut client =
            morons_cli::ApplicationClient::from_negotiated_connection(server.into_connection());
        let status = client
            .debug_status()
            .await
            .map_err(|_| "debug status query failed")?;
        serde_json::to_string(&status).map_err(|_| "debug status encoding failed")
    };
    match tokio::time::timeout(std::time::Duration::from_secs(15), query).await {
        Ok(Ok(status)) => {
            println!("{status}");
            ExitCode::SUCCESS
        }
        Ok(Err(error)) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
        Err(_) => {
            eprintln!("debug status timed out");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_closed_and_explicit() {
        for (args, expected) in [
            (vec![], Ok(Some(false))),
            (vec!["--debug"], Ok(Some(true))),
            (vec!["--help"], Ok(None)),
            (vec!["--debug", "--debug"], Err(USAGE)),
            (vec!["--help", "extra"], Err(USAGE)),
            (vec!["secret"], Err(USAGE)),
        ] {
            assert_eq!(parse_args(args.into_iter().map(Into::into)), expected);
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            assert_eq!(
                parse_args([std::ffi::OsString::from_vec(vec![255])].into_iter()),
                Err(USAGE)
            );
        }
    }
}
