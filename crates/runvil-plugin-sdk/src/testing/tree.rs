//! [`Files`]: the tree of files a fake Host serves the tool calls of one
//! run. It is held in memory, every call of the run reads and changes the
//! one tree, and a test reads it back.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::{DirEntry, FileError, FileErrorKind, FileType, Stat};

/// The files of a fake run: a tree in memory, which a tool call reads
/// and changes through its [`ToolCall`](crate::ToolCall).
///
/// ```text
/// let files = Files::new()
///     .file("/notes/a.md", "one\ntwo\n")
///     .dir("/empty");
/// let pack = loaded.new_pack("acme/notes").unwrap().with_files(files);
/// ```
///
/// The calls of one [`PackRun`](super::PackRun) share the tree, so a
/// later call reads what an earlier one wrote, and
/// [`PackRun::files`](super::PackRun::files) lends it to the test:
///
/// ```text
/// pack.call("save", r#"{"path":"/notes/b.md","text":"three\n"}"#).unwrap();
/// assert_eq!(pack.files().read("/notes/b.md", 0, u64::MAX), Ok(b"three\n".to_vec()));
/// ```
///
/// A path is resolved from the root, as the Harness resolves it, so
/// `notes/a.md` and `/notes/a.md` name one file. A `.` segment and a
/// repeated `/` change nothing, a `..` segment steps up one directory, and
/// a `\` is read as `/`. An empty path, and a path that steps above the
/// root, fail.
///
/// Each method answers as the fake Host answers the file request of its
/// name, for a test that runs a tool's own code with no Plugin around it.
/// A change works as it does on the Harness's files in memory: a write
/// makes the directories on the way to its file, a path that is there
/// cannot be made again as a directory, and a path that is absent cannot
/// be removed. The methods that change the tree take `&self`, as a
/// [`ToolCall`](crate::ToolCall) changes files through a shared
/// reference. A clone is a tree of its own, with the nodes the first one
/// had when it was cloned.
#[derive(Debug, Default)]
pub struct Files {
    /// The nodes, behind a lock, so that a shared reference can change
    /// them.
    tree: Mutex<Tree>,
}

/// The nodes of a [`Files`] tree, and every operation on them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Tree {
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
    /// Something every file operation fails on, with this error.
    Fails(FileError),
}

impl Clone for Files {
    fn clone(&self) -> Files {
        Files {
            tree: Mutex::new(self.tree().clone()),
        }
    }
}

impl PartialEq for Files {
    fn eq(&self, other: &Files) -> bool {
        // One lock is held at a time, so a tree compared with itself
        // does not wait for its own lock.
        let nodes = self.tree().clone();
        nodes == *other.tree()
    }
}

