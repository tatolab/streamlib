// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The name this machine is addressed by, the first part of
//! `<machine>/<stream id>/<node>/<port>`: its hostname, cast the way every
//! exposed name is. Nothing is claimed, suffixed or recorded: a hostname
//! matters on its own machine, and a relay's root names the machine there.

use std::fmt;

use crate::core::error::Result;
use crate::core::graph::cast_exposed_name_to_url_safe;

/// The mDNS domain macOS appends to a hostname, matched case-insensitively.
const MDNS_LOCAL_DOMAIN_SUFFIX: &str = ".local";

/// What names a machine whose host reports no usable name.
const MACHINE_NAME_FOR_A_HOST_REPORTING_NO_USABLE_NAME: &str = "unnamed-machine";

/// The most bytes this reads a hostname into. `HOST_NAME_MAX` is 64 on Linux
/// and 255 on Apple; POSIX allows a longer name to be truncated.
const HOST_NAME_BUFFER_BYTES: usize = 256;

/// The name this machine is addressed by, cast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineName {
    name: String,
}

impl MachineName {
    /// A machine named by its hostname cast, with a trailing mDNS `.local`
    /// dropped first.
    pub fn from_a_hostname(hostname: &str) -> Result<Self> {
        Ok(Self {
            name: cast_exposed_name_to_url_safe(hostname_without_the_mdns_local_domain(hostname))?
                .into_owned(),
        })
    }

    /// The machine's name: the first part of a four-part address.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether `machine`, once cast, names this machine.
    pub fn names_this_machine(&self, machine: &str) -> bool {
        cast_exposed_name_to_url_safe(machine).is_ok_and(|machine_cast| *machine_cast == self.name)
    }

    /// This machine's name, read once as the runtime starts and said once.
    pub(crate) fn of_this_machine() -> Self {
        resolve_the_machine_name(this_hosts_name()).say_once_how_this_machine_was_named()
    }
}

impl fmt::Display for MachineName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

/// `hostname` with its trailing mDNS `.local`, in any case, dropped; every
/// other hostname as it is.
fn hostname_without_the_mdns_local_domain(hostname: &str) -> &str {
    hostname
        .len()
        .checked_sub(MDNS_LOCAL_DOMAIN_SUFFIX.len())
        .filter(|suffix_start| {
            hostname
                .get(*suffix_start..)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(MDNS_LOCAL_DOMAIN_SUFFIX))
        })
        .map_or(hostname, |suffix_start| &hostname[..suffix_start])
}

/// How this machine came by its name, for the one line that says so.
#[derive(Debug, PartialEq, Eq)]
enum HowThisMachineWasNamed {
    FromItsHostname,
    AsTheStandInForAnUnusableHostname {
        why_the_hostname_names_nothing: String,
    },
}

/// A machine's name and how it came by it.
#[must_use]
#[derive(Debug)]
struct ResolvedMachineName {
    machine_name: MachineName,
    how_it_was_named: HowThisMachineWasNamed,
}

impl ResolvedMachineName {
    /// The name, said at info — or, for the stand-in, at warning — once.
    fn say_once_how_this_machine_was_named(self) -> MachineName {
        match &self.how_it_was_named {
            HowThisMachineWasNamed::AsTheStandInForAnUnusableHostname { .. } => {
                tracing::warn!("{}", self.how_it_was_named_said_once())
            }
            HowThisMachineWasNamed::FromItsHostname => {
                tracing::info!("{}", self.how_it_was_named_said_once())
            }
        }
        self.machine_name
    }

    fn how_it_was_named_said_once(&self) -> String {
        let name = &self.machine_name.name;
        match &self.how_it_was_named {
            HowThisMachineWasNamed::FromItsHostname => {
                format!("this machine is named `{name}`, its hostname cast")
            }
            HowThisMachineWasNamed::AsTheStandInForAnUnusableHostname {
                why_the_hostname_names_nothing,
            } => format!(
                "this machine is named `{name}` in place of a name, because \
                 {why_the_hostname_names_nothing}"
            ),
        }
    }
}

/// The resolver with the host's name passed in, so each arm is testable
/// without reaching the host: the hostname, else the stand-in.
fn resolve_the_machine_name(host_name: Option<String>) -> ResolvedMachineName {
    let why_the_hostname_names_nothing = match host_name {
        None => "this machine reports no hostname".to_string(),
        Some(host_name) => match MachineName::from_a_hostname(&host_name) {
            Ok(machine_name) => {
                return ResolvedMachineName {
                    machine_name,
                    how_it_was_named: HowThisMachineWasNamed::FromItsHostname,
                };
            }
            Err(names_nothing) => format!("its hostname names nothing: {names_nothing}"),
        },
    };
    ResolvedMachineName {
        machine_name: MachineName {
            name: MACHINE_NAME_FOR_A_HOST_REPORTING_NO_USABLE_NAME.to_string(),
        },
        how_it_was_named: HowThisMachineWasNamed::AsTheStandInForAnUnusableHostname {
            why_the_hostname_names_nothing,
        },
    }
}

