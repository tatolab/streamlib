// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one local API socket every verb reaches the machine's runtime through, at its fixed path
//! in this user's runtime directory, and the refusal when nothing answers there.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use streamlib_runtime_client_contract::machine_runtime_lock::{
    MachineRuntimeLockHolder, holder_of_the_machine_runtime_lock,
};
use streamlib_runtime_client_contract::streamlib_runtime_directory::{
    StreamlibRuntimeDirectory, current_process_uid,
};

use crate::TatolabCommandFailure;
use crate::local_api_mcp_tool_client::call_one_local_api_tool;

/// The fixed path the machine's runtime serves its local API on, resolved as a reader resolves
/// the runtime directory: creating nothing.
fn local_api_socket_path_on_this_machine() -> Result<PathBuf, TatolabCommandFailure> {
    StreamlibRuntimeDirectory::resolve_for_a_reader_without_creating()
        .map(|runtime_directory| runtime_directory.local_api_socket_path())
        .map_err(|runtime_directory_refusal| {
            TatolabCommandFailure::refused(runtime_directory_refusal.to_string())
        })
}

/// The machine's runtime's local API socket, once something answers a connect there; the
/// no-runtime refusal otherwise. No verb ever starts a runtime.
pub(crate) fn local_api_socket_of_the_running_runtime() -> Result<PathBuf, TatolabCommandFailure> {
    let local_api_socket_path = local_api_socket_path_on_this_machine()?;
    local_api_socket_answering_at(local_api_socket_path, || {
        holder_of_the_machine_runtime_lock().ok().flatten()
    })
}

