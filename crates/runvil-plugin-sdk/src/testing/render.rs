//! `RV_PACK_RENDER` in the fake Host: [`PackRun::render`] sends the verb
//! with the tools a section offered, the functions an `RvPackRenderArgs`
//! carries check what the Plugin hands them as the Harness does, and the
//! rules the Harness applies to a whole render are checked on the answer.

use std::ffi::c_void;
use std::{mem, ptr};

use runvil_plugin_sys::{
    RV_ERR_UNSUPPORTED, RV_PACK_RENDER, RV_TOOL_CHANGES_FILES, RvPackRenderArgs, RvStatus, RvStr,
    RvToolInfo,
};

use super::json;
use super::pack::add;
use super::session::{self, PACK_RENDER, Verb};
use super::{Failure, PackRun};
use crate::ToolInfo;
use crate::entry::{rv_str, size_field};

/// The empty string, which lets the Host derive a tool's wire name.
const EMPTY: RvStr = RvStr {
    ptr: ptr::null(),
    len: 0,
};

/// Records the Plugin's refusal to render.
///
/// # Safety
/// `ctx` and `reason` come from the `RvPackRenderArgs` of a call, and the
/// reason is valid for its length during this call.
unsafe extern "C" fn refuse(ctx: *mut c_void, reason: RvStr) -> RvStatus {
    session::refuse(ctx, &PACK_RENDER, reason)
}

/// Records one tool the render lists, validated; returns `RV_OK` or
/// `RV_ERR_MISUSE`.
///
/// # Safety
/// `ctx` and `info` come from the `RvPackRenderArgs` of a call, and `info`,
/// when it is not NULL, is readable with valid strings during this call.
unsafe extern "C" fn add_tool(ctx: *mut c_void, info: *const RvToolInfo) -> RvStatus {
    // SAFETY: the contract makes `info` point at a readable `RvToolInfo`
    // with valid strings during this call.
    unsafe {
        add(
            ctx,
            &PACK_RENDER,
            info,
            "two tools of one render share a name",
        )
    }
}

