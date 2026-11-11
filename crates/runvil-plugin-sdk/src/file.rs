//! What a tool call learns of its run's files: the [`Stat`] of a path,
//! the [`DirEntry`] values of a directory, and the [`FileError`] a file
//! method of [`ToolCall`](crate::ToolCall) fails with.

use std::fmt;

use runvil_plugin_sys::{
    RV_FILE_ERROR_CONFLICT, RV_FILE_ERROR_DENIED, RV_FILE_ERROR_IO, RV_FILE_ERROR_IS_A_DIRECTORY,
    RV_FILE_ERROR_NOT_A_DIRECTORY, RV_FILE_ERROR_NOT_FOUND, RV_FILE_TYPE_DIRECTORY,
    RV_FILE_TYPE_FILE, RV_FILE_TYPE_SYMLINK, RvFileErrorKind, RvFileType,
};

/// What a path names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FileType {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link.
    Symlink,
    /// Something else, such as a pipe, a socket, or a device.
    Other,
}

impl FileType {
    /// The kind the contract's number `raw` names. A number this crate
    /// does not know is [`FileType::Other`].
    pub(crate) fn from_raw(raw: RvFileType) -> FileType {
        match raw {
            RV_FILE_TYPE_FILE => FileType::File,
            RV_FILE_TYPE_DIRECTORY => FileType::Directory,
            RV_FILE_TYPE_SYMLINK => FileType::Symlink,
            _ => FileType::Other,
        }
    }

    /// The contract's number for this kind.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn raw(self) -> RvFileType {
        match self {
            FileType::File => RV_FILE_TYPE_FILE,
            FileType::Directory => RV_FILE_TYPE_DIRECTORY,
            FileType::Symlink => RV_FILE_TYPE_SYMLINK,
            FileType::Other => runvil_plugin_sys::RV_FILE_TYPE_OTHER,
        }
    }
}

/// What a path names, and how long a file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Stat {
    /// What the path names. A link is named as a link, not as what it
    /// leads to.
    pub file_type: FileType,
    /// The length of a file in bytes. For anything but a file it says
    /// nothing.
    pub len: u64,
}

impl Stat {
    /// The metadata of something of `file_type` that is `len` bytes long.
    #[must_use]
    pub fn new(file_type: FileType, len: u64) -> Stat {
        Stat { file_type, len }
    }
}

/// One entry of a directory: a name beside what it names.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct DirEntry {
    /// The entry's name within its directory.
    pub name: String,
    /// What the entry is.
    pub stat: Stat,
}

impl DirEntry {
    /// The entry `name` with the metadata `stat`.
    #[must_use]
    pub fn new(name: impl Into<String>, stat: Stat) -> DirEntry {
        DirEntry {
            name: name.into(),
            stat,
        }
    }
}

/// Why a file method failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FileErrorKind {
    /// Nothing is at the path.
    NotFound,
    /// The path names something that is not a directory, where the method
    /// needs one.
    NotADirectory,
    /// The path names a directory, where the method needs a file.
    IsADirectory,
    /// The run's policy does not allow the read. The error's text is the
    /// policy's own reason.
    Denied,
    /// Another tool call of the run changed the path, or is changing it,
    /// at the same time. The same read can succeed in a later call.
    Conflict,
    /// The operating system failed the read.
    Io,
    /// A reason the other kinds do not name.
    Other,
}

impl FileErrorKind {
    /// The kind the contract's number `raw` names. A number this crate
    /// does not know is [`FileErrorKind::Other`].
    pub(crate) fn from_raw(raw: RvFileErrorKind) -> FileErrorKind {
        match raw {
            RV_FILE_ERROR_NOT_FOUND => FileErrorKind::NotFound,
            RV_FILE_ERROR_NOT_A_DIRECTORY => FileErrorKind::NotADirectory,
            RV_FILE_ERROR_IS_A_DIRECTORY => FileErrorKind::IsADirectory,
            RV_FILE_ERROR_DENIED => FileErrorKind::Denied,
            RV_FILE_ERROR_CONFLICT => FileErrorKind::Conflict,
            RV_FILE_ERROR_IO => FileErrorKind::Io,
            _ => FileErrorKind::Other,
        }
    }

    /// The contract's number for this kind.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn raw(self) -> RvFileErrorKind {
        match self {
            FileErrorKind::NotFound => RV_FILE_ERROR_NOT_FOUND,
            FileErrorKind::NotADirectory => RV_FILE_ERROR_NOT_A_DIRECTORY,
            FileErrorKind::IsADirectory => RV_FILE_ERROR_IS_A_DIRECTORY,
            FileErrorKind::Denied => RV_FILE_ERROR_DENIED,
            FileErrorKind::Conflict => RV_FILE_ERROR_CONFLICT,
            FileErrorKind::Io => RV_FILE_ERROR_IO,
            FileErrorKind::Other => runvil_plugin_sys::RV_FILE_ERROR_OTHER,
        }
    }
}

/// A file method that failed: why, and the Host's words for what went
/// wrong, which a model can read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileError {
    kind: FileErrorKind,
    text: String,
}

impl FileError {
    /// A failure of `kind` that says `text`.
    #[must_use]
    pub fn new(kind: FileErrorKind, text: impl Into<String>) -> FileError {
        FileError {
            kind,
            text: text.into(),
        }
    }

    /// Why the method failed.
    #[must_use]
    pub fn kind(&self) -> FileErrorKind {
        self.kind
    }

    /// What went wrong, in the Host's words.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for FileError {}

#[cfg(test)]
mod tests {
    use super::{DirEntry, FileError, FileErrorKind, FileType, Stat};

    #[test]
    fn each_file_type_keeps_its_number_and_an_unknown_number_is_other() {
        for kind in [
            FileType::File,
            FileType::Directory,
            FileType::Symlink,
            FileType::Other,
        ] {
            assert_eq!(FileType::from_raw(kind.raw()), kind);
        }
        assert_eq!(FileType::from_raw(99), FileType::Other);
    }

    #[test]
    fn each_error_kind_keeps_its_number_and_an_unknown_number_is_other() {
        for kind in [
            FileErrorKind::NotFound,
            FileErrorKind::NotADirectory,
            FileErrorKind::IsADirectory,
            FileErrorKind::Denied,
            FileErrorKind::Conflict,
            FileErrorKind::Io,
            FileErrorKind::Other,
        ] {
            assert_eq!(FileErrorKind::from_raw(kind.raw()), kind);
        }
        assert_eq!(FileErrorKind::from_raw(99), FileErrorKind::Other);
    }

    #[test]
    fn an_entry_pairs_a_name_with_its_metadata() {
        let entry = DirEntry::new("a.txt", Stat::new(FileType::File, 3));
        assert_eq!(entry.name, "a.txt");
        assert_eq!(entry.stat.file_type, FileType::File);
        assert_eq!(entry.stat.len, 3);
    }

    #[test]
    fn an_error_carries_its_kind_and_displays_its_text() {
        let error = FileError::new(FileErrorKind::Denied, "the mode allows only markdown");
        assert_eq!(error.kind(), FileErrorKind::Denied);
        assert_eq!(error.text(), "the mode allows only markdown");
        assert_eq!(error.to_string(), "the mode allows only markdown");
    }
}