/// Call `tool_name` once with `tool_arguments` on the machine's running runtime, answering the
/// tool's text: a one-shot verb's whole round trip.
pub(crate) fn call_one_tool_of_the_running_runtime(
    tool_name: &str,
    tool_arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<String, TatolabCommandFailure> {
    let local_api_socket_path = local_api_socket_of_the_running_runtime()?;
    Ok(call_one_local_api_tool(
        &local_api_socket_path,
        tool_name,
        tool_arguments,
    )?)
}

/// `local_api_socket_path` when something accepts a connect on it; the no-runtime refusal
/// otherwise, naming the holder `probe_the_machine_runtime_lock_holder` finds when it runs as
/// another user.
pub(crate) fn local_api_socket_answering_at(
    local_api_socket_path: PathBuf,
    probe_the_machine_runtime_lock_holder: impl FnOnce() -> Option<MachineRuntimeLockHolder>,
) -> Result<PathBuf, TatolabCommandFailure> {
    if something_answers_at(&local_api_socket_path) {
        return Ok(local_api_socket_path);
    }
    let holder_serving_another_user = holder_serving_another_user(
        probe_the_machine_runtime_lock_holder(),
        current_process_uid(),
    );
    Err(TatolabCommandFailure::refused(no_runtime_answers_refusal(
        &local_api_socket_path,
        holder_serving_another_user.as_ref(),
    )))
}

/// Whether a connect on `local_api_socket_path` is accepted.
pub(crate) fn something_answers_at(local_api_socket_path: &Path) -> bool {
    UnixStream::connect(local_api_socket_path).is_ok()
}

/// `machine_runtime_lock_holder` when it runs as a uid other than `this_users_uid`: a runtime
/// that serves another user's socket, not this one's.
fn holder_serving_another_user(
    machine_runtime_lock_holder: Option<MachineRuntimeLockHolder>,
    this_users_uid: u32,
) -> Option<MachineRuntimeLockHolder> {
    machine_runtime_lock_holder.filter(|holder| holder.uid != this_users_uid)
}

/// The one-line refusal when nothing answers at `local_api_socket_path`, naming the holder of the
/// machine runtime lock when it serves another user.
pub(crate) fn no_runtime_answers_refusal(
    local_api_socket_path: &Path,
    holder_serving_another_user: Option<&MachineRuntimeLockHolder>,
) -> String {
    let mut refusal = format!(
        "no runtime is running on this machine: nothing answers at {}. Start one by running \
         `tatolabd` in a terminal.",
        local_api_socket_path.display()
    );
    if let Some(holder_serving_another_user) = holder_serving_another_user {
        refusal.push_str(&format!(
            " The machine's runtime is held by {holder_serving_another_user}; it serves that \
             user's socket, not yours."
        ));
    }
    refusal
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub_local_api_server::{NOTHING_LISTENS_LOCAL_API_SOCKET_PATH, StubLocalApiServer};

    fn a_holder_running_as(uid: u32) -> MachineRuntimeLockHolder {
        MachineRuntimeLockHolder {
            user_name: Some("other".to_owned()),
            uid,
            pid: 4242,
            executable: Some(PathBuf::from("/opt/tatolab/bin/tatolabd")),
        }
    }

    #[test]
    fn with_no_holder_of_another_user_the_refusal_names_the_socket_and_how_to_start_a_runtime() {
        assert_eq!(
            no_runtime_answers_refusal(Path::new("/run/user/1000/streamlib/local-api.sock"), None),
            "no runtime is running on this machine: nothing answers at \
             /run/user/1000/streamlib/local-api.sock. Start one by running `tatolabd` in a \
             terminal."
        );
    }

    #[test]
    fn a_holder_of_another_user_is_named_after_the_socket() {
        assert_eq!(
            no_runtime_answers_refusal(
                Path::new("/run/user/1000/streamlib/local-api.sock"),
                Some(&a_holder_running_as(1001))
            ),
            "no runtime is running on this machine: nothing answers at \
             /run/user/1000/streamlib/local-api.sock. Start one by running `tatolabd` in a \
             terminal. The machine's runtime is held by user other (uid 1001), pid 4242, \
             /opt/tatolab/bin/tatolabd; it serves that user's socket, not yours."
        );
    }

    #[test]
    fn only_a_holder_running_as_another_uid_is_named() {
        assert_eq!(
            holder_serving_another_user(Some(a_holder_running_as(1001)), 1000),
            Some(a_holder_running_as(1001))
        );
        assert_eq!(
            holder_serving_another_user(Some(a_holder_running_as(1000)), 1000),
            None
        );
        assert_eq!(holder_serving_another_user(None, 1000), None);
    }

    #[test]
    fn a_socket_nothing_listens_on_is_refused_naming_it_and_the_probed_holder() {
        let this_users_uid = current_process_uid();
        let refusal = TatolabCommandFailure::refusal_message_of(local_api_socket_answering_at(
            PathBuf::from(NOTHING_LISTENS_LOCAL_API_SOCKET_PATH),
            || Some(a_holder_running_as(this_users_uid.wrapping_add(1))),
        ));

        assert!(
            refusal.starts_with(&format!(
                "no runtime is running on this machine: nothing answers at \
                 {NOTHING_LISTENS_LOCAL_API_SOCKET_PATH}."
            )),
            "{refusal}"
        );
        assert!(
            refusal.contains("The machine's runtime is held by user other"),
            "{refusal}"
        );
        assert!(!refusal.contains('\n'), "one line: {refusal}");
    }

    #[test]
    fn a_socket_with_a_closed_listener_left_behind_is_refused() {
        let stale_socket_directory = tempfile::Builder::new()
            .prefix("tl-stale-")
            .tempdir_in("/tmp")
            .unwrap();
        let stale_socket_path = stale_socket_directory.path().join("local-api.sock");
        drop(std::os::unix::net::UnixListener::bind(&stale_socket_path).unwrap());

        assert!(
            local_api_socket_answering_at(stale_socket_path.clone(), || None).is_err(),
            "a socket file whose listener is gone answers nothing"
        );
    }

    #[test]
    fn a_socket_a_runtime_answers_on_is_the_socket_every_verb_reaches() {
        let stub_local_api_server = StubLocalApiServer::serve_default();

        assert_eq!(
            local_api_socket_answering_at(
                stub_local_api_server.local_api_socket_path.clone(),
                || panic!("the holder is probed only when nothing answers")
            )
            .unwrap(),
            stub_local_api_server.local_api_socket_path
        );
    }
}
