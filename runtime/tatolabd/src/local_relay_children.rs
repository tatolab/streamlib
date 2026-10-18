// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The machine's local relay, joined as `tatolabd` starts and left inside its
//! shutdown: one instance per listener, each a supervised `moq-relay` child
//! found beside `tatolabd` in the runtime unit, all sharing the one publish
//! token `tatolabd` mints for itself, their admission answered on the engine's
//! own async runtime.

use std::path::PathBuf;

use streamlib::sdk::runtime::Runner;
use streamlib_relay_adapter_local::{
    LocalRelayAdapter, LocalRelayListener, LocalRelaySharedByBothInstances,
    local_relay_executable_beside, mint_the_runtimes_relay_publish_token,
};
use streamlib_relay_adapter_sdk::RelayAdapter;

/// The local relay's instances while `tatolabd` runs; dropping this stops
/// every child and its admission, then says so.
pub(crate) struct LocalRelayChildrenOfThisRuntime {
    joined_instances: Vec<LocalRelayAdapter>,
}

impl Drop for LocalRelayChildrenOfThisRuntime {
    fn drop(&mut self) {
        for instance in &mut self.joined_instances {
            instance.leave();
        }
        tracing::info!("the local relay has stopped");
    }
}

/// Join both of the local relay's instances, or say why none runs. A
/// listener that cannot be joined is said by name and the other runs alone.
pub(crate) fn join_the_local_relay(engine: &Runner) -> Option<LocalRelayChildrenOfThisRuntime> {
    let settings_of_each_listener = match the_settings_of_both_listeners() {
        Ok(settings_of_each_listener) => settings_of_each_listener,
        Err(why_not) => {
            tracing::info!("no local relay runs: {why_not}");
            return None;
        }
    };
    let relay_executable = match the_local_relay_executable_beside_this_one() {
        Ok(relay_executable) => relay_executable,
        Err(why_not) => {
            tracing::error!("no local relay runs, so no exposed port reaches a relay: {why_not}");
            return None;
        }
    };
    let runtime_publish_token = match mint_the_runtimes_relay_publish_token() {
        Ok(runtime_publish_token) => runtime_publish_token,
        Err(refusal) => {
            tracing::error!("no local relay runs: {refusal}");
            return None;
        }
    };
    let shared = LocalRelaySharedByBothInstances {
        relay_executable,
        relays_directory: engine.runtime_directory().relays_directory(),
        machine_name: engine.machine_name().clone(),
        runtime_publish_token,
        async_runtime: engine.tokio_handle(),
    };

    let mut joined_instances = Vec::new();
    for (listener, settings) in settings_of_each_listener {
        let mut instance = LocalRelayAdapter::for_the_listener(listener, shared.clone());
        match instance.join(settings) {
            Ok(joined) => {
                tracing::info!(
                    relay = %instance.relay_id(),
                    "the local relay listens at {}, serving {} ports",
                    joined.dial_url,
                    listener
                        .levels_served()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" and ")
                );
                joined_instances.push(instance);
            }
            Err(refusal) => tracing::error!("{refusal}"),
        }
    }
    Some(LocalRelayChildrenOfThisRuntime { joined_instances })
}

/// `moq-relay` beside this executable, canonical, or why it is not there.
fn the_local_relay_executable_beside_this_one() -> Result<PathBuf, String> {
    let this_executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|failure| format!("this executable's own path cannot be named: {failure}"))?;
    let relay_executable = local_relay_executable_beside(&this_executable).ok_or_else(|| {
        format!(
            "{} sits in no directory to find the relay in",
            this_executable.display()
        )
    })?;
    if !relay_executable.is_file() {
        return Err(format!(
            "the runtime unit carries no {}; rebuild it with `cargo xtask build-runtime`",
            relay_executable.display()
        ));
    }
    Ok(relay_executable)
}

/// Each listener with the settings it is joined with: its defaults.
#[cfg(not(feature = "machine-directories-under-a-test-root"))]
fn the_settings_of_both_listeners() -> Result<Vec<(LocalRelayListener, serde_json::Value)>, String>
{
    Ok(LocalRelayListener::BOTH
        .into_iter()
        .map(|listener| (listener, serde_json::json!({})))
        .collect())
}

/// Each listener with the port the test names for it, or why none runs: a
/// test build never binds the machine's own relay ports, so one whose test
/// names no ports runs no local relay.
#[cfg(feature = "machine-directories-under-a-test-root")]
fn the_settings_of_both_listeners() -> Result<Vec<(LocalRelayListener, serde_json::Value)>, String>
{
    use streamlib_runtime_client_contract::machine_directories_test_root::{
        TEST_LOCAL_RELAY_EVERY_INTERFACE_PORT_ENVIRONMENT_VARIABLE,
        TEST_LOCAL_RELAY_LOOPBACK_PORT_ENVIRONMENT_VARIABLE,
    };
    LocalRelayListener::BOTH
        .into_iter()
        .map(|listener| {
            let port_variable = match listener {
                LocalRelayListener::Loopback => TEST_LOCAL_RELAY_LOOPBACK_PORT_ENVIRONMENT_VARIABLE,
                LocalRelayListener::EveryInterface => {
                    TEST_LOCAL_RELAY_EVERY_INTERFACE_PORT_ENVIRONMENT_VARIABLE
                }
            };
            let port: u16 = std::env::var(port_variable)
                .ok()
                .and_then(|port| port.parse().ok())
                .ok_or_else(|| {
                    format!(
                        "this test build names no port for it in {port_variable}, and never \
                         binds the machine's own"
                    )
                })?;
            Ok((listener, serde_json::json!({ "port": port })))
        })
        .collect()
}
