// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The name this machine is addressed by, the first part of
//! `<machine>/<stream>/<node>/<port>`.
//!
//! On a tailnet it is the machine's tailnet name, read from the local
//! Tailscale's status as the runtime starts; with no Tailscale it is the
//! hostname. Either is cast the way every exposed name is. Nothing is claimed,
//! suffixed or recorded: Tailscale keeps tailnet names unique, and a hostname
//! matters only on its own machine.

use std::fmt;
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde::Deserialize;

use crate::core::error::Result;
use crate::core::graph::cast_exposed_name_to_url_safe;

/// How long a starting runtime waits on the local Tailscale's status before
/// naming the machine by its hostname instead.
const TAILSCALE_STATUS_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// The Tailscale CLI as `PATH` finds it.
const TAILSCALE_CLI_ON_THE_PATH: &str = "tailscale";

/// Where the Tailscale app for macOS keeps its CLI, which it does not put on
/// `PATH`.
#[cfg(target_os = "macos")]
const TAILSCALE_CLI_IN_THE_MACOS_APP: &str = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";

/// The arguments asking for this machine's own status alone: the tailnet's
/// other machines are the sharing step's to read.
const TAILSCALE_STATUS_OF_THIS_MACHINE_ALONE_ARGUMENTS: [&str; 3] =
    ["status", "--json", "--peers=false"];

/// The backend state of a Tailscale that is up on its tailnet.
const TAILSCALE_BACKEND_STATE_RUNNING: &str = "Running";

/// The separator between the labels of a DNS name.
const DNS_LABEL_SEPARATOR: char = '.';

/// What names a machine whose host reports no usable name.
const MACHINE_NAME_FOR_A_HOST_REPORTING_NO_USABLE_NAME: &str = "unnamed-machine";

/// The most bytes this reads a hostname into. `HOST_NAME_MAX` is 64 on Linux
/// and 255 on Apple; POSIX allows a longer name to be truncated.
const HOST_NAME_BUFFER_BYTES: usize = 256;

/// The name this machine is addressed by, cast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineName {
    /// The machine's name: its tailnet name's first label, or its hostname.
    name: String,
    /// The machine's full tailnet name, when it is on a tailnet.
    tailnet_full_name: Option<String>,
}

impl MachineName {
    /// A machine on a tailnet, named by its tailnet DNS name
    /// (`desk.tail2950db.ts.net`, a trailing dot allowed): the first label is
    /// its name and the whole is its full tailnet name, each cast.
    pub fn from_a_tailnet_dns_name(tailnet_dns_name: &str) -> Result<Self> {
        let tailnet_full_name = tailnet_dns_name.trim_end_matches(DNS_LABEL_SEPARATOR);
        let first_label = tailnet_full_name
            .split(DNS_LABEL_SEPARATOR)
            .next()
            .unwrap_or_default();
        Ok(Self {
            name: cast_exposed_name_to_url_safe(first_label)?.into_owned(),
            tailnet_full_name: Some(cast_exposed_name_to_url_safe(tailnet_full_name)?.into_owned()),
        })
    }

    /// A machine with no tailnet, named by its hostname, cast.
    pub fn from_a_hostname(hostname: &str) -> Result<Self> {
        Ok(Self {
            name: cast_exposed_name_to_url_safe(hostname)?.into_owned(),
            tailnet_full_name: None,
        })
    }

    /// The machine's name: the first part of a four-part address.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The machine's full tailnet name, `None` with no tailnet.
    pub fn tailnet_full_name(&self) -> Option<&str> {
        self.tailnet_full_name.as_deref()
    }

    /// Whether `machine`, once cast, names this machine: its name, or its full
    /// tailnet name.
    pub fn names_this_machine(&self, machine: &str) -> bool {
        cast_exposed_name_to_url_safe(machine).is_ok_and(|machine_cast| {
            *machine_cast == self.name
                || self.tailnet_full_name.as_deref() == Some(machine_cast.as_ref())
        })
    }

