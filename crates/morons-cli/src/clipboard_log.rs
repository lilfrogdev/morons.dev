use std::{
    fs::{File, OpenOptions},
    io::Write,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: usize = 64 * 1024;

struct Log {
    file: File,
    bytes: usize,
}

fn open() -> Option<Log> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "morons-cli-clipboard-{}-{timestamp}.log",
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Some(Log {
        file: options.open(path).ok()?,
        bytes: 0,
    })
}

fn append(writer: &mut impl Write, bytes: &mut usize, line: &[u8]) -> std::io::Result<()> {
    if line.len() <= MAX_BYTES.saturating_sub(*bytes) {
        *bytes += line.len();
        writer.write_all(line)?;
    }
    Ok(())
}

// Callers supply fixed categories only, never clipboard contents or backend errors.
pub(crate) fn record(message: &'static str) {
    static LOG: OnceLock<Mutex<Option<Log>>> = OnceLock::new();
    let Ok(mut log) = LOG.get_or_init(|| Mutex::new(open())).lock() else {
        return;
    };
    if let Some(writer) = log.as_mut() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let line = format!("{timestamp} {message}\n");
        if append(&mut writer.file, &mut writer.bytes, line.as_bytes()).is_err() {
            *log = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_is_bounded_and_propagates_write_failure() {
        let mut output = Vec::new();
        let mut bytes = 0;
        append(&mut output, &mut bytes, &vec![b'x'; MAX_BYTES]).unwrap();
        append(&mut output, &mut bytes, b"overflow").unwrap();
        assert_eq!(output.len(), MAX_BYTES);
        assert_eq!(bytes, MAX_BYTES);
        assert!(append(&mut &mut [][..], &mut 0, b"failure").is_err());
    }
}
