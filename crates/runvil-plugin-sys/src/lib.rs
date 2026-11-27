//! The C contract every Plugin entry point follows, library or linked.
//! `runvil_plugin.h` declares it for C and C++, and the `runvil-plugin-sys`
//! crate mirrors it for Rust; `cargo xtask header` writes the header from
//! the crate's source, comments included, so the two never disagree.
//!
//! A Plugin is one entry point, with the shape of `runvil_plugin`. A
//! library Plugin exports it under that name; a linked Plugin has the same
//! shape under any name.
//!
//! # The contract
//!
//! - Every call passes an [`RvCall`] that names a verb. The call carries
//!   three pointers that belong to the Plugin, which the Host stores and
//!   passes back and never reads through: `plugin`, the Plugin instance;
//!   `pack_run`, one run's instance of a Pack; and `tool_call`, the state
//!   one tool call keeps between its steps.
//! - [`RvHost`] holds only what is true on every call: the contract version
//!   the Host speaks. Each function a Plugin may call back belongs to one
//!   verb and sits in that verb's arguments struct, behind `call->args`,
//!   beside the `ctx` pointer it takes first. The one exception is
//!   `wake`, which sits in the [`RvWaker`] of a tool call, beside the
//!   `ctx` of that waker, and takes that `ctx`.
//! - Strings are a pointer plus a length, UTF-8, not NUL-terminated. Each
//!   string a Plugin passes has a limit in bytes, which one of the
//!   `RV_MAX_` constants names, and a longer string is a misuse. The
//!   bytes a Plugin passes for a file have such a limit too.
//! - A struct, a string, or a run of bytes that one side passes to the
//!   other is valid only until the function it was passed to returns. The
//!   receiver copies what it keeps. The answer to a file request stays
//!   valid for the whole step that receives it, as the section on files
//!   says. What the `ctx` of a tool call's waker points at stays valid
//!   until the Plugin is unloaded, as the section on threads and lifetime
//!   says.
//! - Verb names follow `RV_<OBJECT>_<ACTION>`. Libraries open and close;
//!   Plugins load and unload.
//!
//! # Verbs
//!
//! - [`RV_PLUGIN_LOAD`] (1), with an [`RvLoadArgs`]: makes the Plugin
//!   instance and sets `call->plugin`, names the Plugin once with
//!   `describe_plugin`, and inserts each of the Plugin's Packs with
//!   `insert_pack`.
//! - [`RV_PLUGIN_UNLOAD`] (2), with no arguments: frees `call->plugin`.
//! - [`RV_PACK_NEW`] (3), with an [`RvPackNewArgs`]: makes one run's
//!   instance of a Pack and sets `call->pack_run`, and lists that
//!   instance's tools with `add_tool`.
//! - [`RV_PACK_DELETE`] (4), with no arguments: frees `call->pack_run`.
//! - [`RV_TOOL_CALL`] (5), with an [`RvToolCallArgs`]: runs one step of a
//!   tool call on `call->pack_run`.
//! - [`RV_TOOL_CANCEL`] (6), with no arguments: frees `call->tool_call`,
//!   the state of a tool call the Host has abandoned.
//! - [`RV_PACK_RENDER`] (7), with an [`RvPackRenderArgs`]: lends one run's
//!   instance of a Pack the tools a section offered from it and the model
//!   that will read them, and lists the tools the model is shown, with
//!   `add_tool`.
//!
//! A Plugin answers a verb it does not know with [`RV_ERR_UNSUPPORTED`].
//! [`RV_PLUGIN_UNLOAD`], [`RV_PACK_DELETE`], and [`RV_TOOL_CANCEL`] return
//! [`RV_OK`], and the Host ignores their status.
//!
//! # Packs
//!
//! - A Plugin inserts each of its Packs once, while it loads. The Host
//!   makes one instance of a Pack for each run that holds the Pack, with
//!   [`RV_PACK_NEW`], and frees the instance with [`RV_PACK_DELETE`] when
//!   the run ends.
//! - An instance's catalog tools are the ones it adds during
//!   [`RV_PACK_NEW`], and they stay fixed for the run. A render may list
//!   other tools for a model, as the section on renders says. The Host
//!   forms each tool's id from the
//!   Pack's id, which is the second segment of the Pack's name, and the
//!   tool's name: the tool `fetch` of the Pack `acme/url` is `url/fetch`.
//! - [`RV_PACK_NEW`] runs while the Host prepares a run, so it returns
//!   promptly: it does not block and does no I/O. What is costly to make
//!   belongs in the Plugin instance, which every run shares.
//!
//! # Renders
//!
//! - A tool a Pack lists in [`RV_PACK_NEW`] is a catalog tool: the Host's
//!   scripts call it by its id, a prompt offers it by that id, and its
//!   parameters are the same for the whole run. A tool that only a render
//!   lists is a model-facing tool: only a model calls it, and no script
//!   can name it.
//! - When a section entry's first model round is about to advertise its
//!   tools, the Host sends [`RV_PACK_RENDER`] to the instance of each Pack
//!   the section offered tools from, so that the Pack can shape what the
//!   model sees for that model. The Host sends it once for each of the
//!   section's two sets of tools that holds a tool of the Pack, so up to
//!   twice for one section entry, with the same `entry` and `model`. What
//!   the Pack lists stays fixed for that section entry.
//! - `entry` is the section entry, the value a prompt's Lua reads as
//!   `sys.id`. `model` is the JSON text of an object that describes the
//!   model of the entry's first round, with the keys `server`, `name`,
//!   `description`, `context` (the window in tokens), and `thinking`
//!   (`"never"`, `"always"`, or `"switchable"`), and `provider` when the
//!   Host's catalog names one. A model the Host's catalog does not hold
//!   reports `"never"`. A later version adds a fact as a new key, so a
//!   Plugin ignores a key it does not know.
//! - `offered` points at `offered_len` [`RvToolInfo`] values, one for each
//!   tool the section offered from this Pack in this set, in the order
//!   they were offered. Each `name` is the tool's name within its Pack.
//!   `wire_name` is empty when the Host derives the name. `description`
//!   and `parameters` are the catalog's, and `parameters` is the Host's
//!   own text of the schema, so its spacing and key order can differ from
//!   the text the Plugin gave [`RV_PACK_NEW`]. `flags` are the catalog's.
//! - The Plugin answers by calling `add_tool` once for each tool the model
//!   is shown, in the order it is shown, and returning [`RV_OK`]. A tool
//!   the Plugin leaves out is one the model is not shown. Each tool it
//!   adds is checked as [`RV_PACK_NEW`] checks a tool.
//! - A tool added under the `name` of an offered tool is that catalog
//!   tool. It may change its `wire_name` and `description`, and its
//!   `parameters` must be those the Host lent, so a catalog tool's name
//!   always means one parameter schema. A tool added under any other name
//!   is a model-facing tool, with the `flags` the Plugin gives it, such
//!   as [`RV_TOOL_CHANGES_FILES`]. A model-facing tool is the way to show
//!   a model other parameters than the catalog's: the Plugin lists it
//!   under a name of its own and answers its calls by that name. A name
//!   that is a catalog tool of the run which the section did not offer in
//!   this set is not free for a model-facing tool: the Host fails the
//!   round that asked for the render.
//! - A Plugin that does not render answers [`RV_ERR_UNSUPPORTED`], and the
//!   Host shows the model the offered tools as they are. A Plugin that
//!   cannot render declines with `refuse`, which returns
//!   [`RV_ERR_REFUSED`], at most once, with a UTF-8 reason of at most
//!   [`RV_MAX_LINE`] bytes. A refusal, a misuse, and any other status
//!   fail the render, and the round that asked for it fails with it.
//! - [`RV_PACK_RENDER`] returns promptly: it does not block and does no
//!   I/O. It keeps nothing: `entry`, `model`, and `offered` are valid for
//!   the call only, every tool it lists is copied by the Host as it is
//!   added, and there is nothing for the Plugin to free.
//!
//! # Tool calls
//!
//! - A tool call is a series of steps. The Host starts the call by sending
//!   [`RV_TOOL_CALL`] with `call->tool_call` NULL, and that step may set
//!   `call->tool_call` to state of its own. Each later step is an
//!   [`RV_TOOL_CALL`] with the same `tool`, `arguments`, `resources`, and
//!   `waker`, and with the `call->tool_call` the last step left.
//! - A step ends the call by calling `finish` once and returning
//!   [`RV_OK`]. Any return other than [`RV_PENDING`] ends the call, and a
//!   call that ends without a result fails.
//! - A step that returns [`RV_PENDING`] parks the call. The Host sends a
//!   parked call its next step after the Plugin wakes the call, which it
//!   does by calling `wake` in the call's [`RvWaker`] with that waker's
//!   `ctx`. A wake during a step counts, so a step that wants the next
//!   one right away calls `wake` and then returns [`RV_PENDING`]. Wakes
//!   do not add up: a wake asks for the next step that begins after it,
//!   however many wakes come before that step.
//! - After a step that returns [`RV_PENDING`] and made no file request,
//!   the Host owes the call no step until a wake. So a Plugin wakes the
//!   call before such a step returns, or has one of its threads wake the
//!   call later. A call that nothing wakes can stay parked until the Host
//!   abandons it.
//! - A step that made a file request waits for its answer by returning
//!   [`RV_PENDING`], as the section on files says. The answer brings the
//!   call's next step, so such a step needs no wake, and a wake does not
//!   bring that step sooner.
//! - The Host may also send a step that no wake asked for, but never
//!   while a file request waits for its answer. A step that finds nothing
//!   new to do returns [`RV_PENDING`] again.
//! - The Host may abandon a call between two steps, for example when its
//!   run is canceled. It then sends [`RV_TOOL_CANCEL`] once, with the
//!   `call->tool_call` the last step left, and no further step.
//! - A step does a bounded slice of work and returns. A step that works
//!   for long, or blocks, stalls its run, and a run can be canceled only
//!   between steps. A call that waits on something outside its run, such
//!   as a reply from the network or a timer, blocks no step for it: its
//!   step returns [`RV_PENDING`], and a thread of the Plugin wakes the
//!   call when the wait is over.
//! - Every step is lent the Host's resources as they were when the call
//!   started: `resources` in its [`RvToolCallArgs`] is the JSON text of
//!   an object with one key for each resource the Host provides, such as
//!   `runvil/web-search`, and `{}` when it provides none. What a resource
//!   holds is between the Host and the Plugins that read it.
//! - A tool whose calls change files sets [`RV_TOOL_CHANGES_FILES`] in the
//!   `flags` of its [`RvToolInfo`]. Of the calls that one reply of a model
//!   asks for, the Host runs a call of such a tool by itself: the call
//!   starts when every call the reply lists before it has ended, and the
//!   calls the reply lists after it start when it has ended. So two
//!   changes of one reply happen in the order the reply lists them. The
//!   calls of tools without the flag may run at the same time.
//!
//! # Files
//!
//! - A tool call reaches its run's files only through the nine file
//!   functions in its [`RvToolCallArgs`], so the run's policy applies to
//!   every operation. `file_stat`, `file_list`, and `file_read` read.
//!   `file_write`, `file_append`, `file_mkdir`, `file_remove`,
//!   `file_rename`, and `file_copy` change the files.
//! - A file function only takes a request. It returns [`RV_OK`] when the
//!   Host took the request and [`RV_ERR_MISUSE`] for a misuse, and it
//!   fills nothing. The call's next step receives the answer: `answer` in
//!   that step's [`RvToolCallArgs`] points to an [`RvFileAnswer`]. In a
//!   step that follows no request, `answer` is NULL.
//! - A call has one request outstanding at a time. A second request in
//!   one step is a misuse, and so is a request in a step that calls
//!   `finish`.
//! - A step that made a request returns [`RV_PENDING`]. The Host sends
//!   that call its next step when the answer is ready, and no step before
//!   then, even when the Plugin wakes the call.
//! - An answer's `status` is [`RV_OK`] or [`RV_ERR_FILE`]. With [`RV_OK`]
//!   the answer holds what the request asked for, in the one field its
//!   file function names. A function that changes the files names no
//!   field, so its answer holds a status alone. With [`RV_ERR_FILE`] the
//!   answer's `error` says what went wrong. Every other field of the
//!   answer is zero. An operation that fails does not fail the tool call:
//!   the tool decides what its result says.
//! - The answer, and what it points to, is valid for the step that
//!   receives it: the bytes, the entries and the names in them, and an
//!   error's text. All of it belongs to the Host. The Plugin copies what
//!   it keeps and frees none of it.
//! - The Host holds an answer in memory, so a Plugin reads a large file in
//!   ranges. `file_write` and `file_append` take at most [`RV_MAX_WRITE`]
//!   bytes, so a Plugin writes a longer file as one `file_write` and then
//!   one `file_append` for each further part.
//! - `file_write`, `file_append`, `file_rename`, and `file_copy` make each
//!   directory that is absent on the way to the path they write, and
//!   `file_mkdir` does when its `recursive` is 1.
//! - When the Host abandons a call, an operation it had taken either
//!   happened in full or did not happen.
//! - A path names a file of the run, with `/` between its segments, and is
//!   at most [`RV_MAX_PATH`] bytes long. Every path is taken from the run's
//!   root, whether or not it begins with `/`. An empty segment and a `.`
//!   segment change nothing, a `..` segment steps up one directory, and a
//!   `\` is read as `/`. An empty path, and a path that steps above the
//!   root, fail as file errors.
//! - The answer to `file_stat`, and each entry in the answer to
//!   `file_list`, describe a link and not what it leads to. `file_mkdir`,
//!   `file_remove`, and `file_rename` act on a link itself. Every other
//!   file function acts on what a link leads to.
//!
//! # Threads and lifetime
//!
//! - [`RV_PLUGIN_LOAD`] runs on the thread on which the Host loads the
//!   Plugin. Every other verb may come on any thread, and the steps of one
//!   tool call may come on different threads.
//! - Calls may run at the same time on different threads, with three
//!   exceptions. No two steps of one tool call run at the same time, and
//!   [`RV_TOOL_CANCEL`] runs beside no step of its call.
//!   [`RV_PACK_DELETE`] comes after every tool call and every render of
//!   its `pack_run` has ended, and runs beside no call that names it. So
//!   [`RV_PACK_RENDER`] may run beside tool calls of its `pack_run` and
//!   beside another render of it.
//!   [`RV_PLUGIN_UNLOAD`] comes after every Pack instance of the Plugin is
//!   deleted, and runs beside no other call.
//! - The functions in a verb's arguments struct answer only while that
//!   call runs, only on its thread, and only for its `ctx`. Any other use
//!   returns [`RV_ERR_MISUSE`], and so does a function called against its
//!   own rules, such as `describe_plugin` called twice. A misuse on the
//!   call's own thread fails that call, whatever status the Plugin
//!   returns. A function called on another thread while its call runs
//!   returns [`RV_ERR_MISUSE`] and has no other effect.
//! - `wake` is the one function those rules do not bind. The Plugin may
//!   call the `wake` of a tool call's [`RvWaker`], with that waker's
//!   `ctx`, on any thread and any number of times, from the call's first
//!   step until [`RV_PLUGIN_UNLOAD`] returns: during a step, between two
//!   steps, during [`RV_TOOL_CANCEL`], and after the call has ended. It
//!   returns nothing. It only notes the wake and returns: it calls
//!   nothing of the Plugin's and does not block, so a Plugin may call it
//!   while it holds a lock.
//! - The Host keeps the `ctx` of a load valid while the Plugin is loaded,
//!   until [`RV_PLUGIN_UNLOAD`] returns, so a late call through it is
//!   answered [`RV_ERR_MISUSE`] instead of reaching freed memory. It
//!   keeps what the `ctx` of every waker points at valid until then too,
//!   so a late wake reaches no freed memory either: a wake after its call
//!   has ended does nothing for that call. The `ctx` of a load the Host
//!   rejects, and the `ctx` of every other call, is gone once the call
//!   returns, and the Plugin must not use it.
//! - [`RV_PLUGIN_UNLOAD`] must stop every thread the Plugin started and
//!   release everything it holds before it returns. A library Plugin's
//!   library closes right after, and a thread still running crashes the
//!   process. Every waker is gone once it returns, so the Plugin calls no
//!   `wake` after that.
//!
//! # What the Plugin frees
//!
//! - A load the Host rejects gets no [`RV_PLUGIN_UNLOAD`]: a refusal, a
//!   misuse, a status other than [`RV_OK`], or no description. The Plugin
//!   frees whatever it made before it returns on every such path. A load
//!   that succeeds is unloaded later, even if the Host then drops the
//!   Plugin, for example because its name repeats one already loaded.
//! - An [`RV_PACK_NEW`] the Host rejects gets no [`RV_PACK_DELETE`]: a
//!   refusal, a misuse, or a status other than [`RV_OK`]. The Plugin frees
//!   whatever it made before it returns on every such path. One that
//!   succeeds gets exactly one [`RV_PACK_DELETE`].
//! - An [`RV_PACK_RENDER`] leaves the instance as it was, whether it
//!   succeeds, is refused, or is rejected, so it gets no later verb. The
//!   Plugin frees whatever it made during the call before it returns.
//! - The step that ends a tool call frees the call's state before it
//!   returns. A call that is abandoned gets [`RV_TOOL_CANCEL`] instead,
//!   which frees the state; its `call->tool_call` is NULL when no step set
//!   it. Each call gets one or the other, never both.
//! - A waker belongs to the Host, with what its `ctx` points at. The
//!   Plugin copies the [`RvWaker`] it keeps and frees nothing of it.
//!
//! # Reserved numbers
//!
//! Verb 8 and status codes 1 to 3 are reserved for later versions, so
//! those numbers never get another meaning. This version declares no
//! constant for them, sends no such verb, and returns none of those codes.
//!
//! # Status codes
//!
//! - [`RV_OK`] (0) is success.
//! - Codes 1 to 3 are reserved.
//! - [`RV_ERR_MISUSE`] (4) means the caller broke the contract.
//! - [`RV_ERR_REFUSED`] (5) means the Plugin declined: to load, to make a
//!   Pack instance, or to render a Pack's tools.
//! - [`RV_ERR_UNSUPPORTED`] (6) means the Plugin cannot answer the call: it
//!   does not know the verb, does not render for [`RV_PACK_RENDER`], the
//!   Host reports a version the Plugin was not built for, or a struct's
//!   `size` is smaller than the Plugin expects.
//! - [`RV_PENDING`] (7) means a tool call is parked: the Host sends its
//!   next step after the Plugin wakes the call, or when the answer to the
//!   step's file request is ready. Only [`RV_TOOL_CALL`] returns it.
//! - [`RV_ERR_FILE`] (8) means a file operation failed. No function
//!   returns it: it is the `status` of an [`RvFileAnswer`], whose `error`
//!   says what went wrong.
//!
//! # Versions
//!
//! - [`RV_API_MAJOR`] and [`RV_API_MINOR`] name the contract's version, and
//!   the `runvil-plugin-sys` crate's version is the same number.
//! - A 0.x version makes no compatibility promise. Each one is a contract
//!   of its own, and a Plugin built for one loads only in a Host whose
//!   [`RvHost`] reports exactly that version. The Plugin checks the
//!   version first in [`RV_PLUGIN_LOAD`], before it reads `call->args`,
//!   and answers a version it was not built for with
//!   [`RV_ERR_UNSUPPORTED`].
//! - From 1.0, a minor version only adds: a field at the end of a struct
//!   that has a `size` field, which shows whether the field is there, such
//!   as a function at the end of a verb's arguments struct; a verb with an
//!   arguments struct of its own; or a number for a result kind, a file
//!   type, a file error kind, or a tool flag. A struct with no `size`
//!   field never changes. A Plugin built for 1.n loads in a Host that
//!   reports major version 1 and a minor version of at least n.
//!
//! ## Invariants
//!
//! These are about the Rust crate, so the header's comment stops before
//! them.
//!
//! - The shape of every entry point is [`PluginEntry`] in Rust. Declaring
//!   the types is safe Rust; only calling an entry point or reading
//!   through its pointers needs `unsafe`.
//! - No dependencies and no `unsafe`: this crate only declares.
//! - `include/runvil_plugin.h` is generated from this file and never
//!   edited by hand; `cargo test -p build-xtask` fails when it is stale.
//! - Status codes, verbs, result kinds, file types, file error kinds, and
//!   tool flags keep their numbers in every version; the unit tests pin
//!   them.

