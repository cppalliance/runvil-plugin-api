//! What a tool call learns of its run's files: the [`Stat`] of a path,
//! the [`DirEntry`] values of a directory, and the [`FileError`] a file
//! method of [`ToolCall`](crate::ToolCall) fails with. Each file method
//! returns a [`FileFuture`], and the `Requests` of its call hold the
//! request until a step hands it to the Host, and the Host's answer until
//! the future takes it.

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use runvil_plugin_sys::{
    RV_FILE_ERROR_ALREADY_EXISTS, RV_FILE_ERROR_CONFLICT, RV_FILE_ERROR_DENIED, RV_FILE_ERROR_IO,
    RV_FILE_ERROR_IS_A_DIRECTORY, RV_FILE_ERROR_NOT_A_DIRECTORY, RV_FILE_ERROR_NOT_EMPTY,
    RV_FILE_ERROR_NOT_FOUND, RV_FILE_TYPE_DIRECTORY, RV_FILE_TYPE_FILE, RV_FILE_TYPE_SYMLINK,
    RV_MAX_PATH, RV_MAX_WRITE, RvFileErrorKind, RvFileType,
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
    /// The run's policy does not allow the operation. The error's text is
    /// the policy's own reason.
    Denied,
    /// Another tool call of the run changed the path, or is changing it,
    /// at the same time. The same operation can succeed in a later call.
    Conflict,
    /// The operating system failed the operation.
    Io,
    /// Something is at the path already, where the method needs nothing
    /// there.
    AlreadyExists,
    /// The path names a directory that has entries, where the method
    /// needs an empty one.
    NotEmpty,
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
            RV_FILE_ERROR_ALREADY_EXISTS => FileErrorKind::AlreadyExists,
            RV_FILE_ERROR_NOT_EMPTY => FileErrorKind::NotEmpty,
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
            FileErrorKind::AlreadyExists => RV_FILE_ERROR_ALREADY_EXISTS,
            FileErrorKind::NotEmpty => RV_FILE_ERROR_NOT_EMPTY,
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

/// One file operation a tool call asks its Host for, with copies of the
/// paths and the bytes the call named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    /// What the path names.
    Stat(String),
    /// The entries of the directory at the path.
    List(String),
    /// Up to `len` bytes of the file at `path`, from the byte `offset` on.
    Read {
        /// The file's path.
        path: String,
        /// The first byte to read.
        offset: u64,
        /// The most bytes to read.
        len: u64,
    },
    /// A write of `bytes` as the whole contents of the file at `path`.
    Write {
        /// The file's path.
        path: String,
        /// The contents the file gets.
        bytes: Vec<u8>,
    },
    /// An append of `bytes` to the end of the file at `path`.
    Append {
        /// The file's path.
        path: String,
        /// What the file gains.
        bytes: Vec<u8>,
    },
    /// A new directory at `path`.
    Mkdir {
        /// The directory's path.
        path: String,
        /// Whether each absent directory on the way to it is made too.
        recursive: bool,
    },
    /// The removal of the file or the directory at `path`.
    Remove {
        /// The path to remove.
        path: String,
        /// Whether a directory goes with everything in it.
        recursive: bool,
    },
    /// A move of the file or the directory at `from` to `to`.
    Rename {
        /// The path to move.
        from: String,
        /// Where it moves to.
        to: String,
    },
    /// A copy of the file at `from` to `to`.
    Copy {
        /// The file to copy.
        from: String,
        /// Where the copy goes.
        to: String,
    },
}

