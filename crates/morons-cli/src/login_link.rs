//! Owner-requested OS interaction for a validated, ephemeral login navigation URL.
use std::io::{Read, Write};

use morons_protocol::OpenAiAuthorizationUrl;
use zeroize::Zeroizing;

pub(crate) const COPY_HELPER: &str = "--internal-copy-login-link";
pub(crate) const SELECTION_HELPER: &str = "--internal-copy-selection";
pub(crate) const MAX_SELECTION_BYTES: usize = 256 * 1024;
pub(crate) const READY: u8 = 1;
pub(crate) const INVALID_SELECTION: u8 = 2;
pub(crate) const CLIPBOARD_CONNECT_FAILED: u8 = 3;
pub(crate) const CLIPBOARD_WRITE_FAILED: u8 = 4;

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
/// Login URLs and rendered selections use separate validated helper modes.
#[doc(hidden)]
pub fn run_login_link_helper() -> Option<std::process::ExitCode> {
    let mut args = std::env::args_os().skip(1);
    let first = args.next()?;
    if first != COPY_HELPER && first != SELECTION_HELPER {
        return None;
    }
    let result = if args.next().is_none() {
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
        if first == SELECTION_HELPER {
            let result = copy_selection(
                &mut std::io::stdin().lock(),
                &mut std::io::stdout().lock(),
                |text| {
                    let mut clipboard =
                        arboard::Clipboard::new().map_err(|_| CLIPBOARD_CONNECT_FAILED)?;
                    clipboard
                        .set_text(text)
                        .map_err(|_| CLIPBOARD_WRITE_FAILED)?;
                    Ok(clipboard)
                },
            );
            result.map_err(|code| {
                if code != 0 {
                    let _ = std::io::stdout().lock().write_all(&[code]);
                }
            })
        } else {
            copy_link(
                &mut std::io::stdin().lock(),
                &mut std::io::stdout().lock(),
                |url| {
                    let mut clipboard = arboard::Clipboard::new().map_err(|_| ())?;
                    clipboard.set_text(url.as_str()).map_err(|_| ())?;
                    Ok(clipboard)
                },
            )
        }
    } else {
        Err(())
    };
    Some(if result.is_ok() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    })
}

fn copy_selection<R: Read, W: Write, C>(
    input: &mut R,
    output: &mut W,
    copy: impl FnOnce(&str) -> Result<C, u8>,
) -> Result<(), u8> {
    let mut length = [0; 4];
    input
        .read_exact(&mut length)
        .map_err(|_| INVALID_SELECTION)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_SELECTION_BYTES {
        return Err(INVALID_SELECTION);
    }
    let mut bytes = Zeroizing::new(vec![0; length]);
    input
        .read_exact(&mut bytes)
        .map_err(|_| INVALID_SELECTION)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| INVALID_SELECTION)?;
    if text
        .chars()
        .any(|c| (c.is_control() && c != '\n') || crate::terminal::is_bidirectional_control(c))
    {
        return Err(INVALID_SELECTION);
    }
    let _owner = copy(text)?;
    output.write_all(&[READY]).map_err(|_| 0)?;
    output.flush().map_err(|_| 0)?;
    let mut end = [0];
    match input.read(&mut end) {
        Ok(0) => Ok(()),
        _ => Err(0),
    }
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

#[cfg(test)]
mod selection_tests {
    use super::*;
    #[test]
    fn selection_backend_errors_preserve_stage_without_success_acknowledgment() {
        for code in [CLIPBOARD_CONNECT_FAILED, CLIPBOARD_WRITE_FAILED] {
            let mut frame = 4u32.to_be_bytes().to_vec();
            frame.extend_from_slice(b"test");
            let mut output = Vec::new();
            assert_eq!(
                copy_selection(&mut frame.as_slice(), &mut output, |_| -> Result<(), u8> {
                    Err(code)
                }),
                Err(code)
            );
            assert!(output.is_empty());
        }
    }

    #[test]
    fn selection_helper_validates_before_copy_and_acknowledges_without_echo() {
        for text in ["hello\n界", "bad\x1b]52", "bad\u{202e}", ""] {
            let mut frame = (text.len() as u32).to_be_bytes().to_vec();
            frame.extend_from_slice(text.as_bytes());
            let mut output = Vec::new();
            let mut copied = false;
            let result = copy_selection(&mut frame.as_slice(), &mut output, |value| {
                assert_eq!(value, text);
                copied = true;
                Ok(())
            });
            assert_eq!(result.is_ok(), text == "hello\n界");
            assert_eq!(copied, result.is_ok());
            assert_eq!(output, if copied { vec![READY] } else { vec![] });
        }
        for frame in [
            ((MAX_SELECTION_BYTES + 1) as u32).to_be_bytes().to_vec(),
            vec![0, 0, 0, 1, 255],
            vec![0, 0, 0, 2, b'a'],
        ] {
            assert!(
                copy_selection(
                    &mut frame.as_slice(),
                    &mut Vec::new(),
                    |_| -> Result<(), u8> { panic!("invalid input must not copy") }
                )
                .is_err()
            );
        }
    }
}