#![cfg_attr(not(test), no_std)]

use core::ffi::{c_char, c_void};

/// What every function in the contract returns, except `wake`, which
/// returns nothing, and the `status` of an [`RvFileAnswer`]: zero for
/// success.
pub type RvStatus = u32;
/// Success.
pub const RV_OK: RvStatus = 0;
/// The caller broke the contract.
pub const RV_ERR_MISUSE: RvStatus = 4;
/// The Plugin declined to load, to make a Pack instance, or to render a
/// Pack's tools; `refuse` returns it.
pub const RV_ERR_REFUSED: RvStatus = 5;
/// The Plugin cannot answer the call: it does not know the verb, it was
/// not built for the Host's version, or a struct is shorter than it
/// expects.
pub const RV_ERR_UNSUPPORTED: RvStatus = 6;
/// The tool call is parked. The Host sends its next step after the
/// Plugin wakes the call, or, when the step made a file request, when
/// the answer is ready. A wake during the step counts, so a step that
/// wants the next one right away calls `wake` and then returns this. A
/// step may also come that no wake asked for, but never while a file
/// request waits for its answer. Only [`RV_TOOL_CALL`] returns it.
pub const RV_PENDING: RvStatus = 7;
/// A file operation failed. No function returns it: it is the `status` of
/// an [`RvFileAnswer`], whose `error` says what went wrong.
pub const RV_ERR_FILE: RvStatus = 8;