    /// This machine's name, read once as the runtime starts and said once.
    pub(crate) fn of_this_machine() -> Self {
        resolve_the_machine_name(
            this_machines_tailnet_dns_name_from_the_local_tailscale(),
            this_hosts_name(),
        )
        .say_once_how_this_machine_was_named()
    }
}

impl fmt::Display for MachineName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

/// How this machine came by its name, for the one line that says so.
#[derive(Debug, PartialEq, Eq)]
enum HowThisMachineWasNamed {
    FromItsTailnetName,
    FromItsHostname {
        why_tailscale_named_no_machine: String,
    },
    AsTheStandInForAnUnusableHostname {
        why_tailscale_named_no_machine: String,
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
            _ => tracing::info!("{}", self.how_it_was_named_said_once()),
        }
        self.machine_name
    }

    fn how_it_was_named_said_once(&self) -> String {
        let name = &self.machine_name.name;
        match &self.how_it_was_named {
            HowThisMachineWasNamed::FromItsTailnetName => format!(
                "this machine is named `{name}`, its tailnet name `{}` as the local Tailscale's \
                 status gives it",
                self.machine_name
                    .tailnet_full_name()
                    .unwrap_or(name.as_str())
            ),
            HowThisMachineWasNamed::FromItsHostname {
                why_tailscale_named_no_machine,
            } => format!(
                "this machine is named `{name}`, its hostname cast, because \
                 {why_tailscale_named_no_machine}"
            ),
            HowThisMachineWasNamed::AsTheStandInForAnUnusableHostname {
                why_tailscale_named_no_machine,
                why_the_hostname_names_nothing,
            } => format!(
                "this machine is named `{name}` in place of a name, because \
                 {why_tailscale_named_no_machine} and {why_the_hostname_names_nothing}"
            ),
        }
    }
}

/// The resolver with every input named, so each arm is testable without
/// reaching Tailscale or the host: the tailnet name when Tailscale gave one
/// that names something, else the hostname, else the stand-in.
fn resolve_the_machine_name(
    tailnet_dns_name: std::result::Result<String, String>,
    host_name: Option<String>,
) -> ResolvedMachineName {
    let why_tailscale_named_no_machine = match tailnet_dns_name {
        Ok(tailnet_dns_name) => match MachineName::from_a_tailnet_dns_name(&tailnet_dns_name) {
            Ok(machine_name) => {
                return ResolvedMachineName {
                    machine_name,
                    how_it_was_named: HowThisMachineWasNamed::FromItsTailnetName,
                };
            }
            Err(names_nothing) => format!(
                "the tailnet name `{tailnet_dns_name}` the local Tailscale's status gives it \
                 names nothing: {names_nothing}"
            ),
        },
        Err(why_not) => why_not,
    };
    let why_the_hostname_names_nothing = match host_name {
        None => "this machine reports no hostname".to_string(),
        Some(host_name) => match MachineName::from_a_hostname(&host_name) {
            Ok(machine_name) => {
                return ResolvedMachineName {
                    machine_name,
                    how_it_was_named: HowThisMachineWasNamed::FromItsHostname {
                        why_tailscale_named_no_machine,
                    },
                };
            }
            Err(names_nothing) => format!("its hostname names nothing: {names_nothing}"),
        },
    };
    ResolvedMachineName {
        machine_name: MachineName {
            name: MACHINE_NAME_FOR_A_HOST_REPORTING_NO_USABLE_NAME.to_string(),
            tailnet_full_name: None,
        },
        how_it_was_named: HowThisMachineWasNamed::AsTheStandInForAnUnusableHostname {
            why_tailscale_named_no_machine,
            why_the_hostname_names_nothing,
        },
    }
}

/// The local Tailscale's status of this machine alone, as its CLI writes it.
#[derive(Deserialize)]
struct TailscaleStatusOfThisMachine {
    #[serde(rename = "BackendState", default)]
    backend_state: String,
    #[serde(rename = "Self")]
    this_machine: Option<TailscaleStatusOfOneMachine>,
}

