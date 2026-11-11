//! [`Files`]: the tree of files a fake Host serves a tool call. It is
//! held in memory, and no call changes it.

use std::collections::BTreeMap;

use crate::{DirEntry, FileError, FileErrorKind, FileType, Stat};

/// The files of a fake run: a tree in memory, which a tool call reads
/// through its [`ToolCall`](crate::ToolCall).
///
/// ```text
/// let files = Files::new()
///     .file("/notes/a.md", "one\ntwo\n")
///     .dir("/empty");
/// let pack = loaded.new_pack("acme/notes").unwrap().with_files(files);
/// ```
///
/// A path is resolved from the root, so `notes/a.md` and `/notes/a.md`
/// name one file. A `.` segment and a repeated `/` change nothing, and a
/// `..` segment steps up one directory and stops at the root.
///
/// [`stat`](Files::stat), [`list`](Files::list), and
/// [`read`](Files::read) answer as the fake Host's file functions do, for
/// a test that runs a tool's own code with no Plugin around it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Files {
    /// Every node but the root, by its resolved path.
    nodes: BTreeMap<String, Node>,
}

/// What a path of the tree names.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    /// A file with these bytes.
    File(Vec<u8>),
    /// A directory.
    Directory,
    /// Something every file function fails on, with this error.
    Fails(FileError),
}

impl Files {
    /// A tree with nothing but its root directory.
    #[must_use]
    pub fn new() -> Files {
        Files::default()
    }

    /// The tree with a file at `path` that holds `contents`, and with
    /// every directory on the way to it.
    ///
    /// # Panics
    /// Panics when `path` names the root, or lies under something that is
    /// not a directory.
    #[must_use]
    pub fn file(self, path: &str, contents: impl AsRef<[u8]>) -> Files {
        self.with(path, Node::File(contents.as_ref().to_vec()))
    }

    /// The tree with a directory at `path`, and with every directory on
    /// the way to it.
    ///
    /// # Panics
    /// Panics when `path` names the root, or lies under something that is
    /// not a directory.
    #[must_use]
    pub fn dir(self, path: &str) -> Files {
        self.with(path, Node::Directory)
    }

    /// The tree with something at `path` that every file function fails
    /// on with `error`, as a path the run's policy denies does. Its
    /// directory lists it as an empty file.
    ///
    /// # Panics
    /// Panics when `path` names the root, or lies under something that is
    /// not a directory.
    #[must_use]
    pub fn fails(self, path: &str, error: FileError) -> Files {
        self.with(path, Node::Fails(error))
    }

    /// The tree with `node` at `path`, in place of what was there.
    fn with(mut self, path: &str, node: Node) -> Files {
        let path = resolve(path).unwrap_or_else(|error| panic!("{}: {path:?}", error.text()));
        assert!(path != "/", "the root is always a directory");
        let mut parent = parent_of(&path);
        while parent != "/" {
            let above = self
                .nodes
                .entry(parent.to_owned())
                .or_insert(Node::Directory);
            assert!(
                *above == Node::Directory,
                "{parent} is not a directory, so nothing lies under it"
            );
            parent = parent_of(parent);
        }
        self.nodes.insert(path, node);
        self
    }

    /// What `path` names, as `file_stat` reports it.
    ///
    /// # Errors
    /// Returns a [`FileError`] when nothing is at `path`, or when the
    /// tree was told to fail there.
    pub fn stat(&self, path: &str) -> Result<Stat, FileError> {
        let path = resolve(path)?;
        Ok(match self.at(&path)? {
            Some(bytes) => Stat::new(FileType::File, len_of(bytes)),
            None => Stat::new(FileType::Directory, 0),
        })
    }

