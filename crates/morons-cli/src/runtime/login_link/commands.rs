use crate::login_link::COPY_HELPER;
use morons_protocol::OpenAiAuthorizationUrl;
use std::{ffi::OsStr, path::Path};
use tokio::process::Command;

pub(super) fn browser(url: &OpenAiAuthorizationUrl) -> Result<Command, ()> {
    launcher(
        std::env::consts::OS,
        std::env::var_os("SystemRoot").as_deref(),
        url,
    )
}
fn launcher(
    os: &str,
    system_root: Option<&OsStr>,
    url: &OpenAiAuthorizationUrl,
) -> Result<Command, ()> {
    let mut command = match os {
        "macos" => {
            let mut command = Command::new("/usr/bin/open");
            command.current_dir("/");
            command
        }
        "linux" => {
            let mut command = Command::new("/usr/bin/xdg-open");
            command.current_dir("/").env_remove("BROWSER");
            command
        }
        "windows" => {
            let root = Path::new(system_root.ok_or(())?);
            if !root.is_absolute() {
                return Err(());
            }
            let system = root.join("System32");
            let mut command = Command::new(system.join("rundll32.exe"));
            let mut handler = system.join("url.dll").into_os_string();
            handler.push(",FileProtocolHandler");
            command.arg(handler).current_dir(system);
            command
        }
        _ => return Err(()),
    };
    // Only this closed, authenticated navigation URL may cross the OS argv boundary.
    // Never format it into shell source or emit command/error Debug output.
    command.arg(url.as_str());
    Ok(command)
}
pub(super) fn clipboard() -> Result<Command, ()> {
    let executable = std::env::current_exe().map_err(|_| ())?;
    let mut command = Command::new(executable);
    command.arg(COPY_HELPER);
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_launchers_receive_one_validated_url_argument_without_a_shell() {
        let url = OpenAiAuthorizationUrl::new(
            "https://auth.openai.com/oauth/authorize?state=a&scope=openid%20email".into(),
        )
        .unwrap();
        for os in ["macos", "linux", "windows"] {
            let root = std::env::temp_dir();
            let command = launcher(os, Some(root.as_os_str()), &url).unwrap();
            let command = command.as_std();
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(args.last().unwrap(), &OsStr::new(url.as_str()));
            assert_eq!(args.len(), if os == "windows" { 2 } else { 1 });
            if os == "windows" {
                assert_eq!(
                    command.get_program(),
                    root.join("System32").join("rundll32.exe").as_os_str()
                );
            } else {
                assert_eq!(
                    command.get_program(),
                    if os == "macos" {
                        "/usr/bin/open"
                    } else {
                        "/usr/bin/xdg-open"
                    }
                );
            }
            if os == "linux" {
                assert!(
                    command
                        .get_envs()
                        .any(|(key, value)| key == "BROWSER" && value.is_none())
                );
            }
        }
        assert!(launcher("windows", Some(OsStr::new("relative")), &url).is_err());
        assert!(launcher("windows", None, &url).is_err());
        assert!(launcher("other", None, &url).is_err());
        let copy = clipboard().unwrap();
        assert_eq!(
            copy.as_std().get_args().collect::<Vec<_>>(),
            [OsStr::new(COPY_HELPER)]
        );
    }
}