/// The operation an [`RvCall`] asks for. A fixed-width integer, not a C
/// enum, so its size never depends on the compiler.
pub type RvVerb = u32;
/// Make the Plugin instance, name the Plugin, and insert its Packs.
pub const RV_PLUGIN_LOAD: RvVerb = 1;
/// Free the Plugin instance.
pub const RV_PLUGIN_UNLOAD: RvVerb = 2;
/// Make one run's instance of a Pack and list its tools.
pub const RV_PACK_NEW: RvVerb = 3;
/// Free one run's instance of a Pack.
pub const RV_PACK_DELETE: RvVerb = 4;
/// Run one step of a tool call.
pub const RV_TOOL_CALL: RvVerb = 5;
/// Free the state of a tool call the Host has abandoned.
pub const RV_TOOL_CANCEL: RvVerb = 6;
/// Lend a Pack instance the tools a section offered from it and the model
/// that will read them, and list the tools the model is shown.
pub const RV_PACK_RENDER: RvVerb = 7;

/// How a tool call ended, in an [`RvToolResult`]. A fixed-width integer,
/// as [`RvVerb`] is.
pub type RvResultKind = u32;
/// The call succeeded, and its text may hold data an attacker can
/// influence, such as the contents of a file or a web page. The Host marks
/// such text before a model reads it. Zero, so a result that names no kind
/// is treated with care.
pub const RV_RESULT_UNTRUSTED: RvResultKind = 0;
/// The call succeeded, and the Plugin vouches for every byte of its text.
pub const RV_RESULT_TRUSTED: RvResultKind = 1;
/// The call failed: its arguments do not match the tool's parameters.
pub const RV_RESULT_INVALID_ARGUMENTS: RvResultKind = 2;
/// The call failed: what the tool reaches refused or failed the request.
pub const RV_RESULT_BACKEND: RvResultKind = 3;
/// The call failed: the request never got through.
pub const RV_RESULT_TRANSPORT: RvResultKind = 4;
/// The call failed for any other reason.
pub const RV_RESULT_OTHER: RvResultKind = 5;

