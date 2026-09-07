//! Owner-requested OS interaction for a validated, ephemeral login navigation URL.
use std::io::{Read, Write};

use morons_protocol::OpenAiAuthorizationUrl;
use zeroize::Zeroizing;

pub(crate) const COPY_HELPER: &str = "--internal-copy-login-link";
pub(crate) const READY: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkAction {
    Open,
    Copy,
}

pub(crate) struct LinkEvent {
    pub(crate) scope: std::sync::Arc<()>,
    pub(crate) message: &'static str,
}

/// Handles the bounded clipboard subprocess before terminal/server initialization.
/// This intentionally has no arbitrary text, token or endpoint argument interface.
#[doc(hidden)]
pub fn run_login_link_helper() -> Option<std::process::ExitCode> {
    let mut args = std::env::args_os().skip(1);
    let first = args.next()?;
    let result = if first == COPY_HELPER && args.next().is_none() {
        // A parent crash must not leave a blocked platform clipboard call alive.
        // This watchdog is inside the disposable helper, never the terminal client.
        if std::thread::Builder::new()
            .name("login-link-deadline".into())
            .spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(690));
                std::process::exit(1);
            })
            .is_err()
        {
            return Some(std::process::ExitCode::FAILURE);
        }
        copy_link(
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
            |url| {
                let mut clipboard = arboard::Clipboard::new().map_err(|_| ())?;
                clipboard.set_text(url.as_str()).map_err(|_| ())?;
                Ok(clipboard)
            },
        )
    } else {
        Err(())
    };
    Some(if result.is_ok() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    })
}

fn copy_link<R: Read, W: Write, C>(
    input: &mut R,
    output: &mut W,
    copy: impl FnOnce(&OpenAiAuthorizationUrl) -> Result<C, ()>,
) -> Result<(), ()> {
    let mut length = [0; 4];
    input.read_exact(&mut length).map_err(|_| ())?;
    let length = u32::from_be_bytes(length);
    if length == 0 || length > 4096 {
        return Err(());
    }
    let mut bytes = Zeroizing::new(vec![0; length as usize]);
    input.read_exact(&mut bytes).map_err(|_| ())?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ())?;
    let url = OpenAiAuthorizationUrl::new(text.to_owned()).map_err(|_| ())?;
    let _clipboard_owner = copy(&url)?;
    output.write_all(&[READY]).map_err(|_| ())?;
    output.flush().map_err(|_| ())?;
    // Keep X11/Wayland ownership until the supervising client closes the pipe.
    let mut end = [0];
    match input.read(&mut end) {
        Ok(0) => Ok(()),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipboard_helper_copies_the_complete_validated_url_without_echoing_it() {
        let url = format!(
            "https://auth.openai.com/oauth/authorize?state={}",
            "a".repeat(3500)
        );
        let mut input = (url.len() as u32).to_be_bytes().to_vec();
        input.extend_from_slice(url.as_bytes());
        let mut output = Vec::new();
        copy_link(&mut &input[..], &mut output, |value| {
            assert_eq!(value.as_str(), url);
            Ok(())
        })
        .unwrap();
        assert_eq!(output, [READY]);
    }
    #[test]
    fn clipboard_helper_rejects_bad_frames_and_non_authorization_urls_before_os_access() {
        for url in [
            "file:///tmp/secret",
            "https://evil.invalid/",
            "https://auth.openai.com/oauth/authorize?state=x\x1b",
            "\u{ff}",
        ] {
            let mut input = (url.len() as u32).to_be_bytes().to_vec();
            input.extend_from_slice(url.as_bytes());
            assert!(
                copy_link(&mut &input[..], &mut Vec::new(), |_| -> Result<(), ()> {
                    panic!("no OS access")
                })
                .is_err()
            );
        }
        for input in [
            Vec::new(),
            0u32.to_be_bytes().to_vec(),
            4097u32.to_be_bytes().to_vec(),
            vec![0, 0, 0, 3, 1],
        ] {
            assert!(
                copy_link(&mut &input[..], &mut Vec::new(), |_| -> Result<(), ()> {
                    panic!("no OS access")
                })
                .is_err()
            );
        }
    }
}
