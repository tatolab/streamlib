// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Why a runtime whose name is already live on its mesh does not start.
//!
//! A runtime name is the address other runtimes and agents wire against —
//! `<runtime name>/<display name>/<port>` — so two holders of one name make
//! that address mean two things. The check asks the mesh who holds the name
//! before this runtime declares anything, and refuses by name.
//!
//! **The query runs before the token is declared**, because a local liveliness
//! `get` answers with this session's own tokens too.
//!
//! Best-effort by construction, and the plan says so: a peer this runtime is
//! not yet connected to cannot answer, so two runtimes that start inside one
//! discovery window both run. That residual is reported rather than hidden —
//! see [`super::runtime_mesh_membership`], which says so once per peer.

use std::time::Duration;

use crate::core::error::{Error, Result};
use crate::core::runtime::mesh::HostIdentity;
use crate::core::runtime::mesh::runtime_mesh_key::{AnnouncedRuntimeIdentity, RuntimeMeshKeySpace};
use crate::core::runtime::mesh::runtime_mesh_name::RuntimeMeshName;

/// How long connected peers have to answer who holds this runtime's name.
///
/// A ceiling, not part of startup: the answer comes from this session's own
/// view of its connected peers' declarations rather than from the holder's
/// process, so measured on the loopback a held name comes back in about 140 µs,
/// an unheld one finalises in under 100 µs, and a holder that has been
/// SIGSTOPped is still answered inside 336 µs. Engine-chosen; nothing
/// authorable.
const HOW_LONG_PEERS_HAVE_TO_SAY_WHO_HOLDS_THIS_NAME: Duration = Duration::from_secs(2);

/// Refuse this runtime when its name is already live on the mesh.
///
/// Runs on the session that has just opened, before it declares anything.
pub fn refuse_this_runtime_if_its_name_is_already_live(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    mesh_name: &RuntimeMeshName,
    this_runtime: &AnnouncedRuntimeIdentity,
) -> Result<()> {
    let this_host = HostIdentity::of_this_host();
    for holder in every_live_holder_of(session, key_space, &this_runtime.runtime_name) {
        if a_runtime_here_may_take_this_name_over(&this_host, &holder, a_process_here_is_gone) {
            tracing::info!(
                "Runtime name {} was left on the {mesh_name} mesh by pid {} on this host, whose \
                 process is gone; taking it over",
                holder.runtime_name,
                holder.process_id
            );
            continue;
        }
        return Err(Error::Runtime(why_this_name_is_not_available(
            mesh_name, &this_host, &holder,
        )));
    }
    Ok(())
}

/// Every runtime whose liveliness token is live under `runtime_name`.
fn every_live_holder_of(
    session: &zenoh::Session,
    key_space: &RuntimeMeshKeySpace,
    runtime_name: &str,
) -> Vec<AnnouncedRuntimeIdentity> {
    // A mesh that cannot be asked is not a mesh that said yes, but it is also
    // not grounds to fail a start the plan never lets the mesh fail. The
    // duplicate then shows up as the stated residual.
    key_space
        .every_runtime_announced_under_the_name(
            session,
            runtime_name,
            HOW_LONG_PEERS_HAVE_TO_SAY_WHO_HOLDS_THIS_NAME,
        )
        .inspect_err(|query_failure| {
            tracing::warn!(
                "could not ask the mesh who holds the runtime name {runtime_name}, so a live \
                 duplicate would go unrefused: {query_failure}"
            );
        })
        .unwrap_or_default()
}

/// The exception's decision, with the process probe named so the table is
/// testable without a second host.
///
/// A name is free only when its holder is on this very host — this kernel boot,
/// this pid namespace — and its process has left the process table. Everything
/// else refuses: another host's pid is not one this host can check, a container
/// on this kernel has its own pid namespace, and a host that reports no
/// identity is never this one.
fn a_runtime_here_may_take_this_name_over(
    this_host: &HostIdentity,
    holder: &AnnouncedRuntimeIdentity,
    ask_whether_a_process_here_is_gone: impl Fn(u32) -> bool,
) -> bool {
    this_host.is_the_same_host_a_pid_can_be_checked_on(&holder.host_identity)
        && ask_whether_a_process_here_is_gone(holder.process_id)
}

/// Whether a pid on this host has left its process table.
fn a_process_here_is_gone(process_id: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::linux::host_identity::a_process_on_this_host_is_gone(process_id)
    }
    // No other platform reports a host identity, so no announced host ever
    // equals this one and this arm is never reached. It answers "still there",
    // which keeps the refusal — the same posture as the probe's own error arms.
    #[cfg(not(target_os = "linux"))]
    {
        let _ = process_id;
        false
    }
}

/// The refusal: the name, who holds it, and both ways out.
fn why_this_name_is_not_available(
    mesh_name: &RuntimeMeshName,
    this_host: &HostIdentity,
    holder: &AnnouncedRuntimeIdentity,
) -> String {
    format!(
        "Runtime name {} is already live on the {mesh_name} mesh, held by {} (pid {}). A runtime \
         name is the address other runtimes wire against, so two runtimes may not hold one. \
         Either stop that runtime — `streamlib nodes --mesh-name {mesh_name}` lists it wherever \
         it is running — or start this one under another name: `--runtime-name <name>` \
         on `streamlib run` / `dev`, the STREAMLIB_RUNTIME_NAME environment variable, or \
         Runtime(runtime_name=\"<name>\").",
        holder.runtime_name,
        where_the_holder_is(this_host, &holder.host_identity),
        holder.process_id
    )
}