/// What a path names, in an [`RvStat`]. A fixed-width integer, as
/// [`RvVerb`] is.
pub type RvFileType = u32;
/// Something that is none of the kinds below, such as a pipe, a socket, or
/// a device.
pub const RV_FILE_TYPE_OTHER: RvFileType = 0;
/// A regular file.
pub const RV_FILE_TYPE_FILE: RvFileType = 1;
/// A directory.
pub const RV_FILE_TYPE_DIRECTORY: RvFileType = 2;
/// A symbolic link.
pub const RV_FILE_TYPE_SYMLINK: RvFileType = 3;

/// Why a file operation failed, in an [`RvFileError`]. A fixed-width
/// integer, as [`RvVerb`] is.
pub type RvFileErrorKind = u32;
/// A reason the kinds below do not name.
pub const RV_FILE_ERROR_OTHER: RvFileErrorKind = 0;
/// Nothing is at the path.
pub const RV_FILE_ERROR_NOT_FOUND: RvFileErrorKind = 1;
/// The path names something that is not a directory, where the operation
/// needs one.
pub const RV_FILE_ERROR_NOT_A_DIRECTORY: RvFileErrorKind = 2;
/// The path names a directory, where the operation needs a file.
pub const RV_FILE_ERROR_IS_A_DIRECTORY: RvFileErrorKind = 3;
/// The run's policy does not allow the operation. The error's text is the
/// policy's own reason.
pub const RV_FILE_ERROR_DENIED: RvFileErrorKind = 4;
/// Another tool call of the run changed the path, or is changing it, at
/// the same time. The same operation can succeed in a later call.
pub const RV_FILE_ERROR_CONFLICT: RvFileErrorKind = 5;
/// The operating system failed the operation.
pub const RV_FILE_ERROR_IO: RvFileErrorKind = 6;
/// Something is at the path already, where the operation needs nothing
/// there.
pub const RV_FILE_ERROR_ALREADY_EXISTS: RvFileErrorKind = 7;
/// The path names a directory that has entries, where the operation
/// needs an empty one.
pub const RV_FILE_ERROR_NOT_EMPTY: RvFileErrorKind = 8;

/// What a tool does beyond answering its calls, in the `flags` of an
/// [`RvToolInfo`]: one bit for each `RV_TOOL_` constant. A fixed-width
/// integer, as [`RvVerb`] is.
pub type RvToolFlags = u32;
/// A call of the tool changes files of its run. Bit 0.
pub const RV_TOOL_CHANGES_FILES: RvToolFlags = 1;

/// The contract's major version, which [`RvHost`] reports.
pub const RV_API_MAJOR: u32 = 0;
/// The contract's minor version, which [`RvHost`] reports.
pub const RV_API_MINOR: u32 = 7;

/// The most bytes in a name segment, a version, or a wire name.
pub const RV_MAX_NAME: usize = 64;
/// The most bytes in a refusal reason or a Pack's description.
pub const RV_MAX_LINE: usize = 1024;
/// The most bytes in a path a file function takes.
pub const RV_MAX_PATH: usize = 4096;
/// The most bytes in a tool's description, and in its parameters.
pub const RV_MAX_TOOL_TEXT: usize = 65536;
/// The most bytes in the text of a tool call's result.
pub const RV_MAX_RESULT: usize = 16_777_216;
/// The most bytes `file_write` or `file_append` takes in one request.
pub const RV_MAX_WRITE: usize = 16_777_216;

/// A UTF-8 string that is not NUL-terminated.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RvStr {
    /// The first byte. It may be NULL when `len` is zero.
    pub ptr: *const c_char,
    /// The length in bytes.
    pub len: usize,
}

/// A run of bytes: what `file_write` and `file_append` take, and what the
/// answer to `file_read` holds.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RvBytes {
    /// The first byte. It may be NULL when `len` is zero.
    pub ptr: *const u8,
    /// The length in bytes.
    pub len: usize,
}

/// What a path names, which the answer to `file_stat` and each
/// [`RvDirEntry`] hold.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RvStat {
    /// One of the `RV_FILE_TYPE_` constants.
    pub file_type: RvFileType,
    /// Zero.
    pub reserved: u32,
    /// The length of a file in bytes. For anything but a file the number
    /// means nothing.
    pub len: u64,
}

/// One entry of a directory, in an [`RvDirEntries`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RvDirEntry {
    /// The entry's name within its directory.
    pub name: RvStr,
    /// What the entry is.
    pub stat: RvStat,
}

/// The entries of a directory, in no promised order, which the answer to
/// `file_list` holds.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RvDirEntries {
    /// The first entry. It may be NULL when `len` is zero.
    pub ptr: *const RvDirEntry,
    /// How many entries there are.
    pub len: usize,
}

/// Why a file operation failed, in an [`RvFileAnswer`] whose `status` is
/// [`RV_ERR_FILE`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RvFileError {
    /// One of the `RV_FILE_ERROR_` constants.
    pub kind: RvFileErrorKind,
    /// Zero.
    pub reserved: u32,
    /// What went wrong, in words a model can read.
    pub text: RvStr,
}