impl Request {
    /// Why the Host would refuse the request as a misuse, when it would:
    /// a path or the bytes are longer than the contract allows. A misuse
    /// fails the whole tool call, so such a request never goes to the
    /// Host.
    fn fault(&self) -> Option<FileError> {
        let (paths, bytes): ([&str; 2], &[u8]) = match self {
            Request::Stat(path)
            | Request::List(path)
            | Request::Read { path, .. }
            | Request::Mkdir { path, .. }
            | Request::Remove { path, .. } => ([path, ""], &[]),
            Request::Write { path, bytes } | Request::Append { path, bytes } => ([path, ""], bytes),
            Request::Rename { from, to } | Request::Copy { from, to } => ([from, to], &[]),
        };
        let fault = if paths.iter().any(|path| path.len() > RV_MAX_PATH) {
            format!("the path is longer than {RV_MAX_PATH} bytes")
        } else if bytes.len() > RV_MAX_WRITE {
            format!("the bytes are longer than {RV_MAX_WRITE} bytes")
        } else {
            return None;
        };
        Some(FileError::new(FileErrorKind::Other, fault))
    }
}

/// What the Host answered a request with, copied out of the Host's
/// memory. The one field the request asked for is filled, and the others
/// are empty. A request that changes the files asked for none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Answer {
    /// What a path names, for [`Request::Stat`].
    pub(crate) stat: Stat,
    /// The entries of a directory, for [`Request::List`].
    pub(crate) entries: Vec<DirEntry>,
    /// The bytes of a file, for [`Request::Read`].
    pub(crate) bytes: Vec<u8>,
}

/// The file requests of one tool call, on their way to the Host and
/// back. A [`FileFuture`] queues its request here, each step hands the
/// oldest one to the Host, and the next step puts the Host's answer here
/// for the future to take. The Host holds one request of a call at a
/// time, so the others wait in the order their futures were first polled.
#[derive(Debug, Default)]
pub(crate) struct Requests {
    queue: Mutex<Queue>,
}

/// What [`Requests`] keeps behind its lock.
#[derive(Debug, Default)]
struct Queue {
    /// The number the next request gets. A number names one request from
    /// the first poll of its future until that future has the answer.
    next: u64,
    /// The requests not yet sent, oldest first, each under its number.
    waiting: VecDeque<(u64, Request)>,
    /// The request that is with the Host.
    sent: Option<Sent>,
    /// The answers not yet taken, each under the number of its request.
    answers: Vec<(u64, Result<Answer, FileError>)>,
}

/// The request that is with the Host.
#[derive(Debug)]
struct Sent {
    /// The request's number.
    number: u64,
    /// Whether the request's future still waits. The answer to a request
    /// whose future was dropped is discarded.
    awaited: bool,
}

impl Requests {
    /// The queue, locked. It holds plain data that no panic leaves half
    /// written, so a poisoned lock is opened all the same.
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `request` behind the ones already waiting, and returns the
    /// number that names it.
    fn ask(&self, request: Request) -> u64 {
        let mut queue = self.lock();
        let number = queue.next;
        queue.next += 1;
        queue.waiting.push_back((number, request));
        number
    }

    /// Takes the answer to the request `number`, once a step has brought
    /// it.
    fn take(&self, number: u64) -> Option<Result<Answer, FileError>> {
        let mut queue = self.lock();
        let at = queue
            .answers
            .iter()
            .position(|(answered, _)| *answered == number)?;
        Some(queue.answers.swap_remove(at).1)
    }

    /// Lets go of the request `number`, whose future was dropped. A
    /// request not yet sent leaves the queue, so the Host never sees it.
    /// One that is with the Host stays there, and its answer is discarded
    /// when it comes. An answer already here is discarded now.
    fn forget(&self, number: u64) {
        let mut queue = self.lock();
        queue.waiting.retain(|(waiting, _)| *waiting != number);
        queue.answers.retain(|(answered, _)| *answered != number);
        if let Some(sent) = queue.sent.as_mut().filter(|sent| sent.number == number) {
            sent.awaited = false;
        }
    }

    /// Whether a request is with the Host, so that the next step brings
    /// its answer.
    pub(crate) fn is_sent(&self) -> bool {
        self.lock().sent.is_some()
    }