/// One machine in a Tailscale status.
#[derive(Deserialize)]
struct TailscaleStatusOfOneMachine {
    #[serde(rename = "DNSName", default)]
    dns_name: String,
}

/// This machine's tailnet DNS name from a status the Tailscale CLI wrote, or
/// why the status gives none.
fn tailnet_dns_name_from_the_status(status_json: &[u8]) -> std::result::Result<String, String> {
    let status: TailscaleStatusOfThisMachine =
        serde_json::from_slice(status_json).map_err(|unreadable| {
            format!("the local Tailscale's status is not the JSON its CLI writes: {unreadable}")
        })?;
    if status.backend_state != TAILSCALE_BACKEND_STATE_RUNNING {
        return Err(format!(
            "the local Tailscale is `{}` rather than `{TAILSCALE_BACKEND_STATE_RUNNING}` on a \
             tailnet",
            status.backend_state
        ));
    }
    let Some(this_machine) = status.this_machine else {
        return Err("the local Tailscale's status names no machine of its own".to_string());
    };
    if this_machine
        .dns_name
        .trim_end_matches(DNS_LABEL_SEPARATOR)
        .is_empty()
    {
        return Err("the local Tailscale's status gives this machine no DNS name".to_string());
    }
    Ok(this_machine.dns_name)
}

/// Why the local Tailscale CLI wrote no status.
#[derive(Debug)]
enum WhyTheTailscaleStatusWasNotRead {
    NoSuchProgram,
    CouldNotStart(std::io::Error),
    TimedOut(Duration),
    ReadFailed(String),
    Exited(ExitStatus),
}

impl fmt::Display for WhyTheTailscaleStatusWasNotRead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchProgram => formatter.write_str("it is not installed"),
            Self::CouldNotStart(why) => write!(formatter, "it could not start: {why}"),
            Self::TimedOut(timeout) => write!(formatter, "it wrote no status within {timeout:?}"),
            Self::ReadFailed(why) => write!(formatter, "its status could not be read: {why}"),
            Self::Exited(exit_status) => write!(formatter, "it ended with {exit_status}"),
        }
    }
}

/// Every place this platform's Tailscale CLI may be, in the order tried.
fn tailscale_cli_candidates() -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        &[TAILSCALE_CLI_ON_THE_PATH, TAILSCALE_CLI_IN_THE_MACOS_APP]
    }
    #[cfg(not(target_os = "macos"))]
    {
        &[TAILSCALE_CLI_ON_THE_PATH]
    }
}

/// This machine's tailnet DNS name from the first Tailscale CLI found, or why
/// none gave one.
fn this_machines_tailnet_dns_name_from_the_local_tailscale() -> std::result::Result<String, String>
{
    for tailscale_cli in tailscale_cli_candidates() {
        match read_the_tailscale_status_of_this_machine_alone(
            Path::new(tailscale_cli),
            TAILSCALE_STATUS_READ_TIMEOUT,
        ) {
            Ok(status_json) => return tailnet_dns_name_from_the_status(&status_json),
            Err(WhyTheTailscaleStatusWasNotRead::NoSuchProgram) => continue,
            Err(why_not) => {
                return Err(format!(
                    "the Tailscale CLI `{tailscale_cli}` wrote no status: {why_not}"
                ));
            }
        }
    }
    Err(format!(
        "no Tailscale CLI is installed (looked for {})",
        tailscale_cli_candidates()
            .iter()
            .map(|tailscale_cli| format!("`{tailscale_cli}`"))
            .collect::<Vec<_>>()
            .join(" and ")
    ))
}