/// The answer to a file request, which the step after the request
/// receives through `answer` in its [`RvToolCallArgs`]. The Host fills
/// the one field the request's file function names, or `error`, and
/// leaves every other field zero.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RvFileAnswer {
    /// The size of this struct in bytes: `sizeof(RvFileAnswer)`.
    pub size: u32,
    /// [`RV_OK`] when the operation succeeded, or [`RV_ERR_FILE`] with
    /// `error` filled.
    pub status: RvStatus,
    /// What the path names, in the answer to `file_stat`.
    pub stat: RvStat,
    /// The entries of the directory, in the answer to `file_list`.
    pub entries: RvDirEntries,
    /// The bytes read, in the answer to `file_read`.
    pub bytes: RvBytes,
    /// Why the operation failed, when `status` is [`RV_ERR_FILE`].
    pub error: RvFileError,
}

/// What a Plugin tells `describe_plugin` about itself.
#[repr(C)]
#[derive(Debug)]
pub struct RvPluginInfo {
    /// The size of this struct in bytes: `sizeof(RvPluginInfo)`.
    pub size: u32,
    /// Zero.
    pub reserved: u32,
    /// One name segment, such as `acme-url`: 1 to [`RV_MAX_NAME`] bytes of
    /// lowercase ASCII letters, digits, `-`, `_`, and `.`.
    pub name: RvStr,
    /// Non-empty, at most [`RV_MAX_NAME`] bytes, with no control characters.
    pub version: RvStr,
}

/// What a Plugin tells `insert_pack` about one of its Packs.
#[repr(C)]
#[derive(Debug)]
pub struct RvPackInfo {
    /// The size of this struct in bytes: `sizeof(RvPackInfo)`.
    pub size: u32,
    /// Zero.
    pub reserved: u32,
    /// The Pack's name: a vendor and a Pack id joined by `/`, such as
    /// `acme/url`. Each is one name segment, as a Plugin's name is.
    pub name: RvStr,
    /// One line saying what the Pack offers: at most [`RV_MAX_LINE`] bytes
    /// with no control characters. It may be empty.
    pub description: RvStr,
}

/// What a Pack instance tells `add_tool` about one of its tools.
#[repr(C)]
#[derive(Debug)]
pub struct RvToolInfo {
    /// The size of this struct in bytes: `sizeof(RvToolInfo)`.
    pub size: u32,
    /// Zero, or [`RV_TOOL_CHANGES_FILES`]. A set bit that is no `RV_TOOL_`
    /// constant is a misuse.
    pub flags: RvToolFlags,
    /// The tool's name within its Pack: one name segment, such as `fetch`.
    pub name: RvStr,
    /// The name a model calls the tool by: 1 to [`RV_MAX_NAME`] bytes of
    /// ASCII letters, digits, `_`, and `-`. Empty lets the Host derive it
    /// from the tool's id, as `url_fetch` from `url/fetch`.
    pub wire_name: RvStr,
    /// What the tool does, for a model to read: non-empty, at most
    /// [`RV_MAX_TOOL_TEXT`] bytes.
    pub description: RvStr,
    /// The tool's parameters: the JSON text of a JSON Schema whose value is
    /// an object, at most [`RV_MAX_TOOL_TEXT`] bytes.
    pub parameters: RvStr,
}

/// How a tool call ended, which a step tells `finish`.
#[repr(C)]
#[derive(Debug)]
pub struct RvToolResult {
    /// The size of this struct in bytes: `sizeof(RvToolResult)`.
    pub size: u32,
    /// Whether the call succeeded, and if so how far its text is trusted,
    /// or else why it failed: one of the `RV_RESULT_` constants.
    pub kind: RvResultKind,
    /// The call's output when it succeeded, and otherwise a message a model
    /// can read that says what went wrong: at most [`RV_MAX_RESULT`] bytes.
    pub text: RvStr,
}

/// What the Host reports on every call: the contract version it speaks.
/// The functions a Plugin may call back sit in each verb's arguments
/// struct instead.
#[repr(C)]
#[derive(Debug)]
pub struct RvHost {
    /// The size of this struct in bytes: `sizeof(RvHost)`.
    pub size: u32,
    /// [`RV_API_MAJOR`].
    pub api_major: u32,
    /// [`RV_API_MINOR`].
    pub api_minor: u32,
    /// Zero.
    pub reserved: u32,
}

/// The arguments of [`RV_PLUGIN_LOAD`]: the functions the Plugin may call
/// while it loads, each taking `ctx` first.
#[repr(C)]
#[derive(Debug)]
pub struct RvLoadArgs {
    /// The size of this struct in bytes: `sizeof(RvLoadArgs)`.
    pub size: u32,
    /// Zero.
    pub reserved: u32,
    /// The Host's context for this load, passed back as the first argument
    /// of each function below. The Plugin never reads through it.
    pub ctx: *mut c_void,
    /// Declines the load, at most once, with a UTF-8 reason of at most
    /// [`RV_MAX_LINE`] bytes; returns [`RV_ERR_REFUSED`].
    pub refuse: unsafe extern "C" fn(ctx: *mut c_void, reason: RvStr) -> RvStatus,
    /// Names the Plugin, exactly once.
    pub describe_plugin:
        unsafe extern "C" fn(ctx: *mut c_void, info: *const RvPluginInfo) -> RvStatus,
    /// Inserts one Pack, once for each Pack the Plugin offers. No two
    /// Packs of one Plugin share a name.
    pub insert_pack: unsafe extern "C" fn(ctx: *mut c_void, info: *const RvPackInfo) -> RvStatus,
}

/// The arguments of [`RV_PACK_NEW`]: the Pack to make an instance of, and
/// the functions the Plugin may call while it makes one, each taking `ctx`
/// first.
#[repr(C)]
#[derive(Debug)]
pub struct RvPackNewArgs {
    /// The size of this struct in bytes: `sizeof(RvPackNewArgs)`.
    pub size: u32,
    /// Zero.
    pub reserved: u32,
    /// The Host's context for this call, passed back as the first argument
    /// of each function below. The Plugin never reads through it.
    pub ctx: *mut c_void,
    /// The name of the Pack, as the Plugin gave it to `insert_pack`.
    pub pack: RvStr,
    /// Declines to make the instance, at most once, with a UTF-8 reason of
    /// at most [`RV_MAX_LINE`] bytes that a model can read; returns
    /// [`RV_ERR_REFUSED`]. The run then goes without the Pack.
    pub refuse: unsafe extern "C" fn(ctx: *mut c_void, reason: RvStr) -> RvStatus,
    /// Adds one tool to the instance, once for each tool it offers. No two
    /// tools of one instance share a name.
    pub add_tool: unsafe extern "C" fn(ctx: *mut c_void, info: *const RvToolInfo) -> RvStatus,
}

/// The arguments of [`RV_PACK_RENDER`]: the section entry, the model, the
/// tools the section offered from the Pack, and the functions the Plugin
/// may call while it renders, each taking `ctx` first.
#[repr(C)]
#[derive(Debug)]
pub struct RvPackRenderArgs {
    /// The size of this struct in bytes: `sizeof(RvPackRenderArgs)`.
    pub size: u32,
    /// Zero.
    pub reserved: u32,
    /// The Host's context for this call, passed back as the first argument
    /// of each function below. The Plugin never reads through it.
    pub ctx: *mut c_void,
    /// The section entry, the value a prompt's Lua reads as `sys.id`.
    pub entry: RvStr,
    /// The JSON text of an object that describes the model of the entry's
    /// first round: `server`, `name`, `description`, `context`, `thinking`,
    /// and `provider` when the Host's catalog names one.
    pub model: RvStr,
    /// The first of `offered_len` tools the section offered from this Pack
    /// in this set, in the order they were offered. It may be NULL when
    /// `offered_len` is zero.
    pub offered: *const RvToolInfo,
    /// How many tools `offered` points at.
    pub offered_len: usize,
    /// Declines to render, at most once, with a UTF-8 reason of at most
    /// [`RV_MAX_LINE`] bytes that a model can read; returns
    /// [`RV_ERR_REFUSED`]. The round that asked for the render fails.
    pub refuse: unsafe extern "C" fn(ctx: *mut c_void, reason: RvStr) -> RvStatus,
    /// Adds one tool the model is shown, once for each, in the order the
    /// model sees them. No two tools of one render share a name.
    pub add_tool: unsafe extern "C" fn(ctx: *mut c_void, info: *const RvToolInfo) -> RvStatus,
}

