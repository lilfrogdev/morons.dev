use std::{ffi::OsStr, fmt};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use super::ToolErrorKind;

pub(crate) const MAX_WORKTREE_PATH_BYTES: usize = 1_024;
pub(crate) const MAX_WORKTREE_COMPONENT_BYTES: usize = 255;
pub(crate) const MAX_WORKTREE_PATH_DEPTH: usize = 64;
pub(crate) const MAX_TOOL_PATH_BYTES: usize = 4_096;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ToolPath(String);

impl ToolPath {
    pub(crate) fn parse(value: &str) -> Result<Self, ToolErrorKind> {
        if value.is_empty()
            || value.len() > MAX_TOOL_PATH_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(ToolErrorKind::InvalidPath);
        }
        Ok(Self(value.to_owned()))
    }

    pub(crate) const fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for ToolPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolPath")
            .field("path_bytes", &self.0.len())
            .finish()
    }
}

impl Serialize for ToolPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ToolPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(|_| de::Error::custom("invalid tool path"))
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WorktreePath(String);

impl WorktreePath {
    pub(crate) fn parse(value: &str, root_allowed: bool) -> Result<Self, ToolErrorKind> {
        if value == "." {
            return root_allowed
                .then(|| Self(value.to_owned()))
                .ok_or(ToolErrorKind::InvalidPath);
        }
        if value.is_empty()
            || value.len() > MAX_WORKTREE_PATH_BYTES
            || value.starts_with('/')
            || value.ends_with('/')
            || value.contains(['\\', ':', '\0'])
        {
            return Err(ToolErrorKind::InvalidPath);
        }
        let mut depth = 0_usize;
        for component in value.split('/') {
            depth = depth.checked_add(1).ok_or(ToolErrorKind::InvalidPath)?;
            if component.is_empty()
                || component == "."
                || component == ".."
                || component.len() > MAX_WORKTREE_COMPONENT_BYTES
                || component.chars().any(char::is_control)
                || !native_component_is_exact(component)
            {
                return Err(ToolErrorKind::InvalidPath);
            }
        }
        if depth == 0 || depth > MAX_WORKTREE_PATH_DEPTH {
            return Err(ToolErrorKind::InvalidPath);
        }
        Ok(Self(value.to_owned()))
    }

    pub(crate) const fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) fn components(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.0.split('/').filter(|component| *component != ".")
    }

    pub(crate) fn parent_and_name(&self) -> Result<(Self, &str), ToolErrorKind> {
        if self.0 == "." {
            return Err(ToolErrorKind::InvalidPath);
        }
        match self.0.rsplit_once('/') {
            Some((parent, name)) => Ok((Self(parent.to_owned()), name)),
            None => Ok((Self(".".to_owned()), self.0.as_str())),
        }
    }
}

impl fmt::Debug for WorktreePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("WorktreePath")
            .field(&self.0)
            .finish()
    }
}

impl Serialize for WorktreePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for WorktreePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value, true).map_err(|_| de::Error::custom("invalid worktree-relative path"))
    }
}

fn native_component_is_exact(component: &str) -> bool {
    let native = OsStr::new(component);
    native.to_str() == Some(component)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_tool_paths_allow_normal_relative_and_absolute_semantics() {
        for valid in [
            "file.txt",
            "../sibling.txt",
            "/tmp/file.txt",
            "C:\\project\\file.txt",
        ] {
            assert!(ToolPath::parse(valid).is_ok(), "{valid}");
        }
        for invalid in ["", "line\nfeed", "nul\0path"] {
            assert!(ToolPath::parse(invalid).is_err(), "{invalid:?}");
        }
        assert!(
            !format!("{:?}", ToolPath::parse("/private/path").unwrap()).contains("/private/path")
        );
    }

    #[test]
    fn worktree_paths_are_strict_and_slash_separated() {
        for valid in [".", "src", "src/lib.rs", "é/文件.rs"] {
            assert!(WorktreePath::parse(valid, true).is_ok(), "{valid}");
        }
        for invalid in [
            "",
            "/src",
            "src/",
            "src//lib.rs",
            "./src",
            "src/../x",
            "src\\x",
            "C:/x",
            "src:\\x",
            "src/\u{0000}x",
            "src/\nfile",
        ] {
            assert!(WorktreePath::parse(invalid, true).is_err(), "{invalid:?}");
        }
        assert!(WorktreePath::parse(".", false).is_err());
    }
}