impl PackRun<'_> {
    /// Sends `RV_PACK_RENDER` for the section entry `entry` and the model
    /// `model`, the JSON text of an object, with `offered` as the tools the
    /// section offered from this Pack in one set, and returns the tools the
    /// model is shown, in order. A Pack that answers `RV_ERR_UNSUPPORTED`
    /// shows the offered tools as they are, as the Harness does.
    ///
    /// Each tool the Plugin lists is checked as one a Pack adds in
    /// `RV_PACK_NEW` is, and then the render is checked as a whole:
    ///
    /// - A tool listed under the name of an offered tool keeps the offered
    ///   `parameters`. This fake Host compares the two texts byte for
    ///   byte, which is stricter than the Harness, which compares the JSON
    ///   they hold. A render that lists the offered `ToolInfo` with another
    ///   wire name or description passes both.
    /// - A tool listed under the name of a tool of this instance that the
    ///   section did not offer fails.
    /// - Any other name is a model-facing tool, and passes. A test may then
    ///   call it by that name, as a model does.
    /// - No two tools may show one wire name, the default one included.
    ///
    /// # Errors
    /// Returns the [`Failure`] the Harness would report, checked in the
    /// Harness's order: a refusal, then a misuse, then a status other than
    /// `RV_OK`, and then the first rule above that the answer breaks, as
    /// [`Failure::Rejected`], which names the tool.
    ///
    /// # Panics
    /// Panics when `model` is not the JSON text of an object, or `offered`
    /// holds a tool this instance did not add, which the Harness never
    /// sends.
    pub fn render(
        &self,
        entry: &str,
        model: &str,
        offered: &[ToolInfo],
    ) -> Result<Vec<ToolInfo>, Failure> {
        assert!(
            json::is_object(model),
            "the model must be the JSON text of an object, as the Host lends it"
        );
        for tool in offered {
            assert!(
                self.tools.iter().any(|added| added.name == tool.name),
                "the Pack instance has no tool named {}",
                tool.name
            );
        }
        let infos: Vec<RvToolInfo> = offered.iter().map(raw_info).collect();
        let mut call = self.new_call(RV_PACK_RENDER);
        let (status, mut session) = session::open(Verb::PackRender, |ctx| {
            let args = RvPackRenderArgs {
                size: size_field::<RvPackRenderArgs>(),
                reserved: 0,
                ctx,
                entry: rv_str(entry),
                model: rv_str(model),
                offered: if infos.is_empty() {
                    ptr::null()
                } else {
                    infos.as_ptr()
                },
                offered_len: infos.len(),
                refuse,
                add_tool,
            };
            call.args = (&raw const args).cast();
            // SAFETY: `load_entry`'s caller guaranteed `entry` follows the
            // contract, and `call`, `args`, and the tools `args` names are
            // live structs that nothing else uses.
            unsafe { (self.loaded.entry)(&raw mut call) }
        });
        if status == RV_ERR_UNSUPPORTED && session.refusal.is_none() && session.misuse.is_none() {
            return Ok(offered.to_vec());
        }
        session.rejection(status)?;
        let listed = mem::take(&mut session.tools);
        self.check_render(offered, &listed)?;
        self.rendered
            .borrow_mut()
            .extend(listed.iter().map(|tool| tool.name.clone()));
        Ok(listed)
    }

    /// The rules of a whole render, which no single `add_tool` can check.
    fn check_render(&self, offered: &[ToolInfo], listed: &[ToolInfo]) -> Result<(), Failure> {
        let mut shown: Vec<(String, &str)> = Vec::new();
        for tool in listed {
            if let Some(held) = offered.iter().find(|held| held.name == tool.name) {
                if held.parameters != tool.parameters {
                    return Err(Failure::Rejected(format!(
                        "the render listed the catalog tool {} with other parameters than the \
                         section offered; a catalog tool keeps its parameters, so list other \
                         parameters as a model-facing tool under a name of its own",
                        tool.name
                    )));
                }
            } else if self.tools.iter().any(|added| added.name == tool.name) {
                return Err(Failure::Rejected(format!(
                    "the render listed {}, a catalog tool the section did not offer in this \
                     set; a render lists a catalog tool only when the section offered it",
                    tool.name
                )));
            }
            let wire_name = self.wire_name_of(tool);
            if let Some((_, first)) = shown.iter().find(|(held, _)| *held == wire_name) {
                return Err(Failure::Rejected(format!(
                    "the render shows the tools {first} and {} under one wire name, {wire_name}",
                    tool.name
                )));
            }
            shown.push((wire_name, &tool.name));
        }
        Ok(())
    }

    /// The wire name a model calls `tool` by: its own, or else the id of
    /// the tool with each `/` and `.` turned into `_`.
    fn wire_name_of(&self, tool: &ToolInfo) -> String {
        tool.wire_name
            .clone()
            .unwrap_or_else(|| format!("{}/{}", self.pack_id, tool.name).replace(['/', '.'], "_"))
    }
}

/// `tool` in the form the contract lends an offered tool.
fn raw_info(tool: &ToolInfo) -> RvToolInfo {
    RvToolInfo {
        size: size_field::<RvToolInfo>(),
        flags: if tool.changes_files {
            RV_TOOL_CHANGES_FILES
        } else {
            0
        },
        name: rv_str(&tool.name),
        wire_name: tool.wire_name.as_deref().map_or(EMPTY, rv_str),
        description: rv_str(&tool.description),
        parameters: rv_str(&tool.parameters),
    }
}

#[cfg(test)]
mod tests {
    //! A render through the SDK's entry point and the fake Host: a Pack
    //! that keeps the default, one that reshapes its tools, one that
    //! refuses, and one for each rule a render can break.

    use runvil_plugin_sys::{RV_ERR_UNSUPPORTED, RV_PACK_RENDER, RvCall, RvStatus};

    use crate::entry::entry;
    use crate::testing::{self, Failure};
    use crate::{
        BoxFuture, Pack, PackInfo, Plugin, Refusal, RenderContext, ToolCall, ToolError, ToolInfo,
        ToolOutput,
    };

    const SCHEMA: &str = r#"{"type":"object","properties":{}}"#;
    const OTHER_SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string"}}}"#;
    const ENTRY: &str = "entry-1";
    const MODEL: &str = r#"{"name":"m","thinking":"never"}"#;

    /// The tools every Pack of [`Renderer`] adds: a read and a change.
    fn catalog() -> Vec<ToolInfo> {
        vec![
            ToolInfo::new("first", "The first tool.", SCHEMA).wire_name("First"),
            ToolInfo::new("second", "The second tool.", SCHEMA).changes_files(),
        ]
    }

