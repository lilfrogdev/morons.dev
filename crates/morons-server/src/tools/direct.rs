use std::{
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};

use super::{
    MAX_FILE_BYTES, MAX_READ_LINES, MAX_READ_OUTPUT_BYTES, MAX_REPLACEMENTS, TextReplacement,
    ToolErrorKind, ToolImageOutput, ToolInput, ToolOutput, ToolPath, ToolResult,
};

pub(crate) struct DirectToolExecutor {
    working_directory: PathBuf,
}

impl DirectToolExecutor {
    pub(crate) fn new(working_directory: PathBuf) -> Self {
        Self { working_directory }
    }

    pub(crate) fn execute<F>(&self, input: &ToolInput, cancelled: &F) -> ToolResult
    where
        F: Fn() -> bool,
    {
        if cancelled() {
            return ToolResult::error(ToolErrorKind::Cancelled);
        }
        let result = match input {
            ToolInput::Read {
                path,
                offset,
                limit,
            } => self.read(path, *offset, *limit, cancelled),
            ToolInput::Write { path, content } => self.write(path, content, cancelled),
            ToolInput::Edit { path, replacements } => self.edit(path, replacements, cancelled),
            _ => Err(ToolErrorKind::Filesystem),
        };
        result
            .map(|output| ToolResult::Ok { output })
            .unwrap_or_else(ToolResult::error)
    }

    fn read<F>(
        &self,
        path: &ToolPath,
        offset: u32,
        limit: u16,
        cancelled: &F,
    ) -> Result<ToolOutput, ToolErrorKind>
    where
        F: Fn() -> bool,
    {
        if offset == 0 || limit == 0 || limit > MAX_READ_LINES {
            return Err(ToolErrorKind::ResourceLimit);
        }
        let bytes = read_bounded_with_limit(
            &self.resolve(path),
            morons_image::MAX_INPUT_IMAGE_BYTES as u64,
            cancelled,
        )?;
        let text = match String::from_utf8(bytes) {
            Ok(text) if !text.contains('\0') => text,
            Ok(text) => return normalize_read_image(path, text.into_bytes(), cancelled),
            Err(error) => {
                return normalize_read_image(path, error.into_bytes(), cancelled).map_err(
                    |error| {
                        if error == ToolErrorKind::BinaryFile {
                            ToolErrorKind::InvalidUtf8
                        } else {
                            error
                        }
                    },
                );
            }
        };
        let lines = text.split_inclusive('\n').collect::<Vec<_>>();
        let start = usize::try_from(offset - 1).map_err(|_| ToolErrorKind::ResourceLimit)?;
        let mut output = String::new();
        let mut returned = 0_u32;
        for line in lines.iter().skip(start).take(usize::from(limit)) {
            if cancelled() {
                return Err(ToolErrorKind::Cancelled);
            }
            if output
                .len()
                .checked_add(line.len())
                .is_none_or(|bytes| bytes > MAX_READ_OUTPUT_BYTES)
            {
                if output.is_empty() {
                    return Err(ToolErrorKind::ResourceLimit);
                }
                break;
            }
            output.push_str(line);
            returned = returned
                .checked_add(1)
                .ok_or(ToolErrorKind::ResourceLimit)?;
        }
        let next_offset = offset
            .checked_add(returned)
            .ok_or(ToolErrorKind::ResourceLimit)?;
        let end_of_file = start.saturating_add(returned as usize) >= lines.len();
        Ok(ToolOutput::Read {
            path: path.clone(),
            offset,
            next_offset,
            end_of_file,
            text: output,
        })
    }

    fn write<F>(
        &self,
        path: &ToolPath,
        content: &str,
        cancelled: &F,
    ) -> Result<ToolOutput, ToolErrorKind>
    where
        F: Fn() -> bool,
    {
        if content.len() as u64 > MAX_FILE_BYTES {
            return Err(ToolErrorKind::ResourceLimit);
        }
        if cancelled() {
            return Err(ToolErrorKind::Cancelled);
        }
        fs::write(self.resolve(path), content.as_bytes()).map_err(|_| ToolErrorKind::Uncertain)?;
        Ok(ToolOutput::Written {
            path: path.clone(),
            bytes: content.len() as u64,
        })
    }