/// Wakes one tool call. The Plugin may call `wake` from any thread, any
/// number of times, from the call's first step until
/// [`RV_PLUGIN_UNLOAD`] returns. The Host keeps what `ctx` points at
/// valid until then. A wake after its call has ended does nothing for
/// that call.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RvWaker {
    /// The Host's context for the waker, passed back to `wake`. The
    /// Plugin never reads through it.
    pub ctx: *mut c_void,
    /// Asks the Host to send the call's next step. It only notes the
    /// wake and returns: it calls nothing of the Plugin's and does not
    /// block, so a Plugin may call it while it holds a lock.
    pub wake: unsafe extern "C" fn(ctx: *mut c_void),
}

/// The arguments of [`RV_TOOL_CALL`]: the tool, its arguments, the
/// function that ends the call, the file functions, the answer to the
/// file request of the call's last step, the Host's resources, and the
/// call's waker. Every step of one call gets the same `tool`,
/// `arguments`, `resources`, and `waker`.
#[repr(C)]
#[derive(Debug)]
pub struct RvToolCallArgs {
    /// The size of this struct in bytes: `sizeof(RvToolCallArgs)`.
    pub size: u32,
    /// Zero.
    pub reserved: u32,
    /// The Host's context for this step, passed back as the first argument
    /// of each function below. The Plugin never reads through it.
    pub ctx: *mut c_void,
    /// The tool's name, as the instance gave it to `add_tool`.
    pub tool: RvStr,
    /// The call's arguments: the JSON text of the value the caller passed.
    pub arguments: RvStr,
    /// Ends the call with its result, exactly once, in the step that then
    /// returns [`RV_OK`].
    pub finish: unsafe extern "C" fn(ctx: *mut c_void, result: *const RvToolResult) -> RvStatus,
    /// Asks what `path` names. The answer holds `stat`. A path longer
    /// than [`RV_MAX_PATH`] bytes is a misuse, here and in every file
    /// function below.
    pub file_stat: unsafe extern "C" fn(ctx: *mut c_void, path: RvStr) -> RvStatus,
    /// Asks for the entries of the directory at `path`. The answer holds
    /// `entries`.
    pub file_list: unsafe extern "C" fn(ctx: *mut c_void, path: RvStr) -> RvStatus,
    /// Asks for up to `len` bytes of the file at `path`, starting at the
    /// byte `offset`. The answer holds `bytes`, with fewer than `len`
    /// bytes when the file ends first, and none when `offset` is at or
    /// past its end.
    pub file_read:
        unsafe extern "C" fn(ctx: *mut c_void, path: RvStr, offset: u64, len: u64) -> RvStatus,
    /// The answer to the file request the call's last step made, or NULL
    /// when that step made none.
    pub answer: *const RvFileAnswer,
    /// Asks to write `bytes` as the whole contents of the file at `path`.
    /// The file is made when it is absent, and its contents give way to
    /// `bytes` when it is there. More than [`RV_MAX_WRITE`] bytes are a
    /// misuse, here and in `file_append`.
    pub file_write: unsafe extern "C" fn(ctx: *mut c_void, path: RvStr, bytes: RvBytes) -> RvStatus,
    /// Asks to add `bytes` to the end of the file at `path`. The file is
    /// made when it is absent.
    pub file_append:
        unsafe extern "C" fn(ctx: *mut c_void, path: RvStr, bytes: RvBytes) -> RvStatus,
    /// Asks to make a directory at `path`. With `recursive` 0 every
    /// directory on the way to `path` must be there, and with `recursive`
    /// 1 each one that is absent is made. Any other `recursive` is a
    /// misuse, here and in `file_remove`. Something already at `path`
    /// fails the operation as [`RV_FILE_ERROR_ALREADY_EXISTS`].
    pub file_mkdir: unsafe extern "C" fn(ctx: *mut c_void, path: RvStr, recursive: u32) -> RvStatus,
    /// Asks to remove the file or the directory at `path`. With
    /// `recursive` 0 a directory that has entries fails the operation as
    /// [`RV_FILE_ERROR_NOT_EMPTY`], and with `recursive` 1 it goes with
    /// everything in it. Nothing at `path` fails the operation as
    /// [`RV_FILE_ERROR_NOT_FOUND`].
    pub file_remove:
        unsafe extern "C" fn(ctx: *mut c_void, path: RvStr, recursive: u32) -> RvStatus,
    /// Asks to move the file or the directory at `from` to `to`. A
    /// directory moves with everything in it.
    pub file_rename: unsafe extern "C" fn(ctx: *mut c_void, from: RvStr, to: RvStr) -> RvStatus,
    /// Asks to copy the file at `from` to `to`. The file at `to` is made
    /// when it is absent, and its contents give way to the copy when it
    /// is there.
    pub file_copy: unsafe extern "C" fn(ctx: *mut c_void, from: RvStr, to: RvStr) -> RvStatus,
    /// The Host's resources as they were when the call started: the JSON
    /// text of an object with one key for each resource the Host
    /// provides, such as `runvil/web-search`, and `{}` when it provides
    /// none. Every step of one call gets the same text. Like `tool` and
    /// `arguments`, it is valid for the step.
    pub resources: RvStr,
    /// The waker of this call. Every step of one call gets the same one.
    /// Its `wake` takes the waker's own `ctx`, not the `ctx` of the step.
    pub waker: RvWaker,
}

/// One call into a Plugin's entry point.
#[repr(C)]
#[derive(Debug)]
pub struct RvCall {
    /// The size of this struct in bytes: `sizeof(RvCall)`.
    pub size: u32,
    /// The operation.
    pub verb: RvVerb,
    /// What the Host reports on every call.
    pub host: *const RvHost,
    /// The Plugin instance: NULL going into [`RV_PLUGIN_LOAD`], which sets
    /// it, and that pointer on every later call.
    pub plugin: *mut c_void,
    /// One run's instance of a Pack: NULL going into [`RV_PACK_NEW`],
    /// which sets it, that pointer on [`RV_PACK_DELETE`],
    /// [`RV_PACK_RENDER`], [`RV_TOOL_CALL`], and [`RV_TOOL_CANCEL`], and
    /// NULL otherwise.
    pub pack_run: *mut c_void,
    /// The verb's own arguments: an [`RvLoadArgs`] for [`RV_PLUGIN_LOAD`],
    /// an [`RvPackNewArgs`] for [`RV_PACK_NEW`], an [`RvPackRenderArgs`]
    /// for [`RV_PACK_RENDER`], an [`RvToolCallArgs`] for [`RV_TOOL_CALL`],
    /// and NULL for every other verb.
    pub args: *const c_void,
    /// The state of one tool call: NULL going into the call's first
    /// [`RV_TOOL_CALL`], what the last step left on each later step and on
    /// [`RV_TOOL_CANCEL`], and NULL otherwise.
    pub tool_call: *mut c_void,
}