    /// What a Pack of [`Renderer`] does with a render.
    #[derive(Clone, Copy)]
    enum Mode {
        /// Keeps the default.
        Plain,
        /// Hides `second`, shows `first` as `Look`, and adds the
        /// model-facing `lite`, which changes files.
        Reshapes,
        /// Refuses.
        Refuses,
        /// Panics.
        Panics,
        /// Lists `first` with other parameters.
        ChangesParameters,
        /// Lists `second` whatever was offered.
        ListsUnoffered,
        /// Lists `first` twice.
        Repeats,
        /// Lists two tools under the wire name `Same`.
        SharesWireName,
        /// Lists the model-facing tool `copy` under the wire name that
        /// `other` has by default.
        ClashesWithDefault,
        /// Lists a tool whose name is not one name segment.
        BadName,
        /// Lists two tools whose default wire names differ only where one
        /// name has a `.` and the other a `_`.
        Dotted,
        /// Lists `first` with its parameters spaced out, the same JSON.
        Respaced,
    }

    /// A Plugin whose Packs differ only in what their render does.
    struct Renderer;

    impl Plugin for Renderer {
        const NAME: &'static str = "renderer";
        const VERSION: &'static str = "1";
        const PACKS: &'static [PackInfo] = &[
            PackInfo::new("acme/plain"),
            PackInfo::new("acme/reshape"),
            PackInfo::new("acme/refuses"),
            PackInfo::new("acme/panics"),
            PackInfo::new("acme/changes"),
            PackInfo::new("acme/unoffered"),
            PackInfo::new("acme/repeats"),
            PackInfo::new("acme/shares"),
            PackInfo::new("acme/clashes"),
            PackInfo::new("acme/badname"),
            PackInfo::new("acme/dots"),
            PackInfo::new("acme/spaced"),
            PackInfo::new("acme/inherits"),
        ];

        fn load() -> Result<Renderer, Refusal> {
            Ok(Renderer)
        }