    /// The entries of the directory at `path`, sorted by name, as
    /// `file_list` reports them.
    ///
    /// # Errors
    /// Returns a [`FileError`] when `path` names no directory, or when
    /// the tree was told to fail there.
    pub fn list(&self, path: &str) -> Result<Vec<DirEntry>, FileError> {
        let path = resolve(path)?;
        if self.at(&path)?.is_some() {
            return Err(FileError::new(
                FileErrorKind::NotADirectory,
                format!("not a directory: {path}"),
            ));
        }
        Ok(self
            .nodes
            .iter()
            .filter(|(child, _)| parent_of(child) == path)
            .map(|(child, node)| {
                let stat = match node {
                    Node::File(bytes) => Stat::new(FileType::File, len_of(bytes)),
                    Node::Directory => Stat::new(FileType::Directory, 0),
                    Node::Fails(_) => Stat::new(FileType::File, 0),
                };
                DirEntry::new(name_of(child), stat)
            })
            .collect())
    }

    /// Up to `len` bytes of the file at `path` from the byte `offset` on,
    /// as `file_read` reports them.
    ///
    /// # Errors
    /// Returns a [`FileError`] when `path` names no file, or when the
    /// tree was told to fail there.
    pub fn read(&self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>, FileError> {
        let path = resolve(path)?;
        let Some(bytes) = self.at(&path)? else {
            return Err(FileError::new(
                FileErrorKind::IsADirectory,
                format!("is a directory: {path}"),
            ));
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let end = start
            .saturating_add(usize::try_from(len).unwrap_or(usize::MAX))
            .min(bytes.len());
        Ok(bytes[start..end].to_vec())
    }

    /// What is at the resolved `path`: the bytes of a file, or `None` for
    /// a directory.
    fn at(&self, path: &str) -> Result<Option<&[u8]>, FileError> {
        if path == "/" {
            return Ok(None);
        }
        match self.nodes.get(path) {
            Some(Node::File(bytes)) => Ok(Some(bytes)),
            Some(Node::Directory) => Ok(None),
            Some(Node::Fails(error)) => Err(error.clone()),
            None => Err(FileError::new(
                FileErrorKind::NotFound,
                format!("not found: {path}"),
            )),
        }
    }
}

/// `path` from the root, with one `/` before each segment: `/` alone for
/// the root.
fn resolve(path: &str) -> Result<String, FileError> {
    if path.is_empty() {
        return Err(FileError::new(FileErrorKind::Other, "path is empty"));
    }
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
    }
    if segments.is_empty() {
        return Ok("/".to_owned());
    }
    Ok(segments
        .iter()
        .fold(String::new(), |mut resolved, segment| {
            resolved.push('/');
            resolved.push_str(segment);
            resolved
        }))
}

/// The directory that holds the resolved `path`.
fn parent_of(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/",
        Some((parent, _)) => parent,
    }
}