/// The shape of every Plugin entry point. The header declares the one a
/// library Plugin exports, `runvil_plugin`, instead of this type.
///
/// The entry point must not unwind: a Rust panic across `extern "C"`
/// aborts the process.
pub type PluginEntry = unsafe extern "C" fn(*mut RvCall) -> RvStatus;

#[cfg(test)]
mod tests {
    //! The contract's layout on 64-bit targets, the numbers no version may
    //! change, and the crate version that names the contract's version.

    #[cfg(target_pointer_width = "64")]
    use core::mem::{offset_of, size_of};

    use super::{
        RV_API_MAJOR, RV_API_MINOR, RV_ERR_FILE, RV_ERR_MISUSE, RV_ERR_REFUSED, RV_ERR_UNSUPPORTED,
        RV_FILE_ERROR_ALREADY_EXISTS, RV_FILE_ERROR_CONFLICT, RV_FILE_ERROR_DENIED,
        RV_FILE_ERROR_IO, RV_FILE_ERROR_IS_A_DIRECTORY, RV_FILE_ERROR_NOT_A_DIRECTORY,
        RV_FILE_ERROR_NOT_EMPTY, RV_FILE_ERROR_NOT_FOUND, RV_FILE_ERROR_OTHER,
        RV_FILE_TYPE_DIRECTORY, RV_FILE_TYPE_FILE, RV_FILE_TYPE_OTHER, RV_FILE_TYPE_SYMLINK,
        RV_MAX_LINE, RV_MAX_NAME, RV_MAX_PATH, RV_MAX_RESULT, RV_MAX_TOOL_TEXT, RV_MAX_WRITE,
        RV_OK, RV_PACK_DELETE, RV_PACK_NEW, RV_PACK_RENDER, RV_PENDING, RV_PLUGIN_LOAD,
        RV_PLUGIN_UNLOAD, RV_RESULT_BACKEND, RV_RESULT_INVALID_ARGUMENTS, RV_RESULT_OTHER,
        RV_RESULT_TRANSPORT, RV_RESULT_TRUSTED, RV_RESULT_UNTRUSTED, RV_TOOL_CALL, RV_TOOL_CANCEL,
        RV_TOOL_CHANGES_FILES,
    };
    #[cfg(target_pointer_width = "64")]
    use super::{
        RvBytes, RvCall, RvDirEntries, RvDirEntry, RvFileAnswer, RvFileError, RvHost, RvLoadArgs,
        RvPackInfo, RvPackNewArgs, RvPackRenderArgs, RvPluginInfo, RvStat, RvStr, RvToolCallArgs,
        RvToolInfo, RvToolResult, RvWaker,
    };

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn a_string_is_a_pointer_then_a_length() {
        assert_eq!(size_of::<RvStr>(), 16);
        assert_eq!(offset_of!(RvStr, ptr), 0);
        assert_eq!(offset_of!(RvStr, len), 8);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_plugin_info_keeps_its_layout() {
        assert_eq!(size_of::<RvPluginInfo>(), 40);
        assert_eq!(offset_of!(RvPluginInfo, size), 0);
        assert_eq!(offset_of!(RvPluginInfo, reserved), 4);
        assert_eq!(offset_of!(RvPluginInfo, name), 8);
        assert_eq!(offset_of!(RvPluginInfo, version), 24);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_pack_info_keeps_its_layout() {
        assert_eq!(size_of::<RvPackInfo>(), 40);
        assert_eq!(offset_of!(RvPackInfo, size), 0);
        assert_eq!(offset_of!(RvPackInfo, reserved), 4);
        assert_eq!(offset_of!(RvPackInfo, name), 8);
        assert_eq!(offset_of!(RvPackInfo, description), 24);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_tool_info_keeps_its_layout() {
        assert_eq!(size_of::<RvToolInfo>(), 72);
        assert_eq!(offset_of!(RvToolInfo, size), 0);
        assert_eq!(offset_of!(RvToolInfo, flags), 4);
        assert_eq!(offset_of!(RvToolInfo, name), 8);
        assert_eq!(offset_of!(RvToolInfo, wire_name), 24);
        assert_eq!(offset_of!(RvToolInfo, description), 40);
        assert_eq!(offset_of!(RvToolInfo, parameters), 56);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_tool_result_keeps_its_layout() {
        assert_eq!(size_of::<RvToolResult>(), 24);
        assert_eq!(offset_of!(RvToolResult, size), 0);
        assert_eq!(offset_of!(RvToolResult, kind), 4);
        assert_eq!(offset_of!(RvToolResult, text), 8);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn what_a_file_answer_holds_keeps_its_layout() {
        assert_eq!(size_of::<RvBytes>(), 16);
        assert_eq!(offset_of!(RvBytes, ptr), 0);
        assert_eq!(offset_of!(RvBytes, len), 8);

        assert_eq!(size_of::<RvStat>(), 16);
        assert_eq!(offset_of!(RvStat, file_type), 0);
        assert_eq!(offset_of!(RvStat, reserved), 4);
        assert_eq!(offset_of!(RvStat, len), 8);

        assert_eq!(size_of::<RvDirEntry>(), 32);
        assert_eq!(offset_of!(RvDirEntry, name), 0);
        assert_eq!(offset_of!(RvDirEntry, stat), 16);

        assert_eq!(size_of::<RvDirEntries>(), 16);
        assert_eq!(offset_of!(RvDirEntries, ptr), 0);
        assert_eq!(offset_of!(RvDirEntries, len), 8);

        assert_eq!(size_of::<RvFileError>(), 24);
        assert_eq!(offset_of!(RvFileError, kind), 0);
        assert_eq!(offset_of!(RvFileError, reserved), 4);
        assert_eq!(offset_of!(RvFileError, text), 8);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_file_answer_keeps_its_layout() {
        assert_eq!(size_of::<RvFileAnswer>(), 80);
        assert_eq!(offset_of!(RvFileAnswer, size), 0);
        assert_eq!(offset_of!(RvFileAnswer, status), 4);
        assert_eq!(offset_of!(RvFileAnswer, stat), 8);
        assert_eq!(offset_of!(RvFileAnswer, entries), 24);
        assert_eq!(offset_of!(RvFileAnswer, bytes), 40);
        assert_eq!(offset_of!(RvFileAnswer, error), 56);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_host_keeps_its_layout() {
        assert_eq!(size_of::<RvHost>(), 16);
        assert_eq!(offset_of!(RvHost, size), 0);
        assert_eq!(offset_of!(RvHost, api_major), 4);
        assert_eq!(offset_of!(RvHost, api_minor), 8);
        assert_eq!(offset_of!(RvHost, reserved), 12);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_load_arguments_keep_their_layout() {
        assert_eq!(size_of::<RvLoadArgs>(), 40);
        assert_eq!(offset_of!(RvLoadArgs, size), 0);
        assert_eq!(offset_of!(RvLoadArgs, reserved), 4);
        assert_eq!(offset_of!(RvLoadArgs, ctx), 8);
        assert_eq!(offset_of!(RvLoadArgs, refuse), 16);
        assert_eq!(offset_of!(RvLoadArgs, describe_plugin), 24);
        assert_eq!(offset_of!(RvLoadArgs, insert_pack), 32);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_pack_new_arguments_keep_their_layout() {
        assert_eq!(size_of::<RvPackNewArgs>(), 48);
        assert_eq!(offset_of!(RvPackNewArgs, size), 0);
        assert_eq!(offset_of!(RvPackNewArgs, reserved), 4);
        assert_eq!(offset_of!(RvPackNewArgs, ctx), 8);
        assert_eq!(offset_of!(RvPackNewArgs, pack), 16);
        assert_eq!(offset_of!(RvPackNewArgs, refuse), 32);
        assert_eq!(offset_of!(RvPackNewArgs, add_tool), 40);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_pack_render_arguments_keep_their_layout() {
        assert_eq!(size_of::<RvPackRenderArgs>(), 80);
        assert_eq!(offset_of!(RvPackRenderArgs, size), 0);
        assert_eq!(offset_of!(RvPackRenderArgs, reserved), 4);
        assert_eq!(offset_of!(RvPackRenderArgs, ctx), 8);
        assert_eq!(offset_of!(RvPackRenderArgs, entry), 16);
        assert_eq!(offset_of!(RvPackRenderArgs, model), 32);
        assert_eq!(offset_of!(RvPackRenderArgs, offered), 48);
        assert_eq!(offset_of!(RvPackRenderArgs, offered_len), 56);
        assert_eq!(offset_of!(RvPackRenderArgs, refuse), 64);
        assert_eq!(offset_of!(RvPackRenderArgs, add_tool), 72);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_waker_keeps_its_layout() {
        assert_eq!(size_of::<RvWaker>(), 16);
        assert_eq!(offset_of!(RvWaker, ctx), 0);
        assert_eq!(offset_of!(RvWaker, wake), 8);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_tool_call_arguments_keep_their_layout() {
        assert_eq!(size_of::<RvToolCallArgs>(), 168);
        assert_eq!(offset_of!(RvToolCallArgs, size), 0);
        assert_eq!(offset_of!(RvToolCallArgs, reserved), 4);
        assert_eq!(offset_of!(RvToolCallArgs, ctx), 8);
        assert_eq!(offset_of!(RvToolCallArgs, tool), 16);
        assert_eq!(offset_of!(RvToolCallArgs, arguments), 32);
        assert_eq!(offset_of!(RvToolCallArgs, finish), 48);
        assert_eq!(offset_of!(RvToolCallArgs, file_stat), 56);
        assert_eq!(offset_of!(RvToolCallArgs, file_list), 64);
        assert_eq!(offset_of!(RvToolCallArgs, file_read), 72);
        assert_eq!(offset_of!(RvToolCallArgs, answer), 80);
        assert_eq!(offset_of!(RvToolCallArgs, file_write), 88);
        assert_eq!(offset_of!(RvToolCallArgs, file_append), 96);
        assert_eq!(offset_of!(RvToolCallArgs, file_mkdir), 104);
        assert_eq!(offset_of!(RvToolCallArgs, file_remove), 112);
        assert_eq!(offset_of!(RvToolCallArgs, file_rename), 120);
        assert_eq!(offset_of!(RvToolCallArgs, file_copy), 128);
        assert_eq!(offset_of!(RvToolCallArgs, resources), 136);
        assert_eq!(offset_of!(RvToolCallArgs, waker), 152);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_call_keeps_its_layout() {
        assert_eq!(size_of::<RvCall>(), 48);
        assert_eq!(offset_of!(RvCall, size), 0);
        assert_eq!(offset_of!(RvCall, verb), 4);
        assert_eq!(offset_of!(RvCall, host), 8);
        assert_eq!(offset_of!(RvCall, plugin), 16);
        assert_eq!(offset_of!(RvCall, pack_run), 24);
        assert_eq!(offset_of!(RvCall, args), 32);
        assert_eq!(offset_of!(RvCall, tool_call), 40);
    }

    #[test]
    fn the_status_codes_keep_their_numbers() {
        assert_eq!(RV_OK, 0);
        assert_eq!(RV_ERR_MISUSE, 4);
        assert_eq!(RV_ERR_REFUSED, 5);
        assert_eq!(RV_ERR_UNSUPPORTED, 6);
        assert_eq!(RV_PENDING, 7);
        assert_eq!(RV_ERR_FILE, 8);
    }

    #[test]
    fn the_byte_limits_keep_their_numbers() {
        assert_eq!(RV_MAX_NAME, 64);
        assert_eq!(RV_MAX_LINE, 1024);
        assert_eq!(RV_MAX_PATH, 4096);
        assert_eq!(RV_MAX_TOOL_TEXT, 65536);
        assert_eq!(RV_MAX_RESULT, 16_777_216);
        assert_eq!(RV_MAX_WRITE, 16_777_216);
    }

    #[test]
    fn the_tool_flags_keep_their_bits() {
        assert_eq!(RV_TOOL_CHANGES_FILES, 1);
    }

    #[test]
    fn the_file_types_and_the_file_error_kinds_keep_their_numbers() {
        assert_eq!(RV_FILE_TYPE_OTHER, 0);
        assert_eq!(RV_FILE_TYPE_FILE, 1);
        assert_eq!(RV_FILE_TYPE_DIRECTORY, 2);
        assert_eq!(RV_FILE_TYPE_SYMLINK, 3);
        assert_eq!(RV_FILE_ERROR_OTHER, 0);
        assert_eq!(RV_FILE_ERROR_NOT_FOUND, 1);
        assert_eq!(RV_FILE_ERROR_NOT_A_DIRECTORY, 2);
        assert_eq!(RV_FILE_ERROR_IS_A_DIRECTORY, 3);
        assert_eq!(RV_FILE_ERROR_DENIED, 4);
        assert_eq!(RV_FILE_ERROR_CONFLICT, 5);
        assert_eq!(RV_FILE_ERROR_IO, 6);
        assert_eq!(RV_FILE_ERROR_ALREADY_EXISTS, 7);
        assert_eq!(RV_FILE_ERROR_NOT_EMPTY, 8);
    }

    #[test]
    fn the_verbs_keep_their_numbers() {
        assert_eq!(RV_PLUGIN_LOAD, 1);
        assert_eq!(RV_PLUGIN_UNLOAD, 2);
        assert_eq!(RV_PACK_NEW, 3);
        assert_eq!(RV_PACK_DELETE, 4);
        assert_eq!(RV_TOOL_CALL, 5);
        assert_eq!(RV_TOOL_CANCEL, 6);
        assert_eq!(RV_PACK_RENDER, 7);
    }

    #[test]
    fn the_result_kinds_keep_their_numbers() {
        assert_eq!(RV_RESULT_UNTRUSTED, 0);
        assert_eq!(RV_RESULT_TRUSTED, 1);
        assert_eq!(RV_RESULT_INVALID_ARGUMENTS, 2);
        assert_eq!(RV_RESULT_BACKEND, 3);
        assert_eq!(RV_RESULT_TRANSPORT, 4);
        assert_eq!(RV_RESULT_OTHER, 5);
    }

    #[test]
    fn the_crate_version_is_the_contract_version() {
        assert_eq!(env!("CARGO_PKG_VERSION_MAJOR"), RV_API_MAJOR.to_string());
        assert_eq!(env!("CARGO_PKG_VERSION_MINOR"), RV_API_MINOR.to_string());
    }
}