        fn new_pack(&self, pack: &str) -> Result<Box<dyn Pack>, Refusal> {
            let mode = match pack {
                "acme/plain" => Mode::Plain,
                "acme/reshape" => Mode::Reshapes,
                "acme/refuses" => Mode::Refuses,
                "acme/panics" => Mode::Panics,
                "acme/changes" => Mode::ChangesParameters,
                "acme/unoffered" => Mode::ListsUnoffered,
                "acme/repeats" => Mode::Repeats,
                "acme/shares" => Mode::SharesWireName,
                "acme/clashes" => Mode::ClashesWithDefault,
                "acme/dots" => Mode::Dotted,
                "acme/spaced" => Mode::Respaced,
                "acme/inherits" => return Ok(Box::new(Inherits)),
                _ => Mode::BadName,
            };
            Ok(Box::new(Shaper(mode)))
        }
    }

    /// A Pack that does not override `render`.
    struct Inherits;

    impl Pack for Inherits {
        fn tools(&self) -> Vec<ToolInfo> {
            catalog()
        }

        fn call<'a>(&'a self, _call: ToolCall<'a>) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            Box::pin(async { Ok(ToolOutput::trusted("")) })
        }
    }

    struct Shaper(Mode);

    impl Pack for Shaper {
        fn tools(&self) -> Vec<ToolInfo> {
            catalog()
        }

        fn call<'a>(&'a self, call: ToolCall<'a>) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            let text = format!("called {}", call.tool());
            Box::pin(async move { Ok(ToolOutput::trusted(text)) })
        }

        fn render(
            &self,
            cx: &RenderContext<'_>,
            offered: Vec<ToolInfo>,
        ) -> Result<Vec<ToolInfo>, Refusal> {
            let first = || offered.iter().find(|tool| tool.name == "first").cloned();
            match self.0 {
                Mode::Plain => Ok(offered),
                Mode::Reshapes => {
                    let mut shown: Vec<ToolInfo> = offered
                        .into_iter()
                        .filter(|tool| tool.name != "second")
                        .map(|tool| tool.wire_name("Look"))
                        .collect();
                    shown.push(
                        ToolInfo::new(
                            "lite",
                            format!("{} saw {}", cx.entry, cx.model),
                            OTHER_SCHEMA,
                        )
                        .changes_files(),
                    );
                    Ok(shown)
                }
                Mode::Refuses => Err(Refusal::new("no tools for this model")),
                Mode::Panics => panic!("boom"),
                Mode::ChangesParameters => Ok(first()
                    .map(|mut tool| {
                        tool.parameters = OTHER_SCHEMA.to_owned();
                        tool
                    })
                    .into_iter()
                    .collect()),
                Mode::ListsUnoffered => Ok(vec![catalog().remove(1)]),
                Mode::Repeats => Ok(first().into_iter().chain(first()).collect()),
                Mode::SharesWireName => Ok(vec![
                    ToolInfo::new("one", "One.", SCHEMA).wire_name("Same"),
                    ToolInfo::new("two", "Two.", SCHEMA).wire_name("Same"),
                ]),
                Mode::ClashesWithDefault => Ok(vec![
                    ToolInfo::new("copy", "A copy.", SCHEMA).wire_name("clashes_other"),
                    ToolInfo::new("other", "Another.", SCHEMA),
                ]),
                Mode::BadName => Ok(vec![ToolInfo::new("Not A Segment", "A tool.", SCHEMA)]),
                Mode::Dotted => Ok(vec![
                    ToolInfo::new("a.b", "Dotted.", SCHEMA),
                    ToolInfo::new("a_b", "Underscored.", SCHEMA),
                ]),
                Mode::Respaced => Ok(first()
                    .map(|mut tool| {
                        tool.parameters = SCHEMA.replace(':', ": ");
                        tool
                    })
                    .into_iter()
                    .collect()),
            }
        }
    }

    /// Sends `RV_PACK_RENDER` for the Pack `pack` of [`Renderer`] with
    /// `offered`, and returns the result.
    fn render(pack: &str, offered: &[ToolInfo]) -> Result<Vec<ToolInfo>, Failure> {
        let loaded = testing::load::<Renderer>().unwrap();
        let run = loaded.new_pack(pack).unwrap();
        run.render(ENTRY, MODEL, offered)
    }

    #[test]
    fn a_pack_that_keeps_the_default_lists_what_it_was_offered() {
        assert_eq!(render("acme/plain", &catalog()), Ok(catalog()));
        assert_eq!(
            render("acme/plain", &catalog()[..1]),
            Ok(catalog()[..1].to_vec())
        );
        assert_eq!(render("acme/plain", &[]), Ok(Vec::new()));
    }

    #[test]
    fn a_pack_that_inherits_the_default_render_lists_what_it_was_offered_in_order() {
        let mut backwards = catalog();
        backwards.reverse();
        assert_eq!(
            render("acme/inherits", &backwards),
            Ok(backwards),
            "the default keeps every tool, its order, and its text"
        );
        assert_eq!(render("acme/inherits", &[]), Ok(Vec::new()));
    }

    #[test]
    fn a_test_calls_a_model_facing_tool_by_the_name_a_render_listed_it_under() {
        let loaded = testing::load::<Renderer>().unwrap();
        let run = loaded.new_pack("acme/reshape").unwrap();
        run.render(ENTRY, MODEL, &catalog()).unwrap();
        assert_eq!(
            run.call("lite", "{}"),
            Ok(Ok(ToolOutput::trusted("called lite")))
        );
    }

    #[test]
    #[should_panic(expected = "the Pack instance has no tool named lite")]
    fn a_model_facing_tool_is_not_callable_before_a_render_lists_it() {
        let loaded = testing::load::<Renderer>().unwrap();
        let run = loaded.new_pack("acme/reshape").unwrap();
        let _call = run.start("lite", "{}");
    }

    #[test]
    fn a_render_may_rename_hide_and_add_a_model_facing_tool() {
        let shown = render("acme/reshape", &catalog()).unwrap();
        assert_eq!(
            shown,
            [
                ToolInfo::new("first", "The first tool.", SCHEMA).wire_name("Look"),
                ToolInfo::new("lite", format!("{ENTRY} saw {MODEL}"), OTHER_SCHEMA).changes_files(),
            ]
        );
    }

    #[test]
    fn a_render_leaves_the_instance_as_it_was() {
        let loaded = testing::load::<Renderer>().unwrap();
        let run = loaded.new_pack("acme/reshape").unwrap();
        let first = run.render(ENTRY, MODEL, &catalog()).unwrap();
        assert_eq!(run.render(ENTRY, MODEL, &catalog()).unwrap(), first);
        assert_eq!(run.tools(), catalog());
        assert_eq!(
            run.call("second", "{}"),
            Ok(Ok(ToolOutput::trusted("called second")))
        );
    }

    #[test]
    fn a_refusal_reaches_the_test_with_its_reason() {
        assert_eq!(
            render("acme/refuses", &catalog()),
            Err(Failure::Refused("no tools for this model".to_owned()))
        );
    }

    #[test]
    fn a_panic_in_a_render_is_a_refusal() {
        assert_eq!(
            render("acme/panics", &catalog()),
            Err(Failure::Refused(
                "the Pack panicked while it rendered: boom".to_owned()
            ))
        );
    }

    #[test]
    fn a_catalog_tool_listed_with_other_parameters_fails_and_names_the_tool() {
        let Err(Failure::Rejected(reason)) = render("acme/changes", &catalog()) else {
            panic!("the render should be rejected");
        };
        assert!(
            reason.contains("the catalog tool first with other parameters"),
            "{reason}"
        );
        assert!(
            reason.contains("model-facing tool under a name of its own"),
            "{reason}"
        );
    }

    #[test]
    fn a_catalog_tool_the_section_did_not_offer_fails_and_names_the_tool() {
        let Err(Failure::Rejected(reason)) = render("acme/unoffered", &catalog()[..1]) else {
            panic!("the render should be rejected");
        };
        assert!(
            reason.contains("the render listed second, a catalog tool the section did not offer"),
            "{reason}"
        );
        // Offered, the same tool is the section's own.
        assert_eq!(
            render("acme/unoffered", &catalog()),
            Ok(vec![catalog().remove(1)])
        );
    }

    #[test]
    fn two_tools_under_one_wire_name_fail_the_default_name_included() {
        assert_eq!(
            render("acme/shares", &[]),
            Err(Failure::Rejected(
                "the render shows the tools one and two under one wire name, Same".to_owned()
            ))
        );
        assert_eq!(
            render("acme/clashes", &[]),
            Err(Failure::Rejected(
                "the render shows the tools copy and other under one wire name, clashes_other"
                    .to_owned()
            ))
        );
    }

    #[test]
    fn default_wire_names_that_differ_only_by_a_dot_clash() {
        assert_eq!(
            render("acme/dots", &[]),
            Err(Failure::Rejected(
                "the render shows the tools a.b and a_b under one wire name, dots_a_b".to_owned()
            ))
        );
    }

    #[test]
    fn parameters_that_differ_only_in_spacing_fail_because_the_text_is_compared() {
        let Err(Failure::Rejected(reason)) = render("acme/spaced", &catalog()) else {
            panic!("the render should be rejected");
        };
        assert!(
            reason.contains("the catalog tool first with other parameters"),
            "{reason}"
        );
    }

    #[test]
    fn a_tool_the_host_rejects_is_a_misuse() {
        assert_eq!(
            render("acme/repeats", &catalog()),
            Err(Failure::Misused("two tools of one render share a name"))
        );
        assert_eq!(
            render("acme/badname", &catalog()),
            Err(Failure::Misused(
                "the tool's name is not one name segment, such as `fetch`"
            ))
        );
    }

    /// The entry point of [`Renderer`], behind a Plugin that does not
    /// render: it answers verb 7 as a Plugin built for no render does.
    ///
    /// # Safety
    /// `call` is a whole `RvCall` that the fake Host made.
    unsafe extern "C" fn no_render(call: *mut RvCall) -> RvStatus {
        // SAFETY: the fake Host made `call`, so it is whole.
        if unsafe { (*call).verb } == RV_PACK_RENDER {
            return RV_ERR_UNSUPPORTED;
        }
        // SAFETY: any other verb follows the contract, which `entry` needs.
        unsafe { entry::<Renderer>(call) }
    }

    #[test]
    fn a_plugin_that_answers_unsupported_shows_the_offered_tools_as_they_are() {
        // SAFETY: `no_render` follows the contract and never unwinds.
        let loaded = unsafe { testing::load_entry(no_render) }.unwrap();
        let run = loaded.new_pack("acme/plain").unwrap();
        assert_eq!(
            run.render(ENTRY, MODEL, &catalog()[1..]),
            Ok(catalog()[1..].to_vec())
        );
    }

    #[test]
    #[should_panic(expected = "the Pack instance has no tool named ghost")]
    fn offering_a_tool_the_instance_never_added_is_the_tests_own_fault() {
        let loaded = testing::load::<Renderer>().unwrap();
        let run = loaded.new_pack("acme/plain").unwrap();
        let _shown = run.render(ENTRY, MODEL, &[ToolInfo::new("ghost", "A ghost.", SCHEMA)]);
    }

    #[test]
    #[should_panic(expected = "the model must be the JSON text of an object")]
    fn a_model_that_is_not_an_object_is_the_tests_own_fault() {
        let loaded = testing::load::<Renderer>().unwrap();
        let run = loaded.new_pack("acme/plain").unwrap();
        let _shown = run.render(ENTRY, "not json", &catalog());
    }
}
