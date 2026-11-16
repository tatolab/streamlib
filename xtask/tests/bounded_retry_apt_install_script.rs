// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The bounded-retry apt install script's own behaviour. These drive the real
//! script with `STREAMLIB_APT_GET_COMMAND` pointed at a fixture, so they need
//! no root, no apt and no network, and prove the timeout path actually fires
//! rather than a green run that happened to get a fast mirror.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const BOUNDED_RETRY_SCRIPT_RELATIVE_PATH: &str = ".github/actions/install-linux-engine-build-dependencies/install-system-dependencies-with-bounded-retry.sh";

fn workspace_root_above_the_xtask_manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ always has a workspace root above it")
        .to_path_buf()
}

/// Drives the real script against a fake `apt-get` that logs every
/// subcommand it is handed, so a test can assert what ran and in what order.
struct BoundedRetryScriptHarness {
    _temp: TempDir,
    script: PathBuf,
    apt_get_fixture: PathBuf,
    mirror_switch_fixture: PathBuf,
    dpkg_repair_fixture: PathBuf,
    privilege_prefix_fixture: PathBuf,
    invocation_log: PathBuf,
}

impl BoundedRetryScriptHarness {
    fn new(apt_get_fixture_body: &str) -> Self {
        let temp = TempDir::new().unwrap();

        let invocation_log = temp.path().join("invocations.log");
        fs::write(&invocation_log, "").unwrap();

        Self {
            script: workspace_root_above_the_xtask_manifest()
                .join(BOUNDED_RETRY_SCRIPT_RELATIVE_PATH),
            apt_get_fixture: write_executable_fixture(
                temp.path().join("apt-get-fixture"),
                apt_get_fixture_body,
            ),
            mirror_switch_fixture: write_executable_fixture(
                temp.path().join("mirror-switch-fixture"),
                "#!/usr/bin/env bash\nprintf 'switched %s\\n' \"$1\" \
                     >> \"$STREAMLIB_APT_FIXTURE_INVOCATION_LOG\"\n",
            ),
            dpkg_repair_fixture: write_executable_fixture(
                temp.path().join("dpkg-repair-fixture"),
                "#!/usr/bin/env bash\nprintf 'dpkg-repaired\\n' \
                     >> \"$STREAMLIB_APT_FIXTURE_INVOCATION_LOG\"\n",
            ),
            privilege_prefix_fixture: write_executable_fixture(
                temp.path().join("privilege-prefix-fixture"),
                "#!/usr/bin/env bash\nprintf 'privileged %s\\n' \"$*\" \
                     >> \"$STREAMLIB_APT_FIXTURE_INVOCATION_LOG\"\nexec \"$@\"\n",
            ),
            _temp: temp,
            invocation_log,
        }
    }

    /// Runs with the privilege prefix in play instead of erased, so the
    /// `sudo timeout` word order is observable. Every other test empties the
    /// prefix, and with it empty both orderings emit identical argv.
    fn run_recording_the_privileged_argv(&self) -> BoundedRetryScriptRun {
        self.run_with_environment(&[
            (
                "STREAMLIB_APT_PRIVILEGE_PREFIX",
                self.privilege_prefix_fixture.as_os_str(),
            ),
            ("STREAMLIB_APT_ATTEMPT_TIMEOUT_SECONDS", "5".as_ref()),
        ])
    }

    fn run_with_attempt_bound(&self, attempt_timeout_seconds: u64) -> BoundedRetryScriptRun {
        self.run_with_environment(&[
            ("STREAMLIB_APT_PRIVILEGE_PREFIX", "".as_ref()),
            (
                "STREAMLIB_APT_ATTEMPT_TIMEOUT_SECONDS",
                attempt_timeout_seconds.to_string().as_ref(),
            ),
        ])
    }

