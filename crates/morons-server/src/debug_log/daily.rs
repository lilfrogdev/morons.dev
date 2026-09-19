use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::persistence::paths::{
    create_private_file, ensure_private_directory, validate_private_directory,
    validate_private_file,
};

const MAX_DAILY_BYTES: u64 = 8 * 1024 * 1024;

pub(super) struct DailyLog {
    directory: PathBuf,
    day: u64,
    file: File,
    bytes: u64,
}

fn current_day() -> io::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() / 86_400)
        .map_err(|_| io::Error::other("invalid debug clock"))
}

fn filename(mut day: u64) -> io::Result<String> {
    let mut year = 1970;
    loop {
        if year > 9999 {
            return Err(io::Error::other("invalid debug date"));
        }
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = if leap { 366 } else { 365 };
        if day < days {
            let months = [
                31,
                if leap { 29 } else { 28 },
                31,
                30,
                31,
                30,
                31,
                31,
                30,
                31,
                30,
                31,
            ];
            for (index, days) in months.into_iter().enumerate() {
                if day < days {
                    return Ok(format!(
                        "morons-{year:04}-{:02}-{:02}.log",
                        index + 1,
                        day + 1
                    ));
                }
                day -= days;
            }
        }
        day -= days;
        year += 1;
    }
}

impl DailyLog {
    pub(super) fn open(root: &Path) -> io::Result<Self> {
        validate_private_directory(root).map_err(io::Error::other)?;
        let directory = root.join("logs");
        ensure_private_directory(&directory).map_err(io::Error::other)?;
        Self::open_day(directory, current_day()?)
    }

    fn open_day(directory: PathBuf, day: u64) -> io::Result<Self> {
        validate_private_directory(&directory).map_err(io::Error::other)?;
        let path = directory.join(filename(day)?);
        let file = match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_private_file(&path).map_err(io::Error::other)?
            }
            Err(error) => return Err(error),
            Ok(_) => {
                validate_private_file(&path, Some(MAX_DAILY_BYTES)).map_err(io::Error::other)?;
                let mut options = OpenOptions::new();
                options.append(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
                }
                let file = options.open(&path)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let opened = file.metadata()?;
                    let entry = std::fs::symlink_metadata(&path)?;
                    if opened.dev() != entry.dev()
                        || opened.ino() != entry.ino()
                        || opened.nlink() != 1
                        || opened.mode() & 0o777 != 0o600
                        || opened.uid() != rustix::process::geteuid().as_raw()
                    {
                        return Err(io::Error::other("invalid debug file identity"));
                    }
                }
                file
            }
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_DAILY_BYTES {
            return Err(io::Error::other("invalid debug file"));
        }
        let bytes = metadata.len();
        Ok(Self {
            directory,
            day,
            file,
            bytes,
        })
    }
}

impl Write for DailyLog {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let day = current_day()?;
        if day != self.day {
            *self = Self::open_day(self.directory.clone(), day)?;
        }
        if buffer.len() as u64 > MAX_DAILY_BYTES.saturating_sub(self.bytes) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "daily debug limit reached",
            ));
        }
        let written = self.file.write(buffer)?;
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_files_append_rotate_and_reject_unsafe_entries() {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let root =
            std::env::temp_dir().join(format!("morons-log-{:016x}", u64::from_ne_bytes(nonce)));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let mut log = DailyLog::open(&root).unwrap();
        log.write_all(b"first\n").unwrap();
        let today = current_day().unwrap();
        let path = root.join("logs").join(filename(today).unwrap());
        drop(log);
        let mut log = DailyLog::open(&root).unwrap();
        log.write_all(b"second\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first\nsecond\n");
        log.bytes = MAX_DAILY_BYTES;
        assert_eq!(
            log.write_all(b"x").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"first\nsecond\n");
        drop(log);
        let mut yesterday = DailyLog::open_day(root.join("logs"), today - 1).unwrap();
        yesterday.bytes = MAX_DAILY_BYTES;
        yesterday.write_all(b"rotated\n").unwrap();
        assert_eq!(yesterday.day, today);
        drop(yesterday);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(DailyLog::open(&root).is_err());
        std::fs::remove_file(&path).unwrap();
        symlink(root.join("target"), &path).unwrap();
        assert!(DailyLog::open(&root).is_err());
        assert!(!root.join("target").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn utc_daily_names_cover_leap_and_century_boundaries() {
        for (day, expected) in [
            (0, "1970-01-01"),
            (11016, "2000-02-29"),
            (19782, "2024-02-29"),
            (47541, "2100-03-01"),
        ] {
            assert_eq!(filename(day).unwrap(), format!("morons-{expected}.log"));
        }
        assert!(filename(u64::MAX).is_err());
    }
}
