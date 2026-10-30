// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The machine's local relay as `tatolabd` makes its two instances: each a
//! supervised `moq-relay` child found beside `tatolabd` in the runtime unit,
//! both sharing the one publish token `tatolabd` mints for itself, their
//! admission answered on the engine's own async runtime.

use crate::runtime_unit_executable_beside_tatolabd::runtime_unit_executable_beside_this_one;

use streamlib::sdk::runtime::Runner;
use streamlib_relay_adapter_local::{
    LOCAL_RELAY_EXECUTABLE_FILE_NAME, LocalRelayAdapterInstanceMaker,
    LocalRelaySharedByBothInstances, SettingsOfEachLocalRelayListener,
    mint_the_runtimes_relay_publish_token,
};

/// The maker of the local relay's two instances on `engine`'s machine; when
/// none can run, both are still listed and each says why as it fails to join.
pub(crate) fn the_local_relay_adapter_of_this_runtime(
    engine: &Runner,
) -> LocalRelayAdapterInstanceMaker {
    match the_local_relay_shared_by_both_instances(engine) {
        Ok((shared, settings_of_each_listener)) => {
            LocalRelayAdapterInstanceMaker::sharing(shared, settings_of_each_listener)
        }
        Err(why_not) => LocalRelayAdapterInstanceMaker::running_no_local_relay(why_not),
    }
}

fn the_local_relay_shared_by_both_instances(
    engine: &Runner,
) -> Result<
    (
        LocalRelaySharedByBothInstances,
        SettingsOfEachLocalRelayListener,
    ),
    String,
> {
    let settings_of_each_listener = the_settings_of_both_listeners()?;
    let relay_executable =
        runtime_unit_executable_beside_this_one(LOCAL_RELAY_EXECUTABLE_FILE_NAME)?;
    let runtime_publish_token =
        mint_the_runtimes_relay_publish_token().map_err(|refusal| refusal.to_string())?;
    Ok((
        LocalRelaySharedByBothInstances {
            relay_executable,
            relays_directory: engine.runtime_directory().relays_directory(),
            machine_name: engine.machine_name().clone(),
            runtime_publish_token,
            async_runtime: engine.tokio_handle(),
        },
        settings_of_each_listener,
    ))
}

/// Each listener with the settings it is built in with: its defaults.
#[cfg(not(feature = "machine-directories-under-a-test-root"))]
fn the_settings_of_both_listeners() -> Result<SettingsOfEachLocalRelayListener, String> {
    Ok(streamlib_relay_adapter_local::default_settings_of_each_local_relay_listener())
}

/// Each listener with the port the test names for it, or why none runs: a
/// test build never binds the machine's own relay ports, so one whose test
/// names no ports runs no local relay.
#[cfg(feature = "machine-directories-under-a-test-root")]
fn the_settings_of_both_listeners() -> Result<SettingsOfEachLocalRelayListener, String> {
    use streamlib_relay_adapter_local::LocalRelayListener;
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
            let mut settings = serde_json::Map::new();
            settings.insert("port".to_string(), port.into());
            Ok((listener, settings))
        })
        .collect()
}