    fn run_with_environment(
        &self,
        overrides: &[(&str, &std::ffi::OsStr)],
    ) -> BoundedRetryScriptRun {
        let mut command = Command::new(&self.script);
        command
            .arg("protobuf-compiler")
            .env("STREAMLIB_APT_GET_COMMAND", &self.apt_get_fixture)
            .env(
                "STREAMLIB_APT_MIRROR_SWITCH_COMMAND",
                &self.mirror_switch_fixture,
            )
            .env("STREAMLIB_DPKG_REPAIR_COMMAND", &self.dpkg_repair_fixture)
            .env("STREAMLIB_APT_FIXTURE_INVOCATION_LOG", &self.invocation_log);

        for (name, value) in overrides {
            command.env(name, value);
        }

        let result = command
            .output()
            .expect("the bounded-retry script must be executable");

        BoundedRetryScriptRun {
            succeeded: result.status.success(),
            invocation_log: fs::read_to_string(&self.invocation_log).unwrap(),
            stderr: String::from_utf8_lossy(&result.stderr).into_owned(),
        }
    }
}

impl BoundedRetryScriptHarness {
    /// A `.deb` cache directory beside the fixtures, holding the given
    /// file names, which the script only ever passes to the apt fixture.
    fn deb_cache_directory_holding(&self, deb_file_names: &[&str]) -> PathBuf {
        let deb_cache_directory = self._temp.path().join("deb-cache");
        fs::create_dir_all(&deb_cache_directory).unwrap();
        for deb_file_name in deb_file_names {
            fs::write(deb_cache_directory.join(deb_file_name), "").unwrap();
        }
        deb_cache_directory
    }

    fn run_with_deb_cache_directory(&self, deb_cache_directory: &Path) -> BoundedRetryScriptRun {
        self.run_with_environment(&[
            ("STREAMLIB_APT_PRIVILEGE_PREFIX", "".as_ref()),
            ("STREAMLIB_APT_ATTEMPT_TIMEOUT_SECONDS", "5".as_ref()),
            (
                "STREAMLIB_APT_DEB_CACHE_DIRECTORY",
                deb_cache_directory.as_os_str(),
            ),
        ])
    }
}

struct BoundedRetryScriptRun {
    succeeded: bool,
    invocation_log: String,
    stderr: String,
}