/// This machine's hostname, or `None` when it reports none.
fn this_hosts_name() -> Option<String> {
    let mut buffer = vec![0u8; HOST_NAME_BUFFER_BYTES];
    // SAFETY: the buffer is `HOST_NAME_BUFFER_BYTES` long and that is the length
    // passed; `gethostname` writes at most that many bytes.
    let reported = unsafe {
        libc::gethostname(
            buffer.as_mut_ptr() as *mut libc::c_char,
            HOST_NAME_BUFFER_BYTES,
        )
    };
    // POSIX leaves truncation unterminated, so the name ends at the first NUL
    // or at the buffer's end, whichever comes first.
    let host_name = buffer
        .iter()
        .position(|&byte| byte == 0)
        .map(|end| String::from_utf8_lossy(&buffer[..end]).into_owned())
        .unwrap_or_else(|| String::from_utf8_lossy(&buffer).into_owned());

    (reported == 0 && !host_name.is_empty()).then_some(host_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::CapturedTracingWarnings;

    /// A usable hostname names the machine, cast, said once at info.
    #[test]
    fn a_hostname_names_the_machine_cast() {
        let resolved = resolve_the_machine_name(Some("Jonathans-MacBook".to_string()));
        assert_eq!(
            resolved.how_it_was_named,
            HowThisMachineWasNamed::FromItsHostname
        );
        assert_eq!(resolved.machine_name.name(), "jonathans-macbook");
        let said = resolved.how_it_was_named_said_once();
        assert!(
            said.contains("`jonathans-macbook`") && said.contains("hostname"),
            "{said}"
        );
    }

    /// A hostname's trailing mDNS `.local`, in any case, is dropped before the
    /// cast; every other dotted hostname keeps its dots.
    #[test]
    fn a_hostname_drops_its_trailing_mdns_local_domain_and_keeps_every_other_dot() {
        for (hostname, machine_name) in [
            ("Jonathans-MacBook.local", "jonathans-macbook"),
            ("rig.LOCAL", "rig"),
            ("rig.Local", "rig"),
            ("rig.local.lan", "rig.local.lan"),
            ("build.example.com", "build.example.com"),
            ("rig.localhost", "rig.localhost"),
            ("local", "local"),
            ("rig", "rig"),
        ] {
            assert_eq!(
                MachineName::from_a_hostname(hostname)
                    .expect("the hostname names a machine")
                    .name(),
                machine_name,
                "{hostname}"
            );
        }
        assert!(
            MachineName::from_a_hostname(".local").is_err(),
            "a hostname that is the mDNS domain alone names nothing"
        );
    }

    /// A host reporting no usable name takes the stand-in, said once by
    /// warning naming why; a usable one says nothing at warning.
    #[test]
    fn a_host_reporting_no_usable_name_takes_the_stand_in_said_once_by_warning() {
        for (host_name, why) in [
            (None, "reports no hostname"),
            (Some("!!!"), "its hostname names nothing"),
        ] {
            let (machine_name, warnings) = CapturedTracingWarnings::captured_while(|| {
                resolve_the_machine_name(host_name.map(str::to_string))
                    .say_once_how_this_machine_was_named()
            });
            assert_eq!(
                machine_name.name(),
                MACHINE_NAME_FOR_A_HOST_REPORTING_NO_USABLE_NAME
            );
            let [warning] = warnings.as_slice() else {
                panic!("the stand-in is said in exactly one warning; got {warnings:?}");
            };
            assert!(
                warning.contains(MACHINE_NAME_FOR_A_HOST_REPORTING_NO_USABLE_NAME)
                    && warning.contains(why),
                "{warning}"
            );
        }

        let (_, warnings) = CapturedTracingWarnings::captured_while(|| {
            resolve_the_machine_name(Some("rig".to_string())).say_once_how_this_machine_was_named()
        });
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// A machine is named by its name spelled any way that casts to it, and by
    /// nothing else.
    #[test]
    fn a_machine_is_named_by_its_name_spelled_any_way_that_casts_to_it() {
        let machine_name = MachineName::from_a_hostname("Jonathans-MacBook").unwrap();
        for naming_it in [
            "jonathans-macbook",
            "Jonathans-MacBook",
            "JONATHANS MACBOOK",
        ] {
            assert!(machine_name.names_this_machine(naming_it), "{naming_it}");
        }
        for not_naming_it in ["desk", "jonathans-macbook.tail2950db.ts.net", "", "!!!"] {
            assert!(
                !machine_name.names_this_machine(not_naming_it),
                "{not_naming_it}"
            );
        }
        assert_eq!(machine_name.to_string(), "jonathans-macbook");
    }

    /// This machine's own hostname names it.
    #[test]
    fn this_machines_hostname_names_it() {
        let host_name = this_hosts_name().expect("the test host reports a hostname");
        MachineName::from_a_hostname(&host_name).expect("the test host's hostname names it");
    }
}