/// The name the resolved `path` has in its directory.
fn name_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// The length of `bytes` as the contract counts it.
fn len_of(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{Files, resolve};
    use crate::{DirEntry, FileError, FileErrorKind, FileType, Stat};

    fn tree() -> Files {
        Files::new()
            .file("/notes/deep/a.md", "hello")
            .file("/notes/b.md", "")
            .dir("/empty")
            .fails("/notes/secret", denied())
    }

    fn denied() -> FileError {
        FileError::new(FileErrorKind::Denied, "the mode allows only markdown")
    }

    fn failure<T: std::fmt::Debug>(result: Result<T, FileError>) -> (FileErrorKind, String) {
        let error = result.unwrap_err();
        (error.kind(), error.text().to_owned())
    }

    #[test]
    fn a_path_is_resolved_from_the_root() {
        for (path, resolved) in [
            ("/", "/"),
            ("/a/b", "/a/b"),
            ("a/b", "/a/b"),
            ("//a///b/", "/a/b"),
            ("/a/./b", "/a/b"),
            ("/a/x/../b", "/a/b"),
            ("/../..", "/"),
            (".", "/"),
        ] {
            assert_eq!(resolve(path).unwrap(), resolved, "{path}");
        }
        assert_eq!(
            failure(resolve("")),
            (FileErrorKind::Other, "path is empty".to_owned())
        );
    }

    #[test]
    fn stat_names_files_and_directories_including_the_ones_on_the_way() {
        let tree = tree();
        let directory = Stat::new(FileType::Directory, 0);
        assert_eq!(
            tree.stat("/notes/deep/a.md"),
            Ok(Stat::new(FileType::File, 5))
        );
        assert_eq!(tree.stat("notes/b.md"), Ok(Stat::new(FileType::File, 0)));
        for path in ["/", "/notes", "/notes/deep", "/empty"] {
            assert_eq!(tree.stat(path), Ok(directory), "{path}");
        }
        assert_eq!(
            failure(tree.stat("/absent")),
            (FileErrorKind::NotFound, "not found: /absent".to_owned())
        );
    }

    #[test]
    fn list_returns_the_direct_children_sorted_by_name() {
        let tree = tree();
        assert_eq!(
            tree.list("/notes"),
            Ok(vec![
                DirEntry::new("b.md", Stat::new(FileType::File, 0)),
                DirEntry::new("deep", Stat::new(FileType::Directory, 0)),
                DirEntry::new("secret", Stat::new(FileType::File, 0)),
            ])
        );
        assert_eq!(
            tree.list("/"),
            Ok(vec![
                DirEntry::new("empty", Stat::new(FileType::Directory, 0)),
                DirEntry::new("notes", Stat::new(FileType::Directory, 0)),
            ])
        );
        assert_eq!(tree.list("/empty"), Ok(Vec::new()));
        assert_eq!(
            failure(tree.list("/notes/b.md")),
            (
                FileErrorKind::NotADirectory,
                "not a directory: /notes/b.md".to_owned()
            )
        );
        assert_eq!(
            failure(tree.list("/absent")),
            (FileErrorKind::NotFound, "not found: /absent".to_owned())
        );
    }

    #[test]
    fn read_returns_the_bytes_inside_the_range_and_none_past_the_end() {
        let tree = tree();
        let path = "/notes/deep/a.md";
        assert_eq!(tree.read(path, 0, u64::MAX), Ok(b"hello".to_vec()));
        assert_eq!(tree.read(path, 1, 3), Ok(b"ell".to_vec()));
        assert_eq!(tree.read(path, 3, 100), Ok(b"lo".to_vec()));
        assert_eq!(tree.read(path, 5, 1), Ok(Vec::new()));
        assert_eq!(tree.read(path, u64::MAX, u64::MAX), Ok(Vec::new()));
        assert_eq!(tree.read(path, 0, 0), Ok(Vec::new()));
        assert_eq!(
            failure(tree.read("/notes", 0, 1)),
            (
                FileErrorKind::IsADirectory,
                "is a directory: /notes".to_owned()
            )
        );
        assert_eq!(
            failure(tree.read("/absent", 0, 1)),
            (FileErrorKind::NotFound, "not found: /absent".to_owned())
        );
    }

    #[test]
    fn a_path_told_to_fail_fails_every_function_with_its_error() {
        let tree = tree();
        assert_eq!(tree.stat("/notes/secret"), Err(denied()));
        assert_eq!(tree.list("/notes/secret"), Err(denied()));
        assert_eq!(tree.read("/notes/secret", 0, 1), Err(denied()));
    }

    #[test]
    fn a_later_node_replaces_an_earlier_one_at_the_same_path() {
        let tree = Files::new().file("/a", "one").file("a", "three");
        assert_eq!(tree.read("/a", 0, u64::MAX), Ok(b"three".to_vec()));
    }

    #[test]
    #[should_panic(expected = "/a is not a directory, so nothing lies under it")]
    fn a_node_under_a_file_is_the_tests_own_fault() {
        let _tree = Files::new().file("/a", "x").file("/a/b", "y");
    }

    #[test]
    #[should_panic(expected = "the root is always a directory")]
    fn a_node_at_the_root_is_the_tests_own_fault() {
        let _tree = Files::new().file("/", "x");
    }

    #[test]
    #[should_panic(expected = "path is empty")]
    fn a_node_at_an_empty_path_is_the_tests_own_fault() {
        let _tree = Files::new().dir("");
    }
}