fn write_executable_fixture(path: PathBuf, body: &str) -> PathBuf {
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Logs the whole argv, not just the subcommand: the `Acquire::*` options
/// are part of the contract, so a fixture that recorded only `$1` could not
/// hold them.
const LOG_THE_WHOLE_INVOCATION: &str =
    "printf '%s\\n' \"$*\" >> \"$STREAMLIB_APT_FIXTURE_INVOCATION_LOG\"";

/// Succeeds on `update`, stalls on `install` until the mirror has been
/// switched — the shape of the measured incident, where `update` finished
/// in 65s (inside the bound) and `install` was the command that ran 12m17s.
///
/// A fixture that stalls on *every* subcommand never reaches the install, so
/// the install-side status handling would go unexercised.
///
/// `exec` so the signal reaches `sleep` itself rather than the shell that
/// spawned it. `timeout` signals the whole process group, so the shell's
/// child dies either way and `Command::output()` sees its pipes close — but
/// that is a property of `timeout`'s default, not of this fixture, and a
/// stray survivor would hold those pipes for the whole sleep.
const STALL_ONLY_ON_THE_INSTALL: &str = "\
        if [ \"$1\" = update ]; then exit 0; fi\n\
        if grep -q switched \"$STREAMLIB_APT_FIXTURE_INVOCATION_LOG\"; then exit 0; fi\n\
        exec sleep 60\n";

/// Exits non-zero immediately on the install — a broken package name, not a
/// slow mirror. The distinction is what the failure message has to get right.
const FAIL_THE_INSTALL_OUTRIGHT: &str = "\
        if [ \"$1\" = update ]; then exit 0; fi\n\
        exit 100\n";

/// Refuses the offline install, as apt does when the cached `.debs` no
/// longer cover what the image needs, and succeeds on the network.
const REFUSE_THE_INSTALL_WITHOUT_THE_NETWORK: &str = "\
        case \" $* \" in *' Acquire::http::Proxy=http://127.0.0.1:9 '*) exit 100 ;; esac\n\
        exit 0\n";

fn apt_fixture(body: &str) -> String {
    format!("#!/usr/bin/env bash\n{LOG_THE_WHOLE_INVOCATION}\n{body}")
}

fn count_of(log: &str, subcommand: &str) -> usize {
    log.lines()
        .filter(|line| line.split_whitespace().next() == Some(subcommand))
        .count()
}

#[test]
fn a_healthy_mirror_installs_without_switching() {
    let harness = BoundedRetryScriptHarness::new(&apt_fixture("exit 0\n"));
    let run = harness.run_with_attempt_bound(5);
    let log = &run.invocation_log;

    assert!(run.succeeded, "a healthy mirror must succeed; log:\n{log}");
    assert_eq!(count_of(log, "update"), 1, "log:\n{log}");
    assert_eq!(count_of(log, "install"), 1, "log:\n{log}");
    assert!(
        !log.contains("switched"),
        "a healthy run must not touch the mirror; log:\n{log}"
    );
}

#[test]
fn every_apt_command_carries_the_retry_and_timeout_options() {
    // These cover the transient per-file failure and the connection that
    // goes silent. The wall-clock bound covers neither.
    let harness = BoundedRetryScriptHarness::new(&apt_fixture("exit 0\n"));
    let run = harness.run_with_attempt_bound(5);

    assert_eq!(
        run.invocation_log.lines().count(),
        2,
        "update and install must both have run, or this asserts over nothing; log:\n{}",
        run.invocation_log
    );
    for invocation in run.invocation_log.lines() {
        for option in [
            "Acquire::Retries=3",
            "Acquire::http::Timeout=30",
            "Acquire::https::Timeout=30",
        ] {
            assert!(
                invocation.contains(option),
                "`{option}` missing from `{invocation}`"
            );
        }
    }
}

#[test]
fn the_bound_runs_under_the_privilege_prefix_not_the_other_way_round() {
    // `timeout sudo apt-get` would put sudo in timeout's child slot, where
    // SIGKILL lands on sudo and leaves an orphaned root apt-get holding
    // /var/lib/dpkg/lock-frontend — so the fallback attempt fails on the
    // lock and the escape hatch means nothing. Both orderings emit identical
    // argv when the prefix is empty, which is why this test supplies one.
    let harness = BoundedRetryScriptHarness::new(&apt_fixture("exit 0\n"));
    let run = harness.run_recording_the_privileged_argv();

    let privileged: Vec<&str> = run
        .invocation_log
        .lines()
        .filter_map(|line| line.strip_prefix("privileged "))
        .collect();

    assert_eq!(privileged.len(), 2, "log:\n{}", run.invocation_log);
    for argv in privileged {
        assert!(
            argv.starts_with("timeout "),
            "the prefix must wrap timeout, not the other way round: `{argv}`"
        );
    }
}

#[test]
fn a_slow_install_trips_the_bound_and_escapes_to_the_fallback() {
    let harness = BoundedRetryScriptHarness::new(&apt_fixture(STALL_ONLY_ON_THE_INSTALL));
    let run = harness.run_with_attempt_bound(2);
    let log = &run.invocation_log;

    assert!(
        log.contains("switched"),
        "the install-side bound must fire and repoint the mirror; log:\n{log}"
    );
    assert!(
        run.succeeded,
        "the fallback mirror must carry the install home; log:\n{log}"
    );
    assert_eq!(
        count_of(log, "install"),
        2,
        "the install must be attempted once per mirror; log:\n{log}"
    );
    assert!(
        run.stderr.contains("did not finish inside the 2s bound"),
        "a stalled mirror must be reported as a timeout; stderr:\n{}",
        run.stderr
    );
}

#[test]
fn an_interrupted_dpkg_is_repaired_before_the_fallback_attempt() {
    // The bound can fire mid-unpack, and the SIGINT reaches dpkg too. apt
    // then refuses every later install until dpkg is reconfigured, which
    // would make the fallback attempt fail deterministically.
    let harness = BoundedRetryScriptHarness::new(&apt_fixture(STALL_ONLY_ON_THE_INSTALL));
    let run = harness.run_with_attempt_bound(2);
    let log = &run.invocation_log;

    let repaired = log.lines().position(|line| line == "dpkg-repaired");
    let switched = log.lines().position(|line| line.starts_with("switched"));
    assert!(
        repaired.is_some() && switched.is_some() && repaired < switched,
        "dpkg must be repaired before the mirror switch; log:\n{log}"
    );
}

#[test]
fn a_broken_package_is_not_reported_as_a_slow_mirror() {
    // The likeliest deterministic failure here is a version-pinned package
    // name a runner-image roll retired. Calling that a timeout sends the
    // reader hunting a network problem that is not there.
    let harness = BoundedRetryScriptHarness::new(&apt_fixture(FAIL_THE_INSTALL_OUTRIGHT));
    let run = harness.run_with_attempt_bound(5);

    assert!(!run.succeeded, "a broken package must fail the step");
    assert!(
        run.stderr.contains("failed with apt exit status 100"),
        "the real exit status must be reported; stderr:\n{}",
        run.stderr
    );
    assert!(
        !run.stderr.contains("did not finish inside"),
        "nothing timed out, so nothing may say so; stderr:\n{}",
        run.stderr
    );
}

#[test]
fn an_install_that_ignores_the_signal_is_still_reported_as_a_timeout() {
    // `timeout` reports 124 when the command honoured the signal, and 137
    // when `--kill-after` had to escalate to SIGKILL. apt inside a dpkg
    // transaction can be the second, and calling that an apt failure is the
    // same misdiagnosis the 124 branch exists to prevent.
    // Deliberately not `exec sleep`, unlike STALL_ONLY_ON_THE_INSTALL: exec
    // would replace the shell and discard the trap, `sleep` would honour
    // SIGINT, and the escalation this test exists for would never happen.
    let harness = BoundedRetryScriptHarness::new(&apt_fixture(
        "if [ \"$1\" = update ]; then exit 0; fi\ntrap '' INT\nsleep 60\n",
    ));
    let run = harness.run_with_environment(&[
        ("STREAMLIB_APT_PRIVILEGE_PREFIX", "".as_ref()),
        ("STREAMLIB_APT_ATTEMPT_TIMEOUT_SECONDS", "1".as_ref()),
        ("STREAMLIB_APT_KILL_AFTER_SECONDS", "1".as_ref()),
    ]);

    assert!(!run.succeeded, "stderr:\n{}", run.stderr);
    assert!(
        run.stderr.contains("did not finish inside"),
        "a SIGKILL escalation is a timeout, not an apt failure; stderr:\n{}",
        run.stderr
    );
    assert!(
        !run.stderr.contains("apt exit status"),
        "nothing here is an apt exit status; stderr:\n{}",
        run.stderr
    );
}

#[test]
fn both_mirrors_failing_on_the_install_exits_non_zero_rather_than_hanging() {
    let harness = BoundedRetryScriptHarness::new(&apt_fixture(FAIL_THE_INSTALL_OUTRIGHT));
    let run = harness.run_with_attempt_bound(5);
    let log = &run.invocation_log;

    assert!(
        !run.succeeded,
        "exhausting both mirrors must fail; log:\n{log}"
    );
    assert_eq!(
        count_of(log, "install"),
        2,
        "the install must be tried once per mirror, no more; log:\n{log}"
    );
}

#[test]
fn a_failing_update_does_not_go_on_to_install() {
    let harness = BoundedRetryScriptHarness::new(&apt_fixture("exit 100\n"));
    let run = harness.run_with_attempt_bound(5);
    let log = &run.invocation_log;

    assert!(
        !run.succeeded,
        "exhausting both mirrors must fail; log:\n{log}"
    );
    assert_eq!(
        count_of(log, "update"),
        2,
        "one update per mirror; log:\n{log}"
    );
    assert_eq!(
        count_of(log, "install"),
        0,
        "a failed update must not be followed by an install; log:\n{log}"
    );
}

#[test]
fn cached_debs_that_install_offline_never_reach_a_mirror() {
    let harness = BoundedRetryScriptHarness::new(&apt_fixture("exit 0\n"));
    let deb_cache_directory =
        harness.deb_cache_directory_holding(&["protobuf-compiler_3.21_amd64.deb"]);
    let run = harness.run_with_deb_cache_directory(&deb_cache_directory);
    let log = &run.invocation_log;

    assert!(run.succeeded, "the cached .debs must install; log:\n{log}");
    assert_eq!(count_of(log, "update"), 0, "log:\n{log}");
    assert_eq!(count_of(log, "install"), 1, "log:\n{log}");
    let offline_install = log.lines().next().unwrap_or_default();
    assert!(
        offline_install.contains("Acquire::http::Proxy=http://127.0.0.1:9")
            && offline_install.contains("protobuf-compiler_3.21_amd64.deb"),
        "the install must hand apt the cached .debs and forbid a fetch; log:\n{log}"
    );
}

#[test]
fn cached_debs_apt_refuses_fall_back_to_the_mirror_downloading_into_the_cache() {
    let harness =
        BoundedRetryScriptHarness::new(&apt_fixture(REFUSE_THE_INSTALL_WITHOUT_THE_NETWORK));
    let deb_cache_directory =
        harness.deb_cache_directory_holding(&["protobuf-compiler_3.21_amd64.deb"]);
    let run = harness.run_with_deb_cache_directory(&deb_cache_directory);
    let log = &run.invocation_log;

    assert!(run.succeeded, "the mirror must still install; log:\n{log}");
    assert!(
        log.contains("dpkg-repaired"),
        "a refused offline install is repaired before the mirror; log:\n{log}"
    );
    assert_eq!(count_of(log, "update"), 1, "log:\n{log}");
    assert_eq!(count_of(log, "install"), 2, "log:\n{log}");
    let archives_option = format!("Dir::Cache::Archives={}/", deb_cache_directory.display());
    let network_install = log
        .lines()
        .filter(|line| {
            line.starts_with("install") && !line.contains("Acquire::http::Proxy=http://127.0.0.1:9")
        })
        .collect::<Vec<_>>();
    assert!(
        network_install.len() == 1 && network_install[0].contains(&archives_option),
        "the network install must download into the cache; log:\n{log}"
    );
    assert_eq!(
        count_of(log, "autoclean"),
        1,
        "the cache is pruned to what the mirror still offers; log:\n{log}"
    );
}

#[test]
fn an_empty_deb_cache_goes_straight_to_the_mirror() {
    let harness = BoundedRetryScriptHarness::new(&apt_fixture("exit 0\n"));
    let deb_cache_directory = harness.deb_cache_directory_holding(&[]);
    let run = harness.run_with_deb_cache_directory(&deb_cache_directory);
    let log = &run.invocation_log;

    assert!(run.succeeded, "log:\n{log}");
    assert!(
        !log.contains("Acquire::http::Proxy=http://127.0.0.1:9"),
        "nothing cached means no offline attempt; log:\n{log}"
    );
    assert_eq!(count_of(log, "update"), 1, "log:\n{log}");
    assert!(
        deb_cache_directory.join("partial").is_dir(),
        "apt refuses an archives directory without partial/"
    );
}