    fn edit<F>(
        &self,
        path: &ToolPath,
        replacements: &[TextReplacement],
        cancelled: &F,
    ) -> Result<ToolOutput, ToolErrorKind>
    where
        F: Fn() -> bool,
    {
        if replacements.is_empty() || replacements.len() > MAX_REPLACEMENTS {
            return Err(ToolErrorKind::ResourceLimit);
        }
        let target = self.resolve(path);
        let bytes = read_bounded(&target, cancelled)?;
        if bytes.contains(&0) {
            return Err(ToolErrorKind::BinaryFile);
        }
        let source = String::from_utf8(bytes).map_err(|_| ToolErrorKind::InvalidUtf8)?;
        let edited = apply_replacements(&source, replacements)?;
        if cancelled() {
            return Err(ToolErrorKind::Cancelled);
        }
        fs::write(target, edited.as_bytes()).map_err(|_| ToolErrorKind::Uncertain)?;
        Ok(ToolOutput::Edited {
            path: path.clone(),
            replacements: u16::try_from(replacements.len())
                .map_err(|_| ToolErrorKind::ResourceLimit)?,
            bytes: edited.len() as u64,
        })
    }

    fn resolve(&self, path: &ToolPath) -> PathBuf {
        let requested = Path::new(path.as_str());
        if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.working_directory.join(requested)
        }
    }
}

fn read_bounded<F>(path: &Path, cancelled: &F) -> Result<Vec<u8>, ToolErrorKind>
where
    F: Fn() -> bool,
{
    read_bounded_with_limit(path, MAX_FILE_BYTES, cancelled)
}

fn read_bounded_with_limit<F>(
    path: &Path,
    maximum_bytes: u64,
    cancelled: &F,
) -> Result<Vec<u8>, ToolErrorKind>
where
    F: Fn() -> bool,
{
    let metadata = fs::metadata(path).map_err(map_io)?;
    if !metadata.is_file() {
        return Err(ToolErrorKind::WrongNodeKind);
    }
    if metadata.len() > maximum_bytes {
        return Err(ToolErrorKind::ResourceLimit);
    }
    if cancelled() {
        return Err(ToolErrorKind::Cancelled);
    }
    let bytes = fs::read(path).map_err(map_io)?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(ToolErrorKind::ResourceLimit);
    }
    Ok(bytes)
}

fn normalize_read_image<F>(
    path: &ToolPath,
    bytes: Vec<u8>,
    cancelled: &F,
) -> Result<ToolOutput, ToolErrorKind>
where
    F: Fn() -> bool,
{
    let image = morons_image::normalize_image(&bytes).map_err(|_| ToolErrorKind::BinaryFile)?;
    if cancelled() {
        return Err(ToolErrorKind::Cancelled);
    }
    let mut display_name = Path::new(path.as_str())
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(
            || format!("image.{}", image.media_type.extension()),
            str::to_owned,
        );
    display_name = display_name
        .chars()
        .map(|character| {
            if character.is_control() || matches!(character, '/' | '\\' | '[' | ']') {
                '_'
            } else {
                character
            }
        })
        .collect();
    while display_name.len() > crate::persistence::images::MAX_IMAGE_DISPLAY_NAME_BYTES {
        display_name.pop();
    }
    if !crate::persistence::images::valid_display_name(&display_name) {
        display_name = format!("image.{}", image.media_type.extension());
    }
    let sha256 = Sha256::digest(&image.bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(ToolOutput::ReadImage {
        path: path.clone(),
        image: ToolImageOutput {
            attachment_id: None,
            display_name,
            media_type: image.media_type,
            width: image.width,
            height: image.height,
            bytes: image.bytes.len() as u64,
            sha256,
            data: image.bytes,
        },
    })
}

fn apply_replacements(
    source: &str,
    replacements: &[TextReplacement],
) -> Result<String, ToolErrorKind> {
    let mut ranges = Vec::with_capacity(replacements.len());
    for replacement in replacements {
        if replacement.old_text.is_empty() {
            return Err(ToolErrorKind::ReplacementAmbiguous);
        }
        let mut matches = source.match_indices(&replacement.old_text);
        let Some((start, _)) = matches.next() else {
            return Err(ToolErrorKind::ReplacementNotFound);
        };
        if matches.next().is_some() {
            return Err(ToolErrorKind::ReplacementAmbiguous);
        }
        let end = start
            .checked_add(replacement.old_text.len())
            .ok_or(ToolErrorKind::ResourceLimit)?;
        ranges.push((start, end, replacement.new_text.as_str()));
    }
    ranges.sort_by_key(|(start, _, _)| *start);
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(ToolErrorKind::ReplacementOverlap);
    }
    let output_bytes = source
        .len()
        .checked_add(
            replacements
                .iter()
                .map(|replacement| replacement.new_text.len())
                .sum::<usize>(),
        )
        .and_then(|bytes| {
            replacements.iter().try_fold(bytes, |bytes, replacement| {
                bytes.checked_sub(replacement.old_text.len())
            })
        })
        .ok_or(ToolErrorKind::ResourceLimit)?;
    if output_bytes as u64 > MAX_FILE_BYTES {
        return Err(ToolErrorKind::ResourceLimit);
    }
    let mut output = String::with_capacity(output_bytes);
    let mut cursor = 0_usize;
    for (start, end, replacement) in ranges {
        output.push_str(&source[cursor..start]);
        output.push_str(replacement);
        cursor = end;
    }
    output.push_str(&source[cursor..]);
    Ok(output)
}