impl Eq for Files {}

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
    /// Panics when `path` names the root, lies under something that is
    /// not a directory, or names a directory that has entries.
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

    /// The tree with something at `path` that every file operation fails
    /// on with `error`, a change included, as a path the run's policy
    /// denies does. Its directory lists it as an empty file. Nothing can
    /// be made under it: a change that would make something there fails
    /// with `error` too.
    ///
    /// # Panics
    /// Panics when `path` names the root, lies under something that is
    /// not a directory, or names a directory that has entries, which
    /// would stay readable.
    #[must_use]
    pub fn fails(self, path: &str, error: FileError) -> Files {
        self.with(path, Node::Fails(error))
    }

    /// The tree with `node` at `path`, in place of what was there.
    fn with(mut self, path: &str, node: Node) -> Files {
        self.tree
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .place(path, node);
        self
    }

    /// The nodes, locked. No operation can panic with a change half made,
    /// so a poisoned lock is opened all the same.
    fn tree(&self) -> MutexGuard<'_, Tree> {
        self.tree.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What `path` names, as the answer to `file_stat` reports it.
    ///
    /// # Errors
    /// Returns a [`FileError`] when nothing is at `path`, or when the
    /// tree was told to fail there.
    pub fn stat(&self, path: &str) -> Result<Stat, FileError> {
        self.tree().stat(path)
    }

    /// The entries of the directory at `path`, sorted by name, as the
    /// answer to `file_list` reports them.
    ///
    /// # Errors
    /// Returns a [`FileError`] when `path` names no directory, or when
    /// the tree was told to fail there.
    pub fn list(&self, path: &str) -> Result<Vec<DirEntry>, FileError> {
        self.tree().list(path)
    }

    /// Up to `len` bytes of the file at `path` from the byte `offset` on,
    /// as the answer to `file_read` reports them.
    ///
    /// # Errors
    /// Returns a [`FileError`] when `path` names no file, or when the
    /// tree was told to fail there.
    pub fn read(&self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>, FileError> {
        self.tree().read(path, offset, len)
    }

    /// Writes `contents` as the whole contents of the file at `path`, as
    /// the fake Host performs `file_write`. The file is made when it is
    /// absent, with every directory on the way to it.
    ///
    /// # Errors
    /// Returns a [`FileError`] when `path` names a directory, when
    /// something on the way to it is not a directory, or when the tree
    /// was told to fail there.
    pub fn write(&self, path: &str, contents: impl AsRef<[u8]>) -> Result<(), FileError> {
        self.tree().write(path, contents.as_ref())
    }

    /// Adds `contents` to the end of the file at `path`, as the fake Host
    /// performs `file_append`. The file is made when it is absent, as
    /// [`write`](Files::write) makes it.
    ///
    /// # Errors
    /// Returns a [`FileError`] when `path` names a directory, when
    /// something on the way to it is not a directory, or when the tree
    /// was told to fail there.
    pub fn append(&self, path: &str, contents: impl AsRef<[u8]>) -> Result<(), FileError> {
        self.tree().append(path, contents.as_ref())
    }

    /// Makes a directory at `path`, as the fake Host performs
    /// `file_mkdir`. With `recursive` every absent directory on the way
    /// to it is made too.
    ///
    /// # Errors
    /// Returns a [`FileError`] when something is at `path` already, when
    /// something on the way to it is not a directory, when a directory on
    /// the way is absent and `recursive` is off, or when the tree was told
    /// to fail there.
    pub fn mkdir(&self, path: &str, recursive: bool) -> Result<(), FileError> {
        self.tree().mkdir(path, recursive)
    }

    /// Removes the file or the directory at `path`, as the fake Host
    /// performs `file_remove`. With `recursive` a directory goes with
    /// everything in it.
    ///
    /// # Errors
    /// Returns a [`FileError`] when nothing is at `path`, when `path`
    /// names the root, when it names a directory that has entries and
    /// `recursive` is off, or when the tree was told to fail there.
    pub fn remove(&self, path: &str, recursive: bool) -> Result<(), FileError> {
        self.tree().remove(path, recursive)
    }

    /// Moves the file or the directory at `from` to `to`, as the fake
    /// Host performs `file_rename`. A directory moves with everything in
    /// it, and every absent directory on the way to `to` is made. A file
    /// at `to` gives way to a file, and an empty directory to a
    /// directory.
    ///
    /// # Errors
    /// Returns a [`FileError`], and moves nothing, when nothing is at
    /// `from`, when either path names the root, when `to` lies under
    /// `from`, when what is at `to` does not give way, when something on
    /// the way to `to` is not a directory, or when the tree was told to
    /// fail at either path.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), FileError> {
        self.tree().rename(from, to)
    }

    /// Copies the file at `from` to `to`, as the fake Host performs
    /// `file_copy`. Every absent directory on the way to `to` is made,
    /// and a file at `to` gives way.
    ///
    /// # Errors
    /// Returns a [`FileError`], and copies nothing, when `from` names no
    /// file, when `to` names a directory, when something on the way to
    /// `to` is not a directory, or when the tree was told to fail at
    /// either path.
    pub fn copy(&self, from: &str, to: &str) -> Result<(), FileError> {
        self.tree().copy(from, to)
    }
}

