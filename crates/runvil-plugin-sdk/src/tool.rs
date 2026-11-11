//! A tool as data, [`ToolInfo`]; what one call of it is lent,
//! [`ToolCall`]; how the call ends, [`ToolOutput`] or [`ToolError`]; and
//! [`next_step`], which splits a long call into steps.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A tool described as data, which a [`Pack`](crate::Pack) lists in
/// [`Pack::tools`](crate::Pack::tools).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolInfo {
    /// The tool's name within its Pack: 1 to 64 bytes of lowercase ASCII
    /// letters, digits, `-`, `_`, and `.`, such as `fetch`. The Host puts
    /// the Pack id before it to form the tool's id, as in `url/fetch`.
    pub name: String,
    /// The name a model calls the tool by: 1 to 64 bytes of ASCII letters,
    /// digits, `_`, and `-`. `None` lets the Host derive it from the
    /// tool's id, as `url_fetch` from `url/fetch`.
    pub wire_name: Option<String>,
    /// What the tool does, for a model to read: non-empty, at most 65536
    /// bytes.
    pub description: String,
    /// The tool's parameters: the JSON text of a JSON Schema whose value
    /// is an object, at most 65536 bytes.
    pub parameters: String,
}

impl ToolInfo {
    /// A tool named `name`, with the description a model reads and the
    /// JSON text of its parameter schema, under the wire name the Host
    /// derives.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: impl Into<String>,
    ) -> ToolInfo {
        ToolInfo {
            name: name.into(),
            wire_name: None,
            description: description.into(),
            parameters: parameters.into(),
        }
    }

    /// Sets the name a model calls the tool by.
    #[must_use]
    pub fn wire_name(mut self, wire_name: impl Into<String>) -> ToolInfo {
        self.wire_name = Some(wire_name.into());
        self
    }
}

/// What one tool call lends its [`Pack`](crate::Pack): the tool's name
/// and the call's arguments. It borrows from the call, so a Pack cannot
/// keep it past the call's end.
#[derive(Clone, Copy, Debug)]
pub struct ToolCall<'a> {
    state: &'a CallState,
}

impl<'a> ToolCall<'a> {
    /// The handle over one call's state.
    pub(crate) fn new(state: &'a CallState) -> ToolCall<'a> {
        ToolCall { state }
    }

    /// The name of the called tool, as [`ToolInfo::name`] gave it. A Pack
    /// with several tools matches on it to pick one.
    #[must_use]
    pub fn tool(&self) -> &'a str {
        &self.state.tool
    }

    /// The call's arguments: the JSON text of the value the caller passed.
    /// The Plugin parses it with a JSON library of its own.
    #[must_use]
    pub fn arguments(&self) -> &'a str {
        &self.state.arguments
    }
}

/// What one tool call keeps for all of its steps: the copies of its tool
/// name and arguments that a [`ToolCall`] reads.
#[derive(Debug)]
pub(crate) struct CallState {
    pub(crate) tool: String,
    pub(crate) arguments: String,
}

/// The text a successful tool call returns, and whether the Plugin
/// vouches for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutput {
    text: String,
    trusted: bool,
}

impl ToolOutput {
    /// An output the Plugin vouches for: it wrote every byte of `text`,
    /// which a model reads as is.
    #[must_use]
    pub fn trusted(text: impl Into<String>) -> ToolOutput {
        ToolOutput {
            text: text.into(),
            trusted: true,
        }
    }

    /// An output that may hold data an attacker can influence, such as
    /// the contents of a file or a web page. The Host marks it before a
    /// model reads it.
    #[must_use]
    pub fn untrusted(text: impl Into<String>) -> ToolOutput {
        ToolOutput {
            text: text.into(),
            trusted: false,
        }
    }

    /// The output's text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the Plugin vouches for the text.
    #[must_use]
    pub fn is_trusted(&self) -> bool {
        self.trusted
    }
}

/// Why a tool call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolErrorKind {
    /// The arguments do not match the tool's parameters.
    InvalidArguments,
    /// What the tool reaches refused or failed the request.
    Backend,
    /// The request never got through.
    Transport,
    /// Any other reason.
    Other,
}

/// A failed tool call: why it failed, and a message a model can read that
/// says what went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolError {
    kind: ToolErrorKind,
    message: String,
}

impl ToolError {
    /// A failure of `kind` with `message`, which a model reads, so it
    /// names what was wrong and holds nothing private.
    #[must_use]
    pub fn new(kind: ToolErrorKind, message: impl Into<String>) -> ToolError {
        ToolError {
            kind,
            message: message.into(),
        }
    }

    /// Why the call failed.
    #[must_use]
    pub fn kind(&self) -> ToolErrorKind {
        self.kind
    }

    /// The message a model reads.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ToolError {}

/// Ends the current step of a tool call and goes on at the next one.
///
/// A [`Pack::call`](crate::Pack::call) future that does a long job awaits
/// this every so often, so that no step works for long and the Host can
/// abandon the call between two steps.
pub fn next_step() -> NextStep {
    NextStep { ended: false }
}

/// The future [`next_step`] returns: `Pending` for one poll, then `Ready`.
#[derive(Debug)]
#[must_use = "a future does nothing unless it is awaited"]
pub struct NextStep {
    /// Whether the one `Pending` has been returned.
    ended: bool,
}

impl Future for NextStep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.ended {
            return Poll::Ready(());
        }
        self.ended = true;
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use super::{CallState, ToolCall, ToolError, ToolErrorKind, ToolInfo, ToolOutput, next_step};

    #[test]
    fn a_tool_info_keeps_what_it_was_built_from_and_has_no_wire_name_until_one_is_set() {
        let info = ToolInfo::new("fetch", "Fetches a page.", r#"{"type":"object"}"#);
        assert_eq!(info.name, "fetch");
        assert_eq!(info.description, "Fetches a page.");
        assert_eq!(info.parameters, r#"{"type":"object"}"#);
        assert_eq!(info.wire_name, None);
        assert_eq!(info.wire_name("Fetch").wire_name.as_deref(), Some("Fetch"));
    }

    #[test]
    fn a_tool_call_lends_the_tool_name_and_the_arguments() {
        let state = CallState {
            tool: "fetch".to_owned(),
            arguments: r#"{"url":"x"}"#.to_owned(),
        };
        let call = ToolCall::new(&state);
        assert_eq!(call.tool(), "fetch");
        assert_eq!(call.arguments(), r#"{"url":"x"}"#);
    }

    #[test]
    fn an_output_carries_its_text_and_whether_it_is_trusted() {
        let trusted = ToolOutput::trusted("done");
        assert_eq!((trusted.text(), trusted.is_trusted()), ("done", true));
        let untrusted = ToolOutput::untrusted("page text");
        assert_eq!(
            (untrusted.text(), untrusted.is_trusted()),
            ("page text", false)
        );
    }

    #[test]
    fn an_error_carries_its_kind_and_displays_its_message() {
        let error = ToolError::new(ToolErrorKind::InvalidArguments, "url is missing");
        assert_eq!(error.kind(), ToolErrorKind::InvalidArguments);
        assert_eq!(error.message(), "url is missing");
        assert_eq!(error.to_string(), "url is missing");
    }

    #[test]
    fn next_step_is_pending_for_one_poll_and_then_ready() {
        let mut step = pin!(next_step());
        let mut cx = Context::from_waker(Waker::noop());
        assert_eq!(step.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(step.as_mut().poll(&mut cx), Poll::Ready(()));
    }
}