/// How the holder's host is named to somebody reading the refusal. The token
/// carries an identity rather than a host name — a dead runtime answers no
/// query — so this says what that identity means from here.
fn where_the_holder_is(this_host: &HostIdentity, announced_host: &HostIdentity) -> String {
    if this_host.is_the_same_host_a_pid_can_be_checked_on(announced_host) {
        return "a process on this host".to_string();
    }
    match announced_host {
        HostIdentity::ThisKernelBootAndPidNamespace {
            kernel_boot_id,
            pid_namespace_inode,
        } => format!(
            "another host (kernel boot {kernel_boot_id}, pid namespace {pid_namespace_inode})"
        ),
        HostIdentity::Unidentified => "a host that reports no identity".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identified(kernel_boot_id: &str, pid_namespace_inode: u64) -> HostIdentity {
        HostIdentity::ThisKernelBootAndPidNamespace {
            kernel_boot_id: kernel_boot_id.to_string(),
            pid_namespace_inode,
        }
    }

    fn held_by(host_identity: HostIdentity, process_id: u32) -> AnnouncedRuntimeIdentity {
        AnnouncedRuntimeIdentity {
            runtime_name: "rig-desk-a1b2".to_string(),
            host_identity,
            process_id,
        }
    }

    fn a_mesh_name(name: &str) -> RuntimeMeshName {
        RuntimeMeshName::from_configuration_environment_or_default(Some(name.to_string()))
            .expect("a legal mesh name")
    }

    /// The whole exception: only a token this host left behind frees the name.
    ///
    /// The host half of the table is the predicate's own — see
    /// `HostIdentity::is_the_same_host_a_pid_can_be_checked_on`'s test. This
    /// walks it again because the exception is what the ticket names, and
    /// because the composition is the part that lives here: both halves must
    /// hold, and neither alone is enough.
    #[test]
    fn only_a_dead_process_on_this_very_host_leaves_its_name_free() {
        let here = identified("this-boot", 4_026_531_836);
        let every_process_is_gone = |_| true;

        assert!(
            a_runtime_here_may_take_this_name_over(
                &here,
                &held_by(here.clone(), 4321),
                every_process_is_gone
            ),
            "a token this host left behind must free its name"
        );
        assert!(
            !a_runtime_here_may_take_this_name_over(&here, &held_by(here.clone(), 4321), |_| false),
            "a live process on this host must keep its name"
        );
        assert!(
            !a_runtime_here_may_take_this_name_over(
                &here,
                &held_by(identified("this-boot", 4_026_532_000), 4321),
                every_process_is_gone
            ),
            "a container on this kernel has its own pid namespace, so its pid is not ours to check"
        );
        assert!(
            !a_runtime_here_may_take_this_name_over(
                &here,
                &held_by(identified("another-boot", 4_026_531_836), 4321),
                every_process_is_gone
            ),
            "a pid from before this boot is not this host's"
        );
        assert!(
            !a_runtime_here_may_take_this_name_over(
                &here,
                &held_by(HostIdentity::Unidentified, 4321),
                every_process_is_gone
            ),
            "a holder that reports no host is never this host"
        );
        assert!(
            !a_runtime_here_may_take_this_name_over(
                &HostIdentity::Unidentified,
                &held_by(HostIdentity::Unidentified, 4321),
                every_process_is_gone
            ),
            "a platform that recognises no host takes no name over, macOS included"
        );
    }

    /// The pid the probe is asked about is the holder's, never this process's —
    /// the mistake that would free every name on a busy host.
    #[test]
    fn the_process_probe_is_asked_about_the_holders_own_pid() {
        let here = identified("this-boot", 4_026_531_836);
        let asked_about = std::cell::Cell::new(None);

        a_runtime_here_may_take_this_name_over(&here, &held_by(here.clone(), 4321), |process_id| {
            asked_about.set(Some(process_id));
            true
        });

        assert_eq!(asked_about.get(), Some(4321));
    }

    /// The refusal has to be actionable from the message alone: the name, where
    /// the holder is, its pid, and both ways out.
    #[test]
    fn the_refusal_names_the_name_the_holders_host_and_pid_and_both_fixes() {
        let here = identified("this-boot", 4_026_531_836);
        let refusal = why_this_name_is_not_available(
            &a_mesh_name("lab"),
            &here,
            &held_by(here.clone(), 4321),
        );

        assert!(refusal.contains("rig-desk-a1b2"), "{refusal}");
        assert!(refusal.contains("lab"), "{refusal}");
        assert!(refusal.contains("this host"), "{refusal}");
        assert!(refusal.contains("4321"), "{refusal}");
        assert!(refusal.contains("streamlib nodes"), "{refusal}");
        assert!(refusal.contains("--runtime-name"), "{refusal}");
        assert!(refusal.contains("STREAMLIB_RUNTIME_NAME"), "{refusal}");
        assert!(refusal.contains("runtime_name="), "{refusal}");
    }

    /// A holder elsewhere is named as elsewhere, so the reader knows `streamlib
    /// nodes` on this machine will not list it.
    #[test]
    fn a_holder_on_another_host_is_named_as_another_host() {
        let here = identified("this-boot", 4_026_531_836);

        assert_eq!(where_the_holder_is(&here, &here), "a process on this host");
        assert_eq!(
            where_the_holder_is(&here, &identified("another-boot", 7)),
            "another host (kernel boot another-boot, pid namespace 7)"
        );
        assert_eq!(
            where_the_holder_is(&here, &HostIdentity::Unidentified),
            "a host that reports no identity"
        );
        assert_eq!(
            where_the_holder_is(&HostIdentity::Unidentified, &HostIdentity::Unidentified),
            "a host that reports no identity",
            "a platform that recognises no host must not call a peer its own"
        );
    }
}