    /// Keeps `answer` as the answer to the request that is with the Host,
    /// which the Host then holds no more. The answer is discarded when the
    /// request's future was dropped, and when no request is with the Host.
    pub(crate) fn answered(&self, answer: Result<Answer, FileError>) {
        let mut queue = self.lock();
        if let Some(sent) = queue.sent.take().filter(|sent| sent.awaited) {
            queue.answers.push((sent.number, answer));
        }
    }

    /// Hands the oldest request not yet sent to `hand_over`, when no
    /// request is with the Host. The request is with the Host when
    /// `hand_over` returns `Ok`, and otherwise the error is the answer its
    /// future gets.
    ///
    /// Returns whether `hand_over` refused the request. Its answer is
    /// then here already, with no future polled since to take it and no
    /// request at the Host whose answer would bring the call's next
    /// step, so the caller wakes the call.
    ///
    /// The lock is held while `hand_over` runs. It calls a file function
    /// of the Host, which only takes the request and so reaches nothing
    /// here.
    pub(crate) fn send(&self, hand_over: impl FnOnce(&Request) -> Result<(), FileError>) -> bool {
        let mut queue = self.lock();
        if queue.sent.is_some() {
            return false;
        }
        let Some((number, request)) = queue.waiting.pop_front() else {
            return false;
        };
        match hand_over(&request) {
            Ok(()) => {
                queue.sent = Some(Sent {
                    number,
                    awaited: true,
                });
                false
            }
            Err(error) => {
                queue.answers.push((number, Err(error)));
                true
            }
        }
    }
}

/// The future a file method of [`ToolCall`](crate::ToolCall) returns:
/// one request to the Host, and then the Host's answer.
///
/// The future copies its paths and its bytes when it is made, so it
/// borrows nothing but its call. Its first poll queues the request, and
/// the step that polled it ends by handing the oldest queued request of
/// the call to the Host. The Host holds one request of a call at a time,
/// so further requests wait their turn, in the order their futures were
/// first polled, and go out one with each step. The answer arrives with
/// the step after the request went out, and the future is ready when
/// that step, or a later one, polls it.
///
/// The future gets its answer only when a step polls it: it registers no
/// waker, because the step that brings the answer polls the call's
/// future. So it does not work inside a set that polls a child only
/// after that child's waker fired, such as `FuturesUnordered`. A future
/// that awaits it directly, or through a combinator that polls every
/// child at each poll, such as `join!` or `select!`, gets the answer.
///
/// A future that is dropped before its request went to the Host sends
/// nothing. One that is dropped later leaves the operation to happen, and
/// its answer is discarded.
#[derive(Debug)]
#[must_use = "a future does nothing unless it is awaited"]
pub struct FileFuture<'a, T> {
    /// The requests of the call this one belongs to.
    requests: &'a Requests,
    /// How far the request has come.
    stage: Stage,
    /// Picks what the method returns out of the Host's answer.
    pick: fn(Answer) -> T,
}

/// How far the request of a [`FileFuture`] has come.
#[derive(Debug)]
enum Stage {
    /// Not polled yet, with the request the first poll queues.
    Made(Request),
    /// Not polled yet, with the failure of a request that never goes to
    /// the Host.
    Faulty(FileError),
    /// Queued under this number: waiting to go to the Host, or for the
    /// Host's answer.
    Asked(u64),
    /// Ready, with nothing more to give.
    Done,
}

impl<'a, T> FileFuture<'a, T> {
    /// A future that asks for `request` through `requests`, and resolves
    /// to what `pick` takes out of the Host's answer.
    pub(crate) fn new(
        requests: &'a Requests,
        request: Request,
        pick: fn(Answer) -> T,
    ) -> FileFuture<'a, T> {
        let stage = match request.fault() {
            Some(fault) => Stage::Faulty(fault),
            None => Stage::Made(request),
        };
        FileFuture {
            requests,
            stage,
            pick,
        }
    }
}

