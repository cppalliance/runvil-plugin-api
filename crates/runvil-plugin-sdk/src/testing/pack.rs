//! The functions a fake Host's `RvPackNewArgs` carries: `refuse` and
//! `add_tool`, each checking what the Plugin hands it as the Harness
//! does.

use std::ffi::c_void;

use runvil_plugin_sys::{RV_MAX_NAME, RV_MAX_TOOL_TEXT, RV_OK, RvStatus, RvStr, RvToolInfo};

use super::json;
use super::load::RESERVED;
use super::session::{self, Field, PACK_NEW, Shape, answer, field, shape};
use crate::ToolInfo;
use crate::names::{is_valid_name, is_valid_wire_name};

const TOOL_INFO: Shape = shape!("add_tool", "info", "RvToolInfo");
const TOOL_NAME: Field = field!("the tool's name", 64, RV_MAX_NAME);
const WIRE_NAME: Field = field!("the tool's wire name", 64, RV_MAX_NAME);
const DESCRIPTION: Field = field!("the tool's description", 65536, RV_MAX_TOOL_TEXT);
const PARAMETERS: Field = field!("the tool's parameter schema", 65536, RV_MAX_TOOL_TEXT);

/// Records the Plugin's refusal to make a Pack instance.
pub(super) unsafe extern "C" fn refuse(ctx: *mut c_void, reason: RvStr) -> RvStatus {
    session::refuse(ctx, &PACK_NEW, reason)
}

/// Records one tool the instance adds, validated; returns `RV_OK` or
/// `RV_ERR_MISUSE`.
pub(super) unsafe extern "C" fn add_tool(ctx: *mut c_void, info: *const RvToolInfo) -> RvStatus {
    answer(ctx, &PACK_NEW, |session| {
        // SAFETY: the contract makes `info` point at a readable
        // `RvToolInfo` during this call.
        let info = unsafe { TOOL_INFO.read(info) }?;
        if info.reserved != 0 {
            return Err(RESERVED);
        }
        // SAFETY: the contract makes each string valid for its length.
        let name = unsafe { TOOL_NAME.read(info.name) }?;
        if !is_valid_name(&name) {
            return Err("the tool's name is not one name segment, such as `fetch`");
        }
        if session.tools.iter().any(|tool| tool.name == name) {
            return Err("two tools of one Pack instance share a name");
        }
        // SAFETY: as above.
        let wire_name = unsafe { WIRE_NAME.read(info.wire_name) }?;
        if !wire_name.is_empty() && !is_valid_wire_name(&wire_name) {
            return Err("the tool's wire name is not ASCII letters, digits, `_`, and `-`");
        }
        // SAFETY: as above.
        let description = unsafe { DESCRIPTION.read(info.description) }?;
        if description.is_empty() {
            return Err("the tool's description is empty");
        }
        // SAFETY: as above.
        let parameters = unsafe { PARAMETERS.read(info.parameters) }?;
        if !json::is_object(&parameters) {
            return Err("the tool's parameter schema is not the JSON text of an object");
        }
        session.tools.push(ToolInfo {
            name,
            wire_name: (!wire_name.is_empty()).then_some(wire_name),
            description,
            parameters,
        });
        Ok(RV_OK)
    })
}

#[cfg(test)]
mod tests {
    //! The Pack functions' rules, through a raw entry point that breaks
    //! one of them at a time.

    use runvil_plugin_sys::RV_ERR_UNSUPPORTED;

    use crate::ToolInfo;
    use crate::testing::Failure;
    use crate::testing::fixture::{self, Breach, DELETES};

    #[test]
    fn an_instance_that_follows_the_contract_is_made_and_deleted_once() {
        let loaded = fixture::load(Breach::None).unwrap();
        let pack = loaded.new_pack("acme/fixture").unwrap();
        assert_eq!(
            pack.tools(),
            [
                ToolInfo::new("tool", "A fixture tool.", "{}").wire_name("Tool"),
                ToolInfo::new("other", "Another fixture tool.", "{}"),
            ]
        );
        assert_eq!(DELETES.get(), 0);
        drop(pack);
        assert_eq!(DELETES.get(), 1);
    }

    #[test]
    fn each_breach_of_a_pack_function_is_a_recorded_misuse_and_gets_no_delete() {
        for (breach, misuse) in [
            (
                Breach::PackCtx,
                "a Pack function was handed a ctx other than the open RV_PACK_NEW call's",
            ),
            (
                Breach::ToolName,
                "the tool's name is not one name segment, such as `fetch`",
            ),
            (
                Breach::ToolTwice,
                "two tools of one Pack instance share a name",
            ),
            (
                Breach::WireName,
                "the tool's wire name is not ASCII letters, digits, `_`, and `-`",
            ),
            (Breach::NoDescription, "the tool's description is empty"),
            (
                Breach::Parameters,
                "the tool's parameter schema is not the JSON text of an object",
            ),
            (
                Breach::LoadFunctionInPackNew,
                "a load function was called outside RV_PLUGIN_LOAD",
            ),
        ] {
            let loaded = fixture::load(breach).unwrap();
            assert_eq!(
                loaded.new_pack("acme/fixture").unwrap_err(),
                Failure::Misused(misuse),
                "{breach:?}"
            );
            assert_eq!(DELETES.get(), 0, "{breach:?}");
        }
    }

    #[test]
    fn a_refused_instance_reports_its_reason_and_gets_no_delete() {
        let loaded = fixture::load(Breach::RefusePack).unwrap();
        assert_eq!(
            loaded.new_pack("acme/fixture").unwrap_err(),
            Failure::Refused("no instance today".to_owned())
        );
        assert_eq!(DELETES.get(), 0);
    }

    #[test]
    fn an_instance_whose_call_returns_another_status_is_rejected_with_it() {
        let loaded = fixture::load(Breach::PackStatus).unwrap();
        assert_eq!(
            loaded.new_pack("acme/fixture").unwrap_err(),
            Failure::Status(RV_ERR_UNSUPPORTED)
        );
        assert_eq!(DELETES.get(), 0);
    }

    #[test]
    #[should_panic(expected = "the Plugin inserted no Pack named acme/absent")]
    fn making_a_pack_the_plugin_never_inserted_is_the_tests_own_fault() {
        let loaded = fixture::load(Breach::None).unwrap();
        let _pack = loaded.new_pack("acme/absent");
    }
}