impl Tree {
    /// Puts `node` at `path`, in place of what was there, with every
    /// directory on the way to it.
    ///
    /// # Panics
    /// Panics when a test builds a tree no run could have.
    fn place(&mut self, path: &str, node: Node) {
        let path = resolve(path).unwrap_or_else(|error| panic!("{}: {path:?}", error.text()));
        assert!(path != "/", "the root is always a directory");
        for directory in way_to(&path) {
            let above = self
                .nodes
                .entry(directory.to_owned())
                .or_insert(Node::Directory);
            assert!(
                *above == Node::Directory,
                "{directory} is not a directory, so nothing lies under it"
            );
        }
        if node != Node::Directory {
            assert!(
                !self.has_entries(&path),
                "{path} is a directory with entries, so it stays one"
            );
        }
        self.nodes.insert(path, node);
    }

    /// What `path` names.
    fn stat(&self, path: &str) -> Result<Stat, FileError> {
        let path = resolve(path)?;
        Ok(match self.at(&path)? {
            Some(bytes) => Stat::new(FileType::File, len_of(bytes)),
            None => Stat::new(FileType::Directory, 0),
        })
    }

    /// The entries of the directory at `path`, sorted by name.
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, FileError> {
        let path = resolve(path)?;
        if self.at(&path)?.is_some() {
            return Err(not_a_directory(&path));
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

    /// Up to `len` bytes of the file at `path` from the byte `offset` on.
    fn read(&self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>, FileError> {
        let path = resolve(path)?;
        let Some(bytes) = self.at(&path)? else {
            return Err(is_a_directory(&path));
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let end = start
            .saturating_add(usize::try_from(len).unwrap_or(usize::MAX))
            .min(bytes.len());
        Ok(bytes[start..end].to_vec())
    }

    /// Writes `contents` as the whole contents of the file at `path`.
    fn write(&mut self, path: &str, contents: &[u8]) -> Result<(), FileError> {
        let path = resolve(path)?;
        self.check_file_place(&path)?;
        self.make_way(&path);
        self.nodes.insert(path, Node::File(contents.to_vec()));
        Ok(())
    }

    /// Adds `contents` to the end of the file at `path`, which is made
    /// when it is absent.
    fn append(&mut self, path: &str, contents: &[u8]) -> Result<(), FileError> {
        let resolved = resolve(path)?;
        if let Some(Node::File(bytes)) = self.nodes.get_mut(&resolved) {
            bytes.extend_from_slice(contents);
            return Ok(());
        }
        self.write(path, contents)
    }

    /// Makes a directory at `path`, and with `recursive` every absent
    /// directory on the way to it.
    fn mkdir(&mut self, path: &str, recursive: bool) -> Result<(), FileError> {
        let path = resolve(path)?;
        match self.nodes.get(&path) {
            Some(Node::Fails(error)) => return Err(error.clone()),
            Some(Node::File(_) | Node::Directory) => return Err(already_exists(&path)),
            None if path == "/" => return Err(already_exists(&path)),
            None => {}
        }
        self.check_way(&path)?;
        let whole = way_to(&path)
            .iter()
            .all(|directory| self.nodes.contains_key(*directory));
        if !recursive && !whole {
            // The error names the path the request named, as the Harness
            // names it, and not the directory that is absent.
            return Err(not_found(&path));
        }
        self.make_way(&path);
        self.nodes.insert(path, Node::Directory);
        Ok(())
    }

    /// Removes the file or the directory at `path`, and with `recursive`
    /// everything in a directory.
    fn remove(&mut self, path: &str, recursive: bool) -> Result<(), FileError> {
        let path = resolve(path)?;
        if path == "/" {
            return Err(root_protected());
        }
        match self.nodes.get(&path) {
            None => return Err(not_found(&path)),
            Some(Node::Fails(error)) => return Err(error.clone()),
            Some(Node::Directory) if !recursive && self.has_entries(&path) => {
                return Err(not_empty(&path));
            }
            Some(Node::File(_) | Node::Directory) => {}
        }
        let under = format!("{path}/");
        self.nodes
            .retain(|key, _| *key != path && !key.starts_with(&under));
        Ok(())
    }

    /// Moves the file or the directory at `from` to `to`, a directory
    /// with everything in it. Every check comes before the first change,
    /// so a move that fails moves nothing.
    fn rename(&mut self, from: &str, to: &str) -> Result<(), FileError> {
        let (from, to) = (resolve(from)?, resolve(to)?);
        if from == "/" {
            return Err(root_protected());
        }
        let under = format!("{from}/");
        if to.starts_with(&under) {
            return Err(FileError::new(
                FileErrorKind::Other,
                "a rename cannot move a directory into its own descendant",
            ));
        }
        match self.nodes.get(&from) {
            None => return Err(not_found(&from)),
            Some(Node::Fails(error)) => return Err(error.clone()),
            Some(Node::File(_)) => self.check_file_place(&to)?,
            Some(Node::Directory) => self.check_directory_place(&to)?,
        }
        let moving: Vec<String> = self
            .nodes
            .keys()
            .filter(|key| **key == from || key.starts_with(&under))
            .cloned()
            .collect();
        let moved: Vec<(String, Node)> = moving
            .into_iter()
            .filter_map(|key| {
                let node = self.nodes.remove(&key)?;
                Some((format!("{to}{}", &key[from.len()..]), node))
            })
            .collect();
        self.make_way(&to);
        self.nodes.extend(moved);
        Ok(())
    }

    /// Copies the file at `from` to `to`.
    fn copy(&mut self, from: &str, to: &str) -> Result<(), FileError> {
        let (from, to) = (resolve(from)?, resolve(to)?);
        let Some(bytes) = self.at(&from)?.map(<[u8]>::to_vec) else {
            return Err(is_a_directory(&from));
        };
        self.check_file_place(&to)?;
        self.make_way(&to);
        self.nodes.insert(to, Node::File(bytes));
        Ok(())
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
            None => Err(not_found(path)),
        }
    }

    /// Whether the directory at the resolved `path`, which is not the
    /// root, has entries.
    fn has_entries(&self, path: &str) -> bool {
        let under = format!("{path}/");
        self.nodes.keys().any(|key| key.starts_with(&under))
    }

    /// Fails when something on the way to the resolved `path` is not a
    /// directory: a file there is in the way, and a path told to fail
    /// fails with its error.
    fn check_way(&self, path: &str) -> Result<(), FileError> {
        for directory in way_to(path) {
            match self.nodes.get(directory) {
                Some(Node::File(_)) => return Err(not_a_directory(directory)),
                Some(Node::Fails(error)) => return Err(error.clone()),
                Some(Node::Directory) | None => {}
            }
        }
        Ok(())
    }

    /// Fails when no file can go to the resolved `path`: a directory is
    /// there, the path was told to fail, or something on the way to it is
    /// not a directory.
    fn check_file_place(&self, path: &str) -> Result<(), FileError> {
        if path == "/" {
            return Err(is_a_directory(path));
        }
        match self.nodes.get(path) {
            Some(Node::Directory) => Err(is_a_directory(path)),
            Some(Node::Fails(error)) => Err(error.clone()),
            Some(Node::File(_)) | None => self.check_way(path),
        }
    }

    /// Fails when no directory can move to the resolved `path`: the root,
    /// a file, or a directory with entries is there, the path was told to
    /// fail, or something on the way to it is not a directory.
    fn check_directory_place(&self, path: &str) -> Result<(), FileError> {
        if path == "/" {
            return Err(root_protected());
        }
        match self.nodes.get(path) {
            Some(Node::File(_)) => Err(not_a_directory(path)),
            Some(Node::Fails(error)) => Err(error.clone()),
            Some(Node::Directory) if self.has_entries(path) => Err(not_empty(path)),
            Some(Node::Directory) | None => self.check_way(path),
        }
    }

    /// Makes every directory that is absent on the way to the resolved
    /// `path`.
    fn make_way(&mut self, path: &str) {
        for directory in way_to(path) {
            self.nodes
                .entry(directory.to_owned())
                .or_insert(Node::Directory);
        }
    }
}

/// The failure of an operation that found nothing at `path`.
fn not_found(path: &str) -> FileError {
    FileError::new(FileErrorKind::NotFound, format!("not found: {path}"))
}

/// The failure of an operation that needs a directory at `path` and
/// found something else.
fn not_a_directory(path: &str) -> FileError {
    FileError::new(
        FileErrorKind::NotADirectory,
        format!("not a directory: {path}"),
    )
}

/// The failure of an operation that needs a file at `path` and found a
/// directory.
fn is_a_directory(path: &str) -> FileError {
    FileError::new(
        FileErrorKind::IsADirectory,
        format!("is a directory: {path}"),
    )
}

/// The failure of an operation that needs nothing at `path` and found
/// something.
fn already_exists(path: &str) -> FileError {
    FileError::new(
        FileErrorKind::AlreadyExists,
        format!("already exists: {path}"),
    )
}

/// The failure of an operation that needs an empty directory at `path`
/// and found one with entries.
fn not_empty(path: &str) -> FileError {
    FileError::new(
        FileErrorKind::NotEmpty,
        format!("directory not empty: {path}"),
    )
}

/// The failure of an operation that would remove or move the root, or
/// move something onto it.
fn root_protected() -> FileError {
    FileError::new(
        FileErrorKind::Other,
        "the root / cannot be removed or moved, and nothing can be moved onto it",
    )
}

/// `path` from the root, with one `/` before each segment: `/` alone for
/// the root.
fn resolve(path: &str) -> Result<String, FileError> {
    if path.is_empty() {
        return Err(FileError::new(FileErrorKind::Other, "path is empty"));
    }
    let mut segments = Vec::new();
    for segment in path.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(FileError::new(
                        FileErrorKind::Other,
                        "path contains a traversal segment",
                    ));
                }
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

/// The directories on the way to the resolved `path`, from the root
/// down, without the root itself.
fn way_to(path: &str) -> Vec<&str> {
    let mut way = Vec::new();
    let mut directory = parent_of(path);
    while directory != "/" {
        way.push(directory);
        directory = parent_of(directory);
    }
    way.reverse();
    way
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

    /// The whole contents of the file at `path`.
    fn contents(tree: &Files, path: &str) -> Result<Vec<u8>, FileError> {
        tree.read(path, 0, u64::MAX)
    }

    /// The names in the directory at `path`.
    fn names(tree: &Files, path: &str) -> Vec<String> {
        let entries = tree.list(path).unwrap();
        entries.into_iter().map(|entry| entry.name).collect()
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
            ("/a/..", "/"),
            ("a\\b", "/a/b"),
            (".", "/"),
        ] {
            assert_eq!(resolve(path).unwrap(), resolved, "{path}");
        }
        assert_eq!(
            failure(resolve("")),
            (FileErrorKind::Other, "path is empty".to_owned())
        );
        for above in ["..", "/..", "/a/../..", "a/../../b"] {
            assert_eq!(
                failure(resolve(above)),
                (
                    FileErrorKind::Other,
                    "path contains a traversal segment".to_owned()
                ),
                "{above}"
            );
        }
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
    fn write_makes_a_file_with_the_directories_on_the_way_and_replaces_contents() {
        let tree = tree();
        assert_eq!(tree.write("/new/deep/c.md", "one"), Ok(()));
        assert_eq!(contents(&tree, "/new/deep/c.md"), Ok(b"one".to_vec()));
        assert_eq!(names(&tree, "/new"), ["deep"]);
        assert_eq!(tree.write("new/deep/c.md", "two"), Ok(()));
        assert_eq!(contents(&tree, "/new/deep/c.md"), Ok(b"two".to_vec()));
        assert_eq!(tree.write("/notes/b.md", [0xff_u8, 0x00]), Ok(()));
        assert_eq!(contents(&tree, "/notes/b.md"), Ok(vec![0xff, 0x00]));
    }

    #[test]
    fn write_fails_on_a_directory_and_under_a_file_and_changes_nothing() {
        let tree = tree();
        let before = tree.clone();
        for directory in ["/notes", "/"] {
            assert_eq!(
                failure(tree.write(directory, "x")),
                (
                    FileErrorKind::IsADirectory,
                    format!("is a directory: {directory}")
                )
            );
        }
        assert_eq!(
            failure(tree.write("/notes/b.md/c", "x")),
            (
                FileErrorKind::NotADirectory,
                "not a directory: /notes/b.md".to_owned()
            )
        );
        assert_eq!(
            failure(tree.write("", "x")),
            (FileErrorKind::Other, "path is empty".to_owned())
        );
        assert_eq!(tree, before);
    }

    #[test]
    fn append_adds_to_the_end_of_a_file_and_makes_one_that_is_absent() {
        let tree = tree();
        assert_eq!(tree.append("/notes/deep/a.md", " there"), Ok(()));
        assert_eq!(
            contents(&tree, "/notes/deep/a.md"),
            Ok(b"hello there".to_vec())
        );
        assert_eq!(tree.append("/log/out.txt", "first"), Ok(()));
        assert_eq!(contents(&tree, "/log/out.txt"), Ok(b"first".to_vec()));
        assert_eq!(
            failure(tree.append("/notes", "x")),
            (
                FileErrorKind::IsADirectory,
                "is a directory: /notes".to_owned()
            )
        );
    }

    #[test]
    fn mkdir_makes_one_directory_and_with_recursive_the_whole_way() {
        let tree = tree();
        assert_eq!(tree.mkdir("/made", false), Ok(()));
        assert_eq!(tree.stat("/made"), Ok(Stat::new(FileType::Directory, 0)));
        assert_eq!(
            failure(tree.mkdir("/a/b/c", false)),
            (FileErrorKind::NotFound, "not found: /a/b/c".to_owned())
        );
        assert_eq!(
            failure(tree.stat("/a")),
            (FileErrorKind::NotFound, "not found: /a".to_owned())
        );
        assert_eq!(tree.mkdir("/a/b/c", true), Ok(()));
        assert_eq!(names(&tree, "/a"), ["b"]);
        assert_eq!(names(&tree, "/a/b"), ["c"]);
    }

    #[test]
    fn mkdir_fails_on_what_is_there_and_under_a_file() {
        let tree = tree();
        for (there, recursive) in [
            ("/notes", false),
            ("/notes", true),
            ("/notes/b.md", false),
            ("/", true),
        ] {
            assert_eq!(
                failure(tree.mkdir(there, recursive)),
                (
                    FileErrorKind::AlreadyExists,
                    format!("already exists: {there}")
                )
            );
        }
        assert_eq!(
            failure(tree.mkdir("/notes/b.md/c", true)),
            (
                FileErrorKind::NotADirectory,
                "not a directory: /notes/b.md".to_owned()
            )
        );
    }

    #[test]
    fn remove_takes_a_file_an_empty_directory_and_with_recursive_a_full_one() {
        let tree = tree();
        assert_eq!(tree.remove("/notes/b.md", false), Ok(()));
        assert_eq!(tree.remove("/empty", false), Ok(()));
        assert_eq!(names(&tree, "/"), ["notes"]);
        assert_eq!(
            failure(tree.remove("/notes", false)),
            (
                FileErrorKind::NotEmpty,
                "directory not empty: /notes".to_owned()
            )
        );
        assert_eq!(names(&tree, "/notes"), ["deep", "secret"]);
        assert_eq!(tree.remove("/notes", true), Ok(()));
        assert_eq!(tree, Files::new());
    }

    #[test]
    fn remove_fails_on_an_absent_path_and_on_the_root() {
        let tree = tree();
        assert_eq!(
            failure(tree.remove("/absent", true)),
            (FileErrorKind::NotFound, "not found: /absent".to_owned())
        );
        assert_eq!(
            failure(tree.remove("/", true)),
            (
                FileErrorKind::Other,
                "the root / cannot be removed or moved, and nothing can be moved onto it"
                    .to_owned()
            )
        );
        // A name that begins as a removed one does is another node.
        let tree = Files::new().file("/a", "x").file("/ab", "y");
        assert_eq!(tree.remove("/a", true), Ok(()));
        assert_eq!(names(&tree, "/"), ["ab"]);
    }

    #[test]
    fn rename_moves_a_file_and_makes_the_way_to_its_new_path() {
        let tree = tree();
        assert_eq!(tree.rename("/notes/deep/a.md", "/moved/to/a.md"), Ok(()));
        assert_eq!(contents(&tree, "/moved/to/a.md"), Ok(b"hello".to_vec()));
        assert_eq!(names(&tree, "/notes/deep"), Vec::<String>::new());
        // A file at the new path gives way.
        assert_eq!(tree.rename("/moved/to/a.md", "/notes/b.md"), Ok(()));
        assert_eq!(contents(&tree, "/notes/b.md"), Ok(b"hello".to_vec()));
        assert_eq!(tree.rename("/notes/b.md", "/notes/b.md"), Ok(()));
        assert_eq!(contents(&tree, "/notes/b.md"), Ok(b"hello".to_vec()));
    }

    #[test]
    fn rename_moves_a_directory_with_everything_in_it() {
        let tree = tree();
        assert_eq!(tree.rename("/notes", "/empty"), Ok(()));
        assert_eq!(names(&tree, "/"), ["empty"]);
        assert_eq!(names(&tree, "/empty"), ["b.md", "deep", "secret"]);
        assert_eq!(contents(&tree, "/empty/deep/a.md"), Ok(b"hello".to_vec()));
        // A path told to fail moves with its directory.
        assert_eq!(tree.stat("/empty/secret"), Err(denied()));
    }

    #[test]
    fn a_rename_that_fails_moves_nothing() {
        let tree = tree();
        let before = tree.clone();
        let root = "the root / cannot be removed or moved, and nothing can be moved onto it";
        for (from, to, kind, text) in [
            (
                "/absent",
                "/x",
                FileErrorKind::NotFound,
                "not found: /absent",
            ),
            ("/", "/x", FileErrorKind::Other, root),
            ("/notes", "/", FileErrorKind::Other, root),
            (
                "/notes",
                "/notes/deep/in",
                FileErrorKind::Other,
                "a rename cannot move a directory into its own descendant",
            ),
            (
                "/empty",
                "/notes/b.md",
                FileErrorKind::NotADirectory,
                "not a directory: /notes/b.md",
            ),
            (
                "/empty",
                "/notes",
                FileErrorKind::NotEmpty,
                "directory not empty: /notes",
            ),
            (
                "/notes/b.md",
                "/empty",
                FileErrorKind::IsADirectory,
                "is a directory: /empty",
            ),
            (
                "/empty",
                "/notes/b.md/in",
                FileErrorKind::NotADirectory,
                "not a directory: /notes/b.md",
            ),
        ] {
            assert_eq!(
                failure(tree.rename(from, to)),
                (kind, text.to_owned()),
                "{from} to {to}"
            );
        }
        assert_eq!(tree, before);
    }

    #[test]
    fn copy_copies_one_file_and_makes_the_way_to_the_copy() {
        let tree = tree();
        assert_eq!(tree.copy("/notes/deep/a.md", "/copies/a.md"), Ok(()));
        assert_eq!(contents(&tree, "/copies/a.md"), Ok(b"hello".to_vec()));
        assert_eq!(contents(&tree, "/notes/deep/a.md"), Ok(b"hello".to_vec()));
        // A file at the copy's path gives way.
        assert_eq!(tree.copy("/notes/deep/a.md", "/notes/b.md"), Ok(()));
        assert_eq!(contents(&tree, "/notes/b.md"), Ok(b"hello".to_vec()));
    }

    #[test]
    fn a_copy_that_fails_copies_nothing() {
        let tree = tree();
        let before = tree.clone();
        for (from, to, kind, text) in [
            (
                "/absent",
                "/x",
                FileErrorKind::NotFound,
                "not found: /absent",
            ),
            (
                "/notes",
                "/x",
                FileErrorKind::IsADirectory,
                "is a directory: /notes",
            ),
            (
                "/notes/b.md",
                "/empty",
                FileErrorKind::IsADirectory,
                "is a directory: /empty",
            ),
            (
                "/notes/b.md",
                "/notes/deep/a.md/in",
                FileErrorKind::NotADirectory,
                "not a directory: /notes/deep/a.md",
            ),
        ] {
            assert_eq!(
                failure(tree.copy(from, to)),
                (kind, text.to_owned()),
                "{from} to {to}"
            );
        }
        assert_eq!(tree, before);
    }

    #[test]
    fn a_path_told_to_fail_fails_every_operation_with_its_error() {
        let tree = tree();
        let before = tree.clone();
        let secret = "/notes/secret";
        assert_eq!(tree.stat(secret), Err(denied()));
        assert_eq!(tree.list(secret), Err(denied()));
        assert_eq!(tree.read(secret, 0, 1), Err(denied()));
        assert_eq!(tree.write(secret, "x"), Err(denied()));
        assert_eq!(tree.append(secret, "x"), Err(denied()));
        assert_eq!(tree.mkdir(secret, true), Err(denied()));
        assert_eq!(tree.remove(secret, true), Err(denied()));
        assert_eq!(tree.rename(secret, "/x"), Err(denied()));
        assert_eq!(tree.rename("/notes/b.md", secret), Err(denied()));
        assert_eq!(tree.rename("/empty", secret), Err(denied()));
        assert_eq!(tree.copy(secret, "/x"), Err(denied()));
        assert_eq!(tree.copy("/notes/b.md", secret), Err(denied()));
        assert_eq!(tree, before);
    }

    #[test]
    fn nothing_can_be_made_under_a_path_told_to_fail() {
        let tree = tree();
        let before = tree.clone();
        let under = "/notes/secret/in";
        assert_eq!(tree.write(under, "x"), Err(denied()));
        assert_eq!(tree.append(under, "x"), Err(denied()));
        assert_eq!(tree.mkdir(under, true), Err(denied()));
        assert_eq!(tree.rename("/notes/b.md", under), Err(denied()));
        assert_eq!(tree.rename("/empty", under), Err(denied()));
        assert_eq!(tree.copy("/notes/b.md", under), Err(denied()));
        assert_eq!(tree, before);
    }

    #[test]
    fn a_clone_is_a_tree_of_its_own_and_equal_trees_hold_equal_nodes() {
        let tree = tree();
        let clone = tree.clone();
        assert_eq!(tree, clone);
        let itself = &tree;
        assert!(tree == *itself, "a tree compared with itself");
        assert_eq!(clone.write("/notes/b.md", "more"), Ok(()));
        assert_ne!(tree, clone);
        assert_eq!(contents(&tree, "/notes/b.md"), Ok(Vec::new()));
        assert_eq!(tree.write("/notes/b.md", "more"), Ok(()));
        assert_eq!(tree, clone);
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
    #[should_panic(expected = "/secret is a directory with entries, so it stays one")]
    fn a_failure_over_a_directory_with_entries_is_the_tests_own_fault() {
        let _tree = Files::new()
            .file("/secret/key", "k")
            .fails("/secret", denied());
    }

    #[test]
    fn an_empty_directory_can_become_a_file_or_a_failure() {
        let tree = Files::new()
            .dir("/a")
            .dir("/b")
            .file("/a", "x")
            .fails("/b", denied());
        assert_eq!(tree.read("/a", 0, u64::MAX), Ok(b"x".to_vec()));
        assert_eq!(tree.stat("/b"), Err(denied()));
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