impl<T> Future for FileFuture<'_, T> {
    type Output = Result<T, FileError>;

    /// The future registers no waker. The Host sends the step that
    /// brings the answer when the answer is ready, without a wake, and
    /// that step polls the call's future.
    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        match mem::replace(&mut self.stage, Stage::Done) {
            Stage::Made(request) => {
                self.stage = Stage::Asked(self.requests.ask(request));
                Poll::Pending
            }
            Stage::Faulty(fault) => Poll::Ready(Err(fault)),
            Stage::Asked(number) => {
                if let Some(answer) = self.requests.take(number) {
                    return Poll::Ready(answer.map(self.pick));
                }
                self.stage = Stage::Asked(number);
                Poll::Pending
            }
            Stage::Done => Poll::Ready(Err(FileError::new(
                FileErrorKind::Other,
                "the file future was polled after it was ready",
            ))),
        }
    }
}

impl<T> Drop for FileFuture<'_, T> {
    fn drop(&mut self) {
        if let Stage::Asked(number) = self.stage {
            self.requests.forget(number);
        }
    }
}

// A file method's future is `Send`, as the future of its call must be.
const _: () = {
    const fn is_send<T: Send>() {}
    is_send::<FileFuture<'static, Stat>>();
    is_send::<FileFuture<'static, Vec<DirEntry>>>();
    is_send::<FileFuture<'static, Vec<u8>>>();
    is_send::<FileFuture<'static, ()>>();
};

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    use super::{
        Answer, DirEntry, FileError, FileErrorKind, FileFuture, FileType, Request, Requests, Stat,
    };

    /// Polls `future` once, as a step does, with a waker that does
    /// nothing, because a file future registers none.
    fn poll<T>(future: &mut FileFuture<'_, T>) -> Poll<Result<T, FileError>> {
        Pin::new(future).poll(&mut Context::from_waker(Waker::noop()))
    }

    /// A future that asks `requests` what `path` names.
    fn stat<'a>(requests: &'a Requests, path: &str) -> FileFuture<'a, Stat> {
        FileFuture::new(requests, Request::Stat(path.to_owned()), |answer| {
            answer.stat
        })
    }

    /// The answer to a `stat` of a file that is `len` bytes long.
    fn file_of(len: u64) -> Answer {
        Answer {
            stat: Stat::new(FileType::File, len),
            entries: Vec::new(),
            bytes: Vec::new(),
        }
    }

    /// Runs one step's send, and returns the request the Host took.
    fn sent(requests: &Requests) -> Option<Request> {
        let mut taken = None;
        requests.send(|request| {
            taken = Some(request.clone());
            Ok(())
        });
        taken
    }

    /// How many answers wait in `requests` for their futures.
    fn untaken(requests: &Requests) -> usize {
        requests.lock().answers.len()
    }

    #[test]
    fn a_future_queues_its_request_at_its_first_poll_and_not_before() {
        let requests = Requests::default();
        let mut future = stat(&requests, "/a");
        assert_eq!(sent(&requests), None, "a future that was never polled");
        assert_eq!(poll(&mut future), Poll::Pending);
        assert_eq!(sent(&requests), Some(Request::Stat("/a".to_owned())));
    }

    #[test]
    fn a_future_is_ready_once_a_step_has_brought_its_answer() {
        let requests = Requests::default();
        let mut future = stat(&requests, "/a");
        assert_eq!(poll(&mut future), Poll::Pending);
        assert!(!requests.is_sent());
        assert!(sent(&requests).is_some());
        assert!(requests.is_sent());
        assert_eq!(
            poll(&mut future),
            Poll::Pending,
            "the Host has not answered"
        );
        requests.answered(Ok(file_of(5)));
        assert!(!requests.is_sent());
        assert_eq!(
            poll(&mut future),
            Poll::Ready(Ok(Stat::new(FileType::File, 5)))
        );
    }

    #[test]
    fn a_failed_operation_reaches_its_future_as_the_error() {
        let requests = Requests::default();
        let mut future = stat(&requests, "/a");
        assert_eq!(poll(&mut future), Poll::Pending);
        assert!(sent(&requests).is_some());
        let denied = FileError::new(FileErrorKind::Denied, "the mode allows only /notes");
        requests.answered(Err(denied.clone()));
        assert_eq!(poll(&mut future), Poll::Ready(Err(denied)));
    }

    #[test]
    fn requests_go_to_the_host_one_at_a_time_in_the_order_they_were_asked() {
        let requests = Requests::default();
        let mut first = stat(&requests, "/a");
        let mut second = stat(&requests, "/b");
        // The second future is polled first, so its request is the older.
        assert_eq!(poll(&mut second), Poll::Pending);
        assert_eq!(poll(&mut first), Poll::Pending);
        assert_eq!(sent(&requests), Some(Request::Stat("/b".to_owned())));
        assert_eq!(sent(&requests), None, "one request is with the Host");
        requests.answered(Ok(file_of(2)));
        assert_eq!(sent(&requests), Some(Request::Stat("/a".to_owned())));
        assert_eq!(poll(&mut first), Poll::Pending);
        assert_eq!(
            poll(&mut second),
            Poll::Ready(Ok(Stat::new(FileType::File, 2)))
        );
        requests.answered(Ok(file_of(1)));
        assert_eq!(
            poll(&mut first),
            Poll::Ready(Ok(Stat::new(FileType::File, 1)))
        );
    }

    #[test]
    fn a_future_dropped_before_its_request_went_out_sends_nothing() {
        let requests = Requests::default();
        let mut dropped = stat(&requests, "/a");
        let mut kept = stat(&requests, "/b");
        assert_eq!(poll(&mut dropped), Poll::Pending);
        assert_eq!(poll(&mut kept), Poll::Pending);
        drop(dropped);
        assert_eq!(sent(&requests), Some(Request::Stat("/b".to_owned())));
    }

    #[test]
    fn a_future_dropped_after_its_request_went_out_has_its_answer_discarded() {
        let requests = Requests::default();
        let mut dropped = stat(&requests, "/a");
        let mut kept = stat(&requests, "/b");
        assert_eq!(poll(&mut dropped), Poll::Pending);
        assert_eq!(poll(&mut kept), Poll::Pending);
        assert_eq!(sent(&requests), Some(Request::Stat("/a".to_owned())));
        drop(dropped);
        assert!(requests.is_sent(), "the Host still performs the operation");
        requests.answered(Ok(file_of(1)));
        assert_eq!(untaken(&requests), 0);
        assert_eq!(sent(&requests), Some(Request::Stat("/b".to_owned())));
        requests.answered(Ok(file_of(2)));
        assert_eq!(
            poll(&mut kept),
            Poll::Ready(Ok(Stat::new(FileType::File, 2)))
        );
    }

    #[test]
    fn a_future_dropped_with_its_answer_untaken_leaves_none_behind() {
        let requests = Requests::default();
        let mut future = stat(&requests, "/a");
        assert_eq!(poll(&mut future), Poll::Pending);
        assert!(sent(&requests).is_some());
        requests.answered(Ok(file_of(1)));
        assert_eq!(untaken(&requests), 1);
        drop(future);
        assert_eq!(untaken(&requests), 0);
    }

    #[test]
    fn an_answer_with_no_request_at_the_host_is_discarded() {
        let requests = Requests::default();
        requests.answered(Ok(file_of(1)));
        assert_eq!(untaken(&requests), 0);
    }

    #[test]
    fn a_request_the_host_does_not_take_fails_its_future_and_frees_the_turn() {
        let requests = Requests::default();
        let mut refused = stat(&requests, "/a");
        let mut next = stat(&requests, "/b");
        assert_eq!(poll(&mut refused), Poll::Pending);
        assert_eq!(poll(&mut next), Poll::Pending);
        let fault = FileError::new(FileErrorKind::Other, "status 4");
        assert!(requests.send(|_| Err(fault.clone())));
        assert!(!requests.is_sent());
        assert_eq!(poll(&mut refused), Poll::Ready(Err(fault)));
        assert_eq!(sent(&requests), Some(Request::Stat("/b".to_owned())));
    }

    #[test]
    fn a_send_reports_a_refusal_and_nothing_else() {
        let requests = Requests::default();
        let refuse = |_: &Request| Err(FileError::new(FileErrorKind::Other, "status 4"));
        assert!(!requests.send(refuse), "no request waits");
        let mut first = stat(&requests, "/a");
        let mut second = stat(&requests, "/b");
        assert_eq!(poll(&mut first), Poll::Pending);
        assert_eq!(poll(&mut second), Poll::Pending);
        assert!(!requests.send(|_| Ok(())), "the Host took the request");
        assert!(!requests.send(refuse), "one request is with the Host");
        // The step that brings an answer polls the call's future, so an
        // answer that future leaves untaken is no reason to wake.
        requests.answered(Ok(file_of(1)));
        assert!(!requests.send(|_| Ok(())), "the Host took the next request");
        requests.answered(Ok(file_of(2)));
        assert_eq!(untaken(&requests), 2);
        assert!(!requests.send(refuse), "no request waits");
    }

    #[test]
    fn a_path_longer_than_the_contract_allows_fails_without_a_request() {
        let requests = Requests::default();
        let mut long = stat(&requests, &"a".repeat(4097));
        assert_eq!(
            poll(&mut long),
            Poll::Ready(Err(FileError::new(
                FileErrorKind::Other,
                "the path is longer than 4096 bytes"
            )))
        );
        assert_eq!(sent(&requests), None);
        let mut longest = stat(&requests, &"a".repeat(4096));
        assert_eq!(poll(&mut longest), Poll::Pending);
        assert!(sent(&requests).is_some());
    }

    #[test]
    fn bytes_longer_than_the_contract_allows_fail_without_a_request() {
        let requests = Requests::default();
        let over = Request::Write {
            path: "/a".to_owned(),
            bytes: vec![0; 16_777_217],
        };
        let mut long = FileFuture::new(&requests, over, drop);
        assert_eq!(
            poll(&mut long),
            Poll::Ready(Err(FileError::new(
                FileErrorKind::Other,
                "the bytes are longer than 16777216 bytes"
            )))
        );
        assert_eq!(sent(&requests), None);
        let within = Request::Append {
            path: "/a".to_owned(),
            bytes: vec![0; 16_777_216],
        };
        let mut longest = FileFuture::new(&requests, within, drop);
        assert_eq!(poll(&mut longest), Poll::Pending);
        assert!(sent(&requests).is_some());
    }

    #[test]
    fn either_path_of_a_move_longer_than_the_contract_allows_fails_without_a_request() {
        let requests = Requests::default();
        let long = "a".repeat(4097);
        let too_long = Poll::Ready(Err(FileError::new(
            FileErrorKind::Other,
            "the path is longer than 4096 bytes",
        )));
        for (from, to) in [(long.as_str(), "/b"), ("/a", long.as_str())] {
            let (from, to) = (from.to_owned(), to.to_owned());
            let mut rename = FileFuture::new(&requests, Request::Rename { from, to }, drop);
            assert_eq!(poll(&mut rename), too_long);
        }
        assert_eq!(sent(&requests), None);
    }

    #[test]
    fn a_future_polled_after_it_was_ready_fails() {
        let requests = Requests::default();
        let mut future = stat(&requests, "/a");
        assert_eq!(poll(&mut future), Poll::Pending);
        assert!(sent(&requests).is_some());
        requests.answered(Ok(file_of(1)));
        assert!(poll(&mut future).is_ready());
        assert_eq!(
            poll(&mut future),
            Poll::Ready(Err(FileError::new(
                FileErrorKind::Other,
                "the file future was polled after it was ready"
            )))
        );
    }

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
            FileErrorKind::AlreadyExists,
            FileErrorKind::NotEmpty,
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