fn map_io(error: std::io::Error) -> ToolErrorKind {
    match error.kind() {
        std::io::ErrorKind::NotFound => ToolErrorKind::NotFound,
        std::io::ErrorKind::InvalidInput => ToolErrorKind::InvalidPath,
        std::io::ErrorKind::IsADirectory | std::io::ErrorKind::NotADirectory => {
            ToolErrorKind::WrongNodeKind
        }
        _ => ToolErrorKind::Filesystem,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn direct_tools_read_write_and_apply_exact_edits() {
        let root = test_directory("complete");
        let executor = DirectToolExecutor::new(root.clone());
        let write = executor.execute(
            &ToolInput::Write {
                path: ToolPath::parse("file.txt").unwrap(),
                content: "before\nsecond\n".to_owned(),
            },
            &|| false,
        );
        assert!(matches!(
            write,
            ToolResult::Ok {
                output: ToolOutput::Written { .. }
            }
        ));
        let read = executor.execute(
            &ToolInput::Read {
                path: ToolPath::parse("file.txt").unwrap(),
                offset: 2,
                limit: 1,
            },
            &|| false,
        );
        assert!(
            matches!(read, ToolResult::Ok { output: ToolOutput::Read { ref text, end_of_file: true, .. } } if text == "second\n")
        );
        let edit = executor.execute(
            &ToolInput::Edit {
                path: ToolPath::parse("file.txt").unwrap(),
                replacements: vec![TextReplacement {
                    old_text: "before".to_owned(),
                    new_text: "after".to_owned(),
                }],
            },
            &|| false,
        );
        assert!(matches!(
            edit,
            ToolResult::Ok {
                output: ToolOutput::Edited { .. }
            }
        ));
        assert_eq!(
            fs::read_to_string(root.join("file.txt")).unwrap(),
            "after\nsecond\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn read_normalizes_common_image_content_without_trusting_the_extension() {
        let root = test_directory("image");
        let source = morons_image::normalize_rgba(2, 3, vec![0x77; 24])
            .expect("fixture image should normalize");
        fs::write(root.join("not-an-image.bin"), &source.bytes).expect("fixture should be written");
        let result = DirectToolExecutor::new(root.clone()).execute(
            &ToolInput::Read {
                path: ToolPath::parse("not-an-image.bin").unwrap(),
                offset: 1,
                limit: 1,
            },
            &|| false,
        );
        assert!(matches!(
            result,
            ToolResult::Ok {
                output: ToolOutput::ReadImage {
                    image: ToolImageOutput {
                        attachment_id: None,
                        width: 2,
                        height: 3,
                        ref data,
                        ..
                    },
                    ..
                }
            } if data == &source.bytes
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn direct_tools_allow_parent_and_absolute_paths_but_reject_ambiguous_edits() {
        let root = test_directory("semantics");
        let child = root.join("child");
        fs::create_dir(&child).unwrap();
        fs::write(root.join("shared.txt"), "same same").unwrap();
        let executor = DirectToolExecutor::new(child);
        let result = executor.execute(
            &ToolInput::Edit {
                path: ToolPath::parse("../shared.txt").unwrap(),
                replacements: vec![TextReplacement {
                    old_text: "same".to_owned(),
                    new_text: "changed".to_owned(),
                }],
            },
            &|| false,
        );
        assert_eq!(
            result,
            ToolResult::error(ToolErrorKind::ReplacementAmbiguous)
        );
        let absolute = root.join("absolute.txt");
        let result = executor.execute(
            &ToolInput::Write {
                path: ToolPath::parse(&absolute.to_string_lossy()).unwrap(),
                content: "absolute".to_owned(),
            },
            &|| false,
        );
        assert!(matches!(result, ToolResult::Ok { .. }));
        assert_eq!(fs::read_to_string(absolute).unwrap(), "absolute");
        fs::remove_dir_all(root).unwrap();
    }

    fn test_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "morons-direct-tools-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        path
    }
}