/// Run `tailscale_cli status --json --peers=false` and read what it writes,
/// ending it once `timeout` passes without its status.
fn read_the_tailscale_status_of_this_machine_alone(
    tailscale_cli: &Path,
    timeout: Duration,
) -> std::result::Result<Vec<u8>, WhyTheTailscaleStatusWasNotRead> {
    let mut tailscale_process = match Command::new(tailscale_cli)
        .args(TAILSCALE_STATUS_OF_THIS_MACHINE_ALONE_ARGUMENTS)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(tailscale_process) => tailscale_process,
        Err(not_started) if not_started.kind() == std::io::ErrorKind::NotFound => {
            return Err(WhyTheTailscaleStatusWasNotRead::NoSuchProgram);
        }
        Err(not_started) => {
            return Err(WhyTheTailscaleStatusWasNotRead::CouldNotStart(not_started));
        }
    };
    let end_the_tailscale_process = |mut tailscale_process: std::process::Child| {
        let _ = tailscale_process.kill();
        let _ = tailscale_process.wait();
    };
    let Some(mut tailscale_standard_output) = tailscale_process.stdout.take() else {
        end_the_tailscale_process(tailscale_process);
        return Err(WhyTheTailscaleStatusWasNotRead::ReadFailed(
            "its standard output was not piped".to_string(),
        ));
    };
    let (status_bytes_sender, status_bytes_receiver) = mpsc::channel();
    let spawned_reader = std::thread::Builder::new()
        .name("tailscale-status-reader".to_string())
        .spawn(move || {
            let mut status_bytes = Vec::new();
            let read = tailscale_standard_output
                .read_to_end(&mut status_bytes)
                .map(|_| status_bytes);
            let _ = status_bytes_sender.send(read);
        });
    if let Err(not_spawned) = spawned_reader {
        end_the_tailscale_process(tailscale_process);
        return Err(WhyTheTailscaleStatusWasNotRead::CouldNotStart(not_spawned));
    }
    match status_bytes_receiver.recv_timeout(timeout) {
        Ok(Ok(status_bytes)) => match tailscale_process.wait() {
            Ok(exit_status) if exit_status.success() => Ok(status_bytes),
            Ok(exit_status) => Err(WhyTheTailscaleStatusWasNotRead::Exited(exit_status)),
            Err(not_waited) => Err(WhyTheTailscaleStatusWasNotRead::ReadFailed(
                not_waited.to_string(),
            )),
        },
        Ok(Err(not_read)) => {
            end_the_tailscale_process(tailscale_process);
            Err(WhyTheTailscaleStatusWasNotRead::ReadFailed(
                not_read.to_string(),
            ))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            end_the_tailscale_process(tailscale_process);
            Err(WhyTheTailscaleStatusWasNotRead::TimedOut(timeout))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            end_the_tailscale_process(tailscale_process);
            Err(WhyTheTailscaleStatusWasNotRead::ReadFailed(
                "its reader ended without the status".to_string(),
            ))
        }
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
    use std::os::unix::fs::PermissionsExt;

    fn status_json(backend_state: &str, this_machine: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "Version": "1.102.4",
            "BackendState": backend_state,
            "Self": this_machine,
        }))
        .expect("a status serializes")
    }

    fn resolved_from_the_status(status: &[u8], host_name: Option<&str>) -> ResolvedMachineName {
        resolve_the_machine_name(
            tailnet_dns_name_from_the_status(status),
            host_name.map(str::to_string),
        )
    }

    /// A running Tailscale names the machine by its DNS name's first label,
    /// and the whole name, without its trailing dot, is its full tailnet name.
    #[test]
    fn a_running_tailscale_names_the_machine_by_its_tailnet_name() {
        let resolved = resolved_from_the_status(
            &status_json(
                "Running",
                serde_json::json!({"HostName": "Desk", "DNSName": "desk.tail2950db.ts.net."}),
            ),
            Some("some-other-hostname"),
        );
        assert_eq!(
            resolved.how_it_was_named,
            HowThisMachineWasNamed::FromItsTailnetName
        );
        assert_eq!(resolved.machine_name.name(), "desk");
        assert_eq!(
            resolved.machine_name.tailnet_full_name(),
            Some("desk.tail2950db.ts.net")
        );
    }

    /// A DNS name Tailscale spells in capitals is cast like every other name.
    #[test]
    fn an_uppercase_dns_name_is_cast() {
        let resolved = resolved_from_the_status(
            &status_json(
                "Running",
                serde_json::json!({"DNSName": "Jonathans-MacBook.Tail2950db.TS.net."}),
            ),
            None,
        );
        assert_eq!(resolved.machine_name.name(), "jonathans-macbook");
        assert_eq!(
            resolved.machine_name.tailnet_full_name(),
            Some("jonathans-macbook.tail2950db.ts.net")
        );
    }

    /// Every status that is not a running Tailscale with a DNS name of its own
    /// leaves the machine named by its hostname, cast, saying why.
    #[test]
    fn a_status_that_names_no_tailnet_machine_leaves_the_hostname_saying_why() {
        for (status, why) in [
            (
                status_json("Stopped", serde_json::json!({"DNSName": "desk.ts.net."})),
                "`Stopped`",
            ),
            (
                status_json("NeedsLogin", serde_json::json!({"DNSName": ""})),
                "`NeedsLogin`",
            ),
            (
                status_json("Running", serde_json::Value::Null),
                "names no machine of its own",
            ),
            (
                serde_json::to_vec(&serde_json::json!({"BackendState": "Running"})).unwrap(),
                "names no machine of its own",
            ),
            (
                status_json("Running", serde_json::json!({"DNSName": ""})),
                "no DNS name",
            ),
            (
                status_json("Running", serde_json::json!({"HostName": "desk"})),
                "no DNS name",
            ),
            (b"tailscaled is not running".to_vec(), "not the JSON"),
        ] {
            let resolved = resolved_from_the_status(&status, Some("Jonathans-MacBook"));
            assert_eq!(resolved.machine_name.name(), "jonathans-macbook");
            assert_eq!(resolved.machine_name.tailnet_full_name(), None);
            let HowThisMachineWasNamed::FromItsHostname {
                why_tailscale_named_no_machine,
            } = &resolved.how_it_was_named
            else {
                panic!("named by the hostname: {:?}", resolved.how_it_was_named);
            };
            assert!(
                why_tailscale_named_no_machine.contains(why),
                "{why:?} not in {why_tailscale_named_no_machine:?}"
            );
            let said = resolved.how_it_was_named_said_once();
            assert!(
                said.contains("`jonathans-macbook`") && said.contains("hostname"),
                "{said}"
            );
        }
    }

    /// No Tailscale at all names the machine by its hostname too.
    #[test]
    fn no_tailscale_names_the_machine_by_its_hostname() {
        let resolved = resolve_the_machine_name(
            Err("no Tailscale CLI is installed".to_string()),
            Some("rig".to_string()),
        );
        assert_eq!(
            resolved.machine_name,
            MachineName::from_a_hostname("rig").unwrap()
        );
    }

    /// A host reporting no usable name takes the stand-in, said once by
    /// warning naming why; a usable one says nothing at warning.
    #[test]
    fn a_host_reporting_no_usable_name_takes_the_stand_in_said_once_by_warning() {
        for host_name in [None, Some("!!!")] {
            let (machine_name, warnings) = CapturedTracingWarnings::captured_while(|| {
                resolve_the_machine_name(
                    Err("no Tailscale CLI is installed".to_string()),
                    host_name.map(str::to_string),
                )
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
                    && warning.contains("no Tailscale CLI is installed"),
                "{warning}"
            );
        }

        let (_, warnings) = CapturedTracingWarnings::captured_while(|| {
            resolve_the_machine_name(Err("stopped".to_string()), Some("rig".to_string()))
                .say_once_how_this_machine_was_named()
        });
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// A machine is named by its name or its full tailnet name, each spelled
    /// any way that casts to it, and by nothing else.
    #[test]
    fn a_machine_is_named_by_its_name_or_its_full_tailnet_name() {
        let on_a_tailnet = MachineName::from_a_tailnet_dns_name("desk.tail2950db.ts.net.")
            .expect("a tailnet name names something");
        for naming_it in [
            "desk",
            "Desk",
            "desk.tail2950db.ts.net",
            "DESK.tail2950db.ts.net",
        ] {
            assert!(on_a_tailnet.names_this_machine(naming_it), "{naming_it}");
        }
        for not_naming_it in ["rig", "desk.tail0000.ts.net", "desk.tail2950db", "", "!!!"] {
            assert!(
                !on_a_tailnet.names_this_machine(not_naming_it),
                "{not_naming_it}"
            );
        }

        let off_a_tailnet = MachineName::from_a_hostname("Jonathans-MacBook").unwrap();
        assert!(off_a_tailnet.names_this_machine("jonathans-macbook"));
        assert!(!off_a_tailnet.names_this_machine("desk"));
        assert_eq!(off_a_tailnet.tailnet_full_name(), None);
        assert_eq!(off_a_tailnet.to_string(), "jonathans-macbook");
    }

    fn a_stub_tailscale_cli(directory: &Path, script_body: &str) -> std::path::PathBuf {
        let stub = directory.join("tailscale");
        std::fs::write(&stub, format!("#!/bin/sh\n{script_body}\n")).expect("write the stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o700))
            .expect("make the stub executable");
        stub
    }

    /// The CLI is asked for this machine's status alone, and what it writes is
    /// read whole.
    #[test]
    fn the_cli_is_asked_for_this_machines_status_alone_and_read_whole() {
        let directory = tempfile::tempdir().expect("tempdir");
        let stub = a_stub_tailscale_cli(
            directory.path(),
            r#"[ "$*" = "status --json --peers=false" ] || exit 3
printf '{"BackendState":"Running","Self":{"DNSName":"desk.tail2950db.ts.net."}}'"#,
        );
        let status =
            read_the_tailscale_status_of_this_machine_alone(&stub, Duration::from_secs(10))
                .expect("the stub writes its status");
        assert_eq!(
            tailnet_dns_name_from_the_status(&status).as_deref(),
            Ok("desk.tail2950db.ts.net.")
        );
    }

    /// A CLI that fails, one that is not there, and one that never answers each
    /// leave the status unread, the last ended at the timeout.
    #[test]
    fn a_cli_that_fails_is_missing_or_never_answers_writes_no_status() {
        let directory = tempfile::tempdir().expect("tempdir");
        let failing = a_stub_tailscale_cli(directory.path(), "exit 1");
        assert!(matches!(
            read_the_tailscale_status_of_this_machine_alone(&failing, Duration::from_secs(10)),
            Err(WhyTheTailscaleStatusWasNotRead::Exited(_))
        ));

        assert!(matches!(
            read_the_tailscale_status_of_this_machine_alone(
                &directory.path().join("no-such-tailscale"),
                Duration::from_secs(10)
            ),
            Err(WhyTheTailscaleStatusWasNotRead::NoSuchProgram)
        ));

        let never_answering = directory.path().join("never-answering");
        std::fs::create_dir(&never_answering).expect("a directory for the second stub");
        let never_answering = a_stub_tailscale_cli(&never_answering, "exec sleep 600");
        let started = std::time::Instant::now();
        assert!(matches!(
            read_the_tailscale_status_of_this_machine_alone(
                &never_answering,
                Duration::from_millis(200)
            ),
            Err(WhyTheTailscaleStatusWasNotRead::TimedOut(_))
        ));
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the read gave up at its timeout rather than waiting the CLI out"
        );
    }

    /// This machine's own hostname names it.
    #[test]
    fn this_machines_hostname_names_it() {
        let host_name = this_hosts_name().expect("the test host reports a hostname");
        MachineName::from_a_hostname(&host_name).expect("the test host's hostname names it");
    }
}
