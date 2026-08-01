// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Everything about this machine that moves a latency number, captured into
//! `machine-spec.json` beside every measurement artifact.
//!
//! A `source_emit_to_sink_receive` percentile is only defensible if a reader can
//! tell whether the box was locked down or noisy while it was collected. This
//! module records the knobs, records *why* a knob could not be read when it
//! could not, and refuses to call the machine locked on anything it does not
//! know for certain.
//!
//! Nothing here ever invokes `sudo`. `sudo -n` fails on the reference box, so a
//! privileged probe would sit on a password prompt and wedge an unattended
//! multi-hour run. Where locking needs root, the current state is recorded and
//! the exact command an owner would run is emitted as data — never executed.

use std::collections::BTreeSet;
use std::ffi::CStr;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// File name the measurement harness writes this artifact under.
pub const MACHINE_SPECIFICATION_JSON_FILE_NAME: &str = "machine-spec.json";

/// Bumped whenever a field is added, removed, or given a different meaning, so
/// an artifact from an older run is never silently compared against a newer one.
pub const MACHINE_SPECIFICATION_SCHEMA_VERSION: u32 = 1;

/// A one-minute load average at or above this means other work was resident on
/// the box, and a gated measurement cell is refused.
pub const MAXIMUM_ONE_MINUTE_LOAD_AVERAGE_FOR_LOCKED_MEASUREMENT_STATE: f64 = 1.0;

/// A probed field: either an observation, or an explicit reason it is unknown.
/// There is no third state — a missing sysfs file becomes an `Unavailable`
/// carrying the OS error, never an empty string or a zero.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "probe_outcome", rename_all = "snake_case")]
pub enum ProbedValueOrUnavailableReason<ObservedValue> {
    /// The probe ran and produced this value.
    Observed {
        /// The observation.
        value: ObservedValue,
    },
    /// The probe could not run, or ran and produced nothing usable.
    Unavailable {
        /// Why, in enough detail to reproduce the failure on another box.
        reason: String,
    },
}

impl<ObservedValue> ProbedValueOrUnavailableReason<ObservedValue> {
    /// The observation, or `None` when the probe could not determine it.
    pub fn observed_value(&self) -> Option<&ObservedValue> {
        match self {
            Self::Observed { value } => Some(value),
            Self::Unavailable { .. } => None,
        }
    }

    /// Why the field is unknown, or `None` when it was observed.
    pub fn unavailable_reason(&self) -> Option<&str> {
        match self {
            Self::Observed { .. } => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }

    fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }

    fn observed(value: ObservedValue) -> Self {
        Self::Observed { value }
    }
}

/// Everything about this machine that moves a latency number.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineSpecification {
    /// Schema of this artifact; see [`MACHINE_SPECIFICATION_SCHEMA_VERSION`].
    pub machine_specification_schema_version: u32,
    /// Wall-clock stamp for provenance only. Never feeds a latency computation —
    /// every measured interval in this spike comes from `CLOCK_MONOTONIC`.
    pub probed_at_unix_epoch_seconds: ProbedValueOrUnavailableReason<u64>,
    /// `PRETTY_NAME` from `/etc/os-release`.
    pub operating_system_pretty_name: ProbedValueOrUnavailableReason<String>,
    /// Hostname, so two artifacts from different boxes cannot be confused.
    pub host_name: ProbedValueOrUnavailableReason<String>,

    /// CPU model, topology, and every frequency knob.
    pub central_processing_unit: CentralProcessingUnitSpecification,
    /// Kernel identity, preemption model, and the sysctls that move tails.
    pub kernel: KernelSpecification,
    /// glibc version — it decides which futex and `clock_gettime` paths run.
    pub c_library_version: ProbedValueOrUnavailableReason<String>,
    /// GPU identity, driver, and clock state.
    pub graphics_processing_unit: GraphicsProcessingUnitSpecification,
    /// RAM and swap.
    pub memory: MemorySpecification,
    /// The interpreter and numpy the Python arms run against.
    pub python_runtime: PythonRuntimeSpecification,
    /// Scheduling class and niceness of the process that ran this probe.
    pub probing_process_scheduling: ProbingProcessSchedulingSpecification,
    /// What else was resident on the box when the probe ran.
    pub machine_load_at_probe_time: MachineLoadSpecification,

    /// Every reason this machine was not in a locked measurement state at probe
    /// time. Computed once by [`probe_machine_specification`]; recomputable from
    /// the observation fields with [`locked_measurement_state_violations`].
    pub locked_measurement_state_violations: Vec<String>,
    /// The privileged commands that would clear the violations above.
    /// Recorded as data. Never executed.
    pub owner_commands_to_reach_locked_measurement_state:
        Vec<OwnerPrivilegedCommandToReachLockedMeasurementState>,
}

/// CPU model, topology, and every frequency knob that moves a latency tail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CentralProcessingUnitSpecification {
    /// `model name` from `/proc/cpuinfo`.
    pub model_name: ProbedValueOrUnavailableReason<String>,
    /// Distinct `(package, core)` pairs in sysfs topology.
    pub physical_core_count: ProbedValueOrUnavailableReason<usize>,
    /// Online logical processors machine-wide.
    pub logical_processor_count: ProbedValueOrUnavailableReason<usize>,
    /// Logical processors this process may actually run on — affinity and cgroup
    /// aware, so it can be lower than `logical_processor_count`.
    pub logical_processor_count_available_to_this_process: ProbedValueOrUnavailableReason<usize>,
    /// `/sys/devices/system/cpu/smt/control`: `on`, `off`, `forceoff`,
    /// `notsupported`, or `notimplemented`.
    pub simultaneous_multithreading_control: ProbedValueOrUnavailableReason<String>,
    /// cpufreq driver in charge, e.g. `amd-pstate-epp`, `intel_pstate`.
    pub frequency_scaling_driver: ProbedValueOrUnavailableReason<String>,
    // The three per-policy fields below are sequences rather than maps.
    // `ProbedValueOrUnavailableReason` is internally tagged, and serde's
    // internally-tagged path buffers through `Content`, which drops serde_json's
    // integer-map-key coercion: a `BTreeMap<u32, _>` here serializes fine and
    // then fails to deserialize with `invalid type: string "0", expected u32`.
    // A sequence also keeps policies in numeric order, where a string-keyed map
    // would sort `policy10` ahead of `policy2`.
    /// Governor of each cpufreq policy, ascending by policy number.
    pub frequency_scaling_governor_per_policy:
        ProbedValueOrUnavailableReason<Vec<CpuFrequencyPolicyObservation<String>>>,
    /// Energy/performance preference of each cpufreq policy.
    pub energy_performance_preference_per_policy:
        ProbedValueOrUnavailableReason<Vec<CpuFrequencyPolicyObservation<String>>>,
    /// `scaling_cur_freq` of each policy at probe time, converted from kHz to MHz.
    pub current_frequency_megahertz_per_policy:
        ProbedValueOrUnavailableReason<Vec<CpuFrequencyPolicyObservation<f64>>>,
    /// Whether opportunistic boost / turbo is enabled.
    pub boost_is_enabled: ProbedValueOrUnavailableReason<bool>,
    /// Which sysfs file the boost state was read from — the AMD and Intel
    /// drivers expose it under different paths with inverted polarity.
    pub boost_control_sysfs_path: ProbedValueOrUnavailableReason<String>,
    /// `/sys/devices/system/cpu/isolated`. An observed empty string means no
    /// processor is isolated, which is a real observation, not an unknown.
    pub isolated_processor_cpu_list: ProbedValueOrUnavailableReason<String>,
    /// `/sys/devices/system/cpu/nohz_full`. Empty means no tickless processor.
    pub tickless_processor_cpu_list: ProbedValueOrUnavailableReason<String>,
}

/// One cpufreq policy's reading of a single sysfs attribute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuFrequencyPolicyObservation<ObservedAttribute> {
    /// The `N` in `/sys/devices/system/cpu/cpufreq/policyN`.
    pub policy_number: u32,
    /// What that policy reported.
    pub observed_attribute: ObservedAttribute,
}

/// Kernel identity, preemption model, and the sysctls that move latency tails.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelSpecification {
    /// `/proc/sys/kernel/osrelease`.
    pub release: ProbedValueOrUnavailableReason<String>,
    /// `/proc/sys/kernel/version` — the build banner carrying the preempt token.
    pub version_banner: ProbedValueOrUnavailableReason<String>,
    /// The `PREEMPT*` token from the build banner, e.g. `PREEMPT_DYNAMIC`.
    pub build_preemption_model: ProbedValueOrUnavailableReason<String>,
    /// Whether this is a `PREEMPT_RT` kernel.
    pub is_preempt_realtime: ProbedValueOrUnavailableReason<bool>,
    /// The live preemption mode of a `PREEMPT_DYNAMIC` kernel. Lives in debugfs,
    /// which is root-only on a stock Ubuntu install, so this is normally an
    /// `Unavailable` carrying the permission error.
    pub runtime_preemption_mode: ProbedValueOrUnavailableReason<String>,
    /// `/proc/cmdline`, which carries `isolcpus`, `nohz_full`, `mitigations`.
    pub boot_command_line: ProbedValueOrUnavailableReason<String>,
    /// The selected transparent-hugepage policy: `always`, `madvise`, or `never`.
    pub transparent_hugepage_policy: ProbedValueOrUnavailableReason<String>,
    /// `kernel.randomize_va_space`: 0 off, 1 conservative, 2 full.
    pub address_space_layout_randomization_level: ProbedValueOrUnavailableReason<i64>,
    /// `kernel.sched_rt_runtime_us`. A positive value throttles SCHED_FIFO work
    /// to that slice of every period, which shows up as periodic latency spikes.
    pub realtime_scheduler_runtime_microseconds: ProbedValueOrUnavailableReason<i64>,
    /// `kernel.timer_migration`.
    pub timer_migration_is_enabled: ProbedValueOrUnavailableReason<bool>,
    /// `kernel.perf_event_paranoid`. Recorded because it decides what profiling
    /// evidence an unprivileged run can attach to the artifact; it does not
    /// itself move a latency number and so does not gate a cell.
    pub performance_event_paranoid_level: ProbedValueOrUnavailableReason<i64>,
}

/// GPU identity, driver, and clock state as `nvidia-smi` reports them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphicsProcessingUnitSpecification {
    /// Marketing name of GPU 0.
    pub model_name: ProbedValueOrUnavailableReason<String>,
    /// NVIDIA kernel driver version.
    pub driver_version: ProbedValueOrUnavailableReason<String>,
    /// Persistence mode. Off means the driver unloads between clients and the
    /// first submission of a cell pays initialization cost the rest do not.
    pub persistence_mode: ProbedValueOrUnavailableReason<String>,
    /// Current performance state, `P0` (max) through `P8` (idle).
    pub performance_state: ProbedValueOrUnavailableReason<String>,
    /// Compute mode, e.g. `Default`, `Exclusive_Process`.
    pub compute_mode: ProbedValueOrUnavailableReason<String>,
    /// Total device memory in mebibytes.
    pub total_memory_mebibytes: ProbedValueOrUnavailableReason<f64>,
    /// Enforced power limit in watts.
    pub power_limit_watts: ProbedValueOrUnavailableReason<f64>,
    /// Graphics clock at probe time, in megahertz.
    pub current_graphics_clock_megahertz: ProbedValueOrUnavailableReason<f64>,
    /// Highest graphics clock the device will report, in megahertz.
    pub maximum_graphics_clock_megahertz: ProbedValueOrUnavailableReason<f64>,
    /// Memory clock at probe time, in megahertz.
    pub current_memory_clock_megahertz: ProbedValueOrUnavailableReason<f64>,
    /// NVML `clocks_event_reasons.active` bitmask as reported, hex.
    pub active_clock_event_reasons_bitmask: ProbedValueOrUnavailableReason<String>,
    /// Whether `--lock-gpu-clocks` is in force. NVML exposes no read-back for
    /// the locked range, so on an unprivileged probe this is always an
    /// `Unavailable` and the lock is a stated protocol variable rather than a
    /// gate condition.
    pub graphics_clock_lock_state: ProbedValueOrUnavailableReason<String>,
}

/// RAM and swap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySpecification {
    /// `MemTotal` in kibibytes.
    pub total_kibibytes: ProbedValueOrUnavailableReason<u64>,
    /// `MemAvailable` in kibibytes at probe time.
    pub available_kibibytes: ProbedValueOrUnavailableReason<u64>,
    /// `SwapTotal` in kibibytes.
    pub swap_total_kibibytes: ProbedValueOrUnavailableReason<u64>,
    /// `SwapTotal - SwapFree` in kibibytes: pages already out at probe time,
    /// each of which is a multi-millisecond fault waiting to land in a tail.
    pub swap_used_kibibytes: ProbedValueOrUnavailableReason<u64>,
    /// Whether any swap device is configured.
    pub swap_is_enabled: ProbedValueOrUnavailableReason<bool>,
}

/// The interpreter and numpy the Python arms of the spike run against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PythonRuntimeSpecification {
    /// Absolute path of the interpreter that answered the probe.
    pub interpreter_executable_path: ProbedValueOrUnavailableReason<String>,
    /// Dotted version, e.g. `3.12.3`.
    pub interpreter_version: ProbedValueOrUnavailableReason<String>,
    /// Full `sys.version` banner, which carries the compiler and build date.
    pub interpreter_version_banner: ProbedValueOrUnavailableReason<String>,
    /// numpy version, which decides the zero-copy buffer path the PyO3 arm hits.
    pub numpy_version: ProbedValueOrUnavailableReason<String>,
    /// Whether this is a free-threaded build with the GIL disabled — the single
    /// fact that most changes what an in-process Python result means.
    pub global_interpreter_lock_is_disabled: ProbedValueOrUnavailableReason<bool>,
}

/// Scheduling class and niceness of the process that ran this probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbingProcessSchedulingSpecification {
    /// `SCHED_OTHER`, `SCHED_FIFO`, `SCHED_RR`, `SCHED_BATCH`, `SCHED_IDLE`,
    /// or `SCHED_DEADLINE`. A stated protocol variable: both spike arms must
    /// run under the same one for their percentiles to be comparable.
    pub scheduling_policy_name: ProbedValueOrUnavailableReason<String>,
    /// Raw `sched_getscheduler` result with `SCHED_RESET_ON_FORK` masked off.
    pub scheduling_policy_number: ProbedValueOrUnavailableReason<i32>,
    /// Whether `SCHED_RESET_ON_FORK` was set — children would drop back to
    /// `SCHED_OTHER`, which silently un-does a `chrt` on a spawned arm.
    pub scheduling_policy_resets_on_fork: ProbedValueOrUnavailableReason<bool>,
    /// Realtime priority, 0 under the fair-share classes.
    pub realtime_priority: ProbedValueOrUnavailableReason<i32>,
    /// Niceness, -20 through 19.
    pub niceness: ProbedValueOrUnavailableReason<i32>,
}

/// What else was resident on the box when the probe ran.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineLoadSpecification {
    /// One-minute load average.
    pub load_average_one_minute: ProbedValueOrUnavailableReason<f64>,
    /// Five-minute load average.
    pub load_average_five_minutes: ProbedValueOrUnavailableReason<f64>,
    /// Fifteen-minute load average.
    pub load_average_fifteen_minutes: ProbedValueOrUnavailableReason<f64>,
    /// Currently runnable kernel scheduling entities.
    pub runnable_scheduling_entity_count: ProbedValueOrUnavailableReason<u64>,
    /// Total kernel scheduling entities on the box.
    pub total_scheduling_entity_count: ProbedValueOrUnavailableReason<u64>,
}

/// A root-only command that would clear one locked-state violation. Emitted as
/// data for an owner to run by hand — this module never executes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerPrivilegedCommandToReachLockedMeasurementState {
    /// The knob the command locks.
    pub knob_description: String,
    /// What the probe saw, so the owner can tell whether it is still needed.
    pub current_observed_state: String,
    /// The exact shell command, verbatim.
    pub shell_command: String,
    /// The command that puts the knob back afterwards.
    pub reverting_shell_command: String,
}

/// One knob's lock command paired with the command that puts it back. Pairing
/// them in one value is what keeps a lock step from outliving its restore step.
#[derive(Debug, Clone, Copy)]
pub struct ReferenceMachineLockAndRestoreCommandPair {
    /// The knob both commands target.
    pub knob_description: &'static str,
    /// Run before the measurement run.
    pub locking_shell_command: &'static str,
    /// Run after it — leaving SMT off and swap disabled is a change to the
    /// owner's daily machine, not a measurement artifact.
    pub restoring_shell_command: &'static str,
}

/// What [`owner_checklist_to_reach_locked_measurement_state`] actually emitted
/// on the reference box on 2026-08-01: AMD Ryzen 9 5900X (12 physical / 24
/// logical) on `amd-pstate-epp`, Ubuntu 24.04.4, kernel 7.0.0-28-generic
/// `PREEMPT_DYNAMIC`, RTX 3090 on driver 595.84.
///
/// The `performance` governor and `madvise` hugepage policy were already in
/// their locked state, so no command for them appears. The one remaining
/// violation on that box — a 3.45 one-minute load average — has no command:
/// it clears by quiescing the machine before the run.
pub const OWNER_CHECKLIST_OBSERVED_ON_REFERENCE_MACHINE:
    &[ReferenceMachineLockAndRestoreCommandPair] = &[
    ReferenceMachineLockAndRestoreCommandPair {
        knob_description: "opportunistic CPU boost/turbo",
        locking_shell_command: "echo 0 | sudo tee /sys/devices/system/cpu/cpufreq/boost",
        restoring_shell_command: "echo 1 | sudo tee /sys/devices/system/cpu/cpufreq/boost",
    },
    ReferenceMachineLockAndRestoreCommandPair {
        knob_description: "simultaneous multithreading",
        locking_shell_command: "echo off | sudo tee /sys/devices/system/cpu/smt/control",
        restoring_shell_command: "echo on | sudo tee /sys/devices/system/cpu/smt/control",
    },
    ReferenceMachineLockAndRestoreCommandPair {
        knob_description: "swap",
        locking_shell_command: "sudo swapoff -a",
        restoring_shell_command: "sudo swapon -a",
    },
    ReferenceMachineLockAndRestoreCommandPair {
        knob_description: "GPU persistence mode",
        locking_shell_command: "sudo nvidia-smi -pm 1",
        restoring_shell_command: "sudo nvidia-smi -pm 0",
    },
    ReferenceMachineLockAndRestoreCommandPair {
        knob_description: "GPU graphics clock lock",
        locking_shell_command: "sudo nvidia-smi -i 0 --lock-gpu-clocks=1680,1680",
        restoring_shell_command: "sudo nvidia-smi -i 0 --reset-gpu-clocks",
    },
];

const CPU_FREQUENCY_SCALING_SYSFS_DIRECTORY: &str = "/sys/devices/system/cpu/cpufreq";
const CPU_TOPOLOGY_SYSFS_DIRECTORY: &str = "/sys/devices/system/cpu";
const AMD_AND_ACPI_BOOST_SYSFS_PATH: &str = "/sys/devices/system/cpu/cpufreq/boost";
const INTEL_PSTATE_NO_TURBO_SYSFS_PATH: &str = "/sys/devices/system/cpu/intel_pstate/no_turbo";
const TRANSPARENT_HUGEPAGE_SYSFS_PATH: &str = "/sys/kernel/mm/transparent_hugepage/enabled";
const RUNTIME_PREEMPTION_MODE_DEBUGFS_PATH: &str = "/sys/kernel/debug/sched/preempt";

/// Probe the machine. Never fails: a probe that cannot run records why.
pub fn probe_machine_specification() -> MachineSpecification {
    let mut specification = MachineSpecification {
        machine_specification_schema_version: MACHINE_SPECIFICATION_SCHEMA_VERSION,
        probed_at_unix_epoch_seconds: probe_unix_epoch_seconds(),
        operating_system_pretty_name: probe_operating_system_pretty_name(),
        host_name: read_trimmed_file_contents("/proc/sys/kernel/hostname"),
        central_processing_unit: probe_central_processing_unit(),
        kernel: probe_kernel(),
        c_library_version: probe_c_library_version(),
        graphics_processing_unit: probe_graphics_processing_unit(),
        memory: probe_memory(),
        python_runtime: probe_python_runtime(),
        probing_process_scheduling: probe_process_scheduling(),
        machine_load_at_probe_time: probe_machine_load(),
        locked_measurement_state_violations: Vec::new(),
        owner_commands_to_reach_locked_measurement_state: Vec::new(),
    };

    specification.locked_measurement_state_violations =
        locked_measurement_state_violations(&specification);
    specification.owner_commands_to_reach_locked_measurement_state =
        owner_checklist_to_reach_locked_measurement_state(&specification);

    tracing::info!(
        violation_count = specification.locked_measurement_state_violations.len(),
        "probed machine specification"
    );
    specification
}

/// True when every latency-relevant knob is in its locked/deterministic state.
/// The harness refuses to run a gated cell when this is false.
pub fn machine_is_in_locked_measurement_state(specification: &MachineSpecification) -> bool {
    locked_measurement_state_violations(specification).is_empty()
}

/// Every reason this machine is not in a locked measurement state, recomputed
/// from the observation fields. An unknown field is always a violation.
///
/// The gate covers knobs that are readable without root, demonstrably move the
/// latency tail, and are machine-global rather than per-run. Scheduling policy,
/// `perf_event_paranoid`, and the GPU clock lock are stated protocol variables
/// recorded in the artifact instead: the first two are properties of how the
/// harness was launched, and NVML exposes no read-back for the third.
pub fn locked_measurement_state_violations(specification: &MachineSpecification) -> Vec<String> {
    let mut violations = Vec::new();
    let central_processing_unit = &specification.central_processing_unit;

    match &central_processing_unit.frequency_scaling_governor_per_policy {
        ProbedValueOrUnavailableReason::Unavailable { reason } => violations.push(format!(
            "cpufreq governor is unknown, which counts as unlocked: {reason}"
        )),
        ProbedValueOrUnavailableReason::Observed { value } => {
            if value.is_empty() {
                violations.push(
                    "cpufreq exposed no policies, so the governor could not be confirmed"
                        .to_string(),
                );
            }
            let policies_not_on_performance: Vec<String> = value
                .iter()
                .filter(|observation| observation.observed_attribute != "performance")
                .map(|observation| {
                    format!(
                        "policy{}={}",
                        observation.policy_number, observation.observed_attribute
                    )
                })
                .collect();
            if !policies_not_on_performance.is_empty() {
                violations.push(format!(
                    "cpufreq governor is not `performance` on {}",
                    policies_not_on_performance.join(", ")
                ));
            }
        }
    }

    record_violation_unless_boolean_knob_matches(
        &central_processing_unit.boost_is_enabled,
        false,
        "CPU boost/turbo",
        &mut violations,
    );

    match &central_processing_unit.simultaneous_multithreading_control {
        ProbedValueOrUnavailableReason::Unavailable { reason } => violations.push(format!(
            "simultaneous multithreading state is unknown, which counts as unlocked: {reason}"
        )),
        ProbedValueOrUnavailableReason::Observed { value } => {
            if !matches!(value.as_str(), "off" | "forceoff" | "notsupported") {
                violations.push(format!(
                    "simultaneous multithreading is `{value}`; a sibling thread sharing the core \
                     is a tail-latency source"
                ));
            }
        }
    }

    record_violation_unless_boolean_knob_matches(
        &specification.memory.swap_is_enabled,
        false,
        "swap",
        &mut violations,
    );

    match &specification.kernel.transparent_hugepage_policy {
        ProbedValueOrUnavailableReason::Unavailable { reason } => violations.push(format!(
            "transparent hugepage policy is unknown, which counts as unlocked: {reason}"
        )),
        ProbedValueOrUnavailableReason::Observed { value } => {
            if value.as_str() == "always" {
                violations.push(
                    "transparent hugepages are set to `always`; khugepaged compaction stalls land \
                     directly in the p99.9"
                        .to_string(),
                );
            }
        }
    }

    match &specification.graphics_processing_unit.persistence_mode {
        ProbedValueOrUnavailableReason::Unavailable { reason } => violations.push(format!(
            "GPU persistence mode is unknown, which counts as unlocked: {reason}"
        )),
        ProbedValueOrUnavailableReason::Observed { value } => {
            if !value.eq_ignore_ascii_case("enabled") {
                violations.push(format!(
                    "GPU persistence mode is `{value}`; the driver unloads between clients and the \
                     first submissions of a cell pay initialization the rest do not"
                ));
            }
        }
    }

    match &specification
        .machine_load_at_probe_time
        .load_average_one_minute
    {
        ProbedValueOrUnavailableReason::Unavailable { reason } => violations.push(format!(
            "one-minute load average is unknown, which counts as unlocked: {reason}"
        )),
        ProbedValueOrUnavailableReason::Observed { value } => {
            if *value >= MAXIMUM_ONE_MINUTE_LOAD_AVERAGE_FOR_LOCKED_MEASUREMENT_STATE {
                violations.push(format!(
                    "one-minute load average is {value:.2}, at or above the \
                     {MAXIMUM_ONE_MINUTE_LOAD_AVERAGE_FOR_LOCKED_MEASUREMENT_STATE:.2} ceiling; \
                     other work was resident on the box"
                ));
            }
        }
    }

    violations
}

/// The privileged commands that would clear this machine's violations, derived
/// from what was actually observed. Data only — nothing here is ever executed,
/// and `sudo` is never invoked by this crate.
pub fn owner_checklist_to_reach_locked_measurement_state(
    specification: &MachineSpecification,
) -> Vec<OwnerPrivilegedCommandToReachLockedMeasurementState> {
    let mut checklist = Vec::new();
    let central_processing_unit = &specification.central_processing_unit;

    let governors_already_locked = matches!(
        &central_processing_unit.frequency_scaling_governor_per_policy,
        ProbedValueOrUnavailableReason::Observed { value }
            if !value.is_empty()
                && value
                    .iter()
                    .all(|observation| observation.observed_attribute == "performance")
    );
    if !governors_already_locked {
        checklist.push(OwnerPrivilegedCommandToReachLockedMeasurementState {
            knob_description: "cpufreq governor on every policy".to_string(),
            current_observed_state: describe_probed_value(
                &central_processing_unit.frequency_scaling_governor_per_policy,
                |governor_per_policy| {
                    summarize_distinct_values(
                        governor_per_policy
                            .iter()
                            .map(|observation| &observation.observed_attribute),
                    )
                },
            ),
            shell_command: "sudo cpupower frequency-set -g performance".to_string(),
            reverting_shell_command: "sudo cpupower frequency-set -g schedutil".to_string(),
        });
    }

    if !boolean_knob_is_observed_as(&central_processing_unit.boost_is_enabled, false) {
        let boost_control_path = central_processing_unit
            .boost_control_sysfs_path
            .observed_value()
            .cloned()
            .unwrap_or_else(|| AMD_AND_ACPI_BOOST_SYSFS_PATH.to_string());
        let boost_is_inverted_polarity = boost_control_path == INTEL_PSTATE_NO_TURBO_SYSFS_PATH;
        let (disabling_value, enabling_value) = if boost_is_inverted_polarity {
            ("1", "0")
        } else {
            ("0", "1")
        };
        checklist.push(OwnerPrivilegedCommandToReachLockedMeasurementState {
            knob_description: "opportunistic CPU boost/turbo".to_string(),
            current_observed_state: describe_probed_value(
                &central_processing_unit.boost_is_enabled,
                |is_enabled| if *is_enabled { "enabled" } else { "disabled" }.to_string(),
            ),
            shell_command: format!("echo {disabling_value} | sudo tee {boost_control_path}"),
            reverting_shell_command: format!(
                "echo {enabling_value} | sudo tee {boost_control_path}"
            ),
        });
    }

    let multithreading_already_off = matches!(
        &central_processing_unit.simultaneous_multithreading_control,
        ProbedValueOrUnavailableReason::Observed { value }
            if matches!(value.as_str(), "off" | "forceoff" | "notsupported")
    );
    if !multithreading_already_off {
        checklist.push(OwnerPrivilegedCommandToReachLockedMeasurementState {
            knob_description: "simultaneous multithreading".to_string(),
            current_observed_state: describe_probed_value(
                &central_processing_unit.simultaneous_multithreading_control,
                Clone::clone,
            ),
            shell_command: "echo off | sudo tee /sys/devices/system/cpu/smt/control".to_string(),
            reverting_shell_command: "echo on | sudo tee /sys/devices/system/cpu/smt/control"
                .to_string(),
        });
    }

    let hugepage_policy_already_acceptable = matches!(
        &specification.kernel.transparent_hugepage_policy,
        ProbedValueOrUnavailableReason::Observed { value } if value.as_str() != "always"
    );
    if !hugepage_policy_already_acceptable {
        checklist.push(OwnerPrivilegedCommandToReachLockedMeasurementState {
            knob_description: "transparent hugepage policy".to_string(),
            current_observed_state: describe_probed_value(
                &specification.kernel.transparent_hugepage_policy,
                Clone::clone,
            ),
            shell_command: format!("echo never | sudo tee {TRANSPARENT_HUGEPAGE_SYSFS_PATH}"),
            reverting_shell_command: format!(
                "echo madvise | sudo tee {TRANSPARENT_HUGEPAGE_SYSFS_PATH}"
            ),
        });
    }

    if !boolean_knob_is_observed_as(&specification.memory.swap_is_enabled, false) {
        checklist.push(OwnerPrivilegedCommandToReachLockedMeasurementState {
            knob_description: "swap".to_string(),
            current_observed_state: describe_probed_value(
                &specification.memory.swap_is_enabled,
                |is_enabled| if *is_enabled { "enabled" } else { "disabled" }.to_string(),
            ),
            shell_command: "sudo swapoff -a".to_string(),
            reverting_shell_command: "sudo swapon -a".to_string(),
        });
    }

    let graphics_processing_unit = &specification.graphics_processing_unit;
    let persistence_already_enabled = matches!(
        &graphics_processing_unit.persistence_mode,
        ProbedValueOrUnavailableReason::Observed { value } if value.eq_ignore_ascii_case("enabled")
    );
    if !persistence_already_enabled {
        checklist.push(OwnerPrivilegedCommandToReachLockedMeasurementState {
            knob_description: "GPU persistence mode".to_string(),
            current_observed_state: describe_probed_value(
                &graphics_processing_unit.persistence_mode,
                Clone::clone,
            ),
            shell_command: "sudo nvidia-smi -pm 1".to_string(),
            reverting_shell_command: "sudo nvidia-smi -pm 0".to_string(),
        });
    }

    // NVML reports no locked-clock range back, so this entry is emitted on every
    // run rather than gated on an observation, and it is deliberately not one of
    // the violations `machine_is_in_locked_measurement_state` counts.
    let graphics_clock_lock_megahertz = graphics_processing_unit
        .maximum_graphics_clock_megahertz
        .observed_value()
        .map_or(1695, |maximum_megahertz| {
            // Locking at the reported maximum invites thermal slowdown mid-cell;
            // NVIDIA's rated boost clock sits near 80% of it.
            (maximum_megahertz * 0.8).round() as i64
        });
    checklist.push(OwnerPrivilegedCommandToReachLockedMeasurementState {
        knob_description: "GPU graphics clock lock (not readable back from NVML)".to_string(),
        current_observed_state: describe_probed_value(
            &graphics_processing_unit.current_graphics_clock_megahertz,
            |megahertz| format!("{megahertz} MHz at probe time"),
        ),
        shell_command: format!(
            "sudo nvidia-smi -i 0 --lock-gpu-clocks={graphics_clock_lock_megahertz},\
             {graphics_clock_lock_megahertz}"
        ),
        reverting_shell_command: "sudo nvidia-smi -i 0 --reset-gpu-clocks".to_string(),
    });

    checklist
}

/// Serialize the specification to `machine-spec.json` under `directory_path`.
pub fn write_machine_specification_json_file(
    specification: &MachineSpecification,
    directory_path: &Path,
) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(directory_path)?;
    let file_path = directory_path.join(MACHINE_SPECIFICATION_JSON_FILE_NAME);
    let serialized = serde_json::to_string_pretty(specification)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    std::fs::write(&file_path, serialized)?;
    Ok(file_path)
}

fn record_violation_unless_boolean_knob_matches(
    probed_knob: &ProbedValueOrUnavailableReason<bool>,
    required_value: bool,
    knob_description: &str,
    violations: &mut Vec<String>,
) {
    match probed_knob {
        ProbedValueOrUnavailableReason::Unavailable { reason } => violations.push(format!(
            "{knob_description} state is unknown, which counts as unlocked: {reason}"
        )),
        ProbedValueOrUnavailableReason::Observed { value } => {
            if *value != required_value {
                violations.push(format!(
                    "{knob_description} is {}, and a locked measurement state requires it {}",
                    if *value { "enabled" } else { "disabled" },
                    if required_value {
                        "enabled"
                    } else {
                        "disabled"
                    }
                ));
            }
        }
    }
}

fn boolean_knob_is_observed_as(
    probed_knob: &ProbedValueOrUnavailableReason<bool>,
    expected_value: bool,
) -> bool {
    probed_knob.observed_value() == Some(&expected_value)
}

fn describe_probed_value<ObservedValue>(
    probed_value: &ProbedValueOrUnavailableReason<ObservedValue>,
    describe_observation: impl Fn(&ObservedValue) -> String,
) -> String {
    match probed_value {
        ProbedValueOrUnavailableReason::Observed { value } => describe_observation(value),
        ProbedValueOrUnavailableReason::Unavailable { reason } => format!("unknown: {reason}"),
    }
}

fn summarize_distinct_values<'a>(values: impl Iterator<Item = &'a String>) -> String {
    let distinct: BTreeSet<&str> = values.map(String::as_str).collect();
    if distinct.is_empty() {
        "no values reported".to_string()
    } else {
        distinct.into_iter().collect::<Vec<_>>().join(", ")
    }
}

fn read_trimmed_file_contents(path: impl AsRef<Path>) -> ProbedValueOrUnavailableReason<String> {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(contents) => ProbedValueOrUnavailableReason::observed(contents.trim().to_string()),
        Err(error) => ProbedValueOrUnavailableReason::unavailable(format!(
            "`{}` could not be read: {error}",
            path.display()
        )),
    }
}

fn read_file_contents_parsed<ParsedValue>(
    path: impl AsRef<Path>,
) -> ProbedValueOrUnavailableReason<ParsedValue>
where
    ParsedValue: std::str::FromStr,
    ParsedValue::Err: std::fmt::Display,
{
    let path = path.as_ref();
    match read_trimmed_file_contents(path) {
        ProbedValueOrUnavailableReason::Unavailable { reason } => {
            ProbedValueOrUnavailableReason::unavailable(reason)
        }
        ProbedValueOrUnavailableReason::Observed { value } => match value.parse::<ParsedValue>() {
            Ok(parsed) => ProbedValueOrUnavailableReason::observed(parsed),
            Err(error) => ProbedValueOrUnavailableReason::unavailable(format!(
                "`{}` held `{value}`, which did not parse: {error}",
                path.display()
            )),
        },
    }
}

fn read_sysfs_boolean_flag(path: &str) -> ProbedValueOrUnavailableReason<bool> {
    match read_file_contents_parsed::<i64>(path) {
        ProbedValueOrUnavailableReason::Observed { value } => {
            ProbedValueOrUnavailableReason::observed(value != 0)
        }
        ProbedValueOrUnavailableReason::Unavailable { reason } => {
            ProbedValueOrUnavailableReason::unavailable(reason)
        }
    }
}

fn run_command_capturing_trimmed_standard_output(
    program: &str,
    arguments: &[&str],
) -> Result<String, String> {
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("`{program}` could not be executed: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`{program}` exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn probe_unix_epoch_seconds() -> ProbedValueOrUnavailableReason<u64> {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => ProbedValueOrUnavailableReason::observed(elapsed.as_secs()),
        Err(error) => ProbedValueOrUnavailableReason::unavailable(format!(
            "the system clock is before the Unix epoch: {error}"
        )),
    }
}

fn probe_operating_system_pretty_name() -> ProbedValueOrUnavailableReason<String> {
    match std::fs::read_to_string("/etc/os-release") {
        Err(error) => ProbedValueOrUnavailableReason::unavailable(format!(
            "`/etc/os-release` could not be read: {error}"
        )),
        Ok(contents) => contents
            .lines()
            .find_map(|line| line.strip_prefix("PRETTY_NAME="))
            .map(|value| {
                ProbedValueOrUnavailableReason::observed(value.trim().trim_matches('"').to_string())
            })
            .unwrap_or_else(|| {
                ProbedValueOrUnavailableReason::unavailable(
                    "`/etc/os-release` carried no PRETTY_NAME line".to_string(),
                )
            }),
    }
}

fn probe_c_library_version() -> ProbedValueOrUnavailableReason<String> {
    // SAFETY: `gnu_get_libc_version` returns a pointer to a static NUL-terminated
    // string owned by glibc; it is never null and never freed.
    let version = unsafe { CStr::from_ptr(libc::gnu_get_libc_version()) };
    match version.to_str() {
        Ok(version) => ProbedValueOrUnavailableReason::observed(version.to_string()),
        Err(error) => ProbedValueOrUnavailableReason::unavailable(format!(
            "`gnu_get_libc_version` returned non-UTF-8 bytes: {error}"
        )),
    }
}

fn probe_central_processing_unit() -> CentralProcessingUnitSpecification {
    let (physical_core_count, logical_processor_count) = probe_processor_topology_counts();
    let (boost_is_enabled, boost_control_sysfs_path) = probe_boost_state();

    CentralProcessingUnitSpecification {
        model_name: probe_processor_model_name(),
        physical_core_count,
        logical_processor_count,
        logical_processor_count_available_to_this_process: match std::thread::available_parallelism(
        ) {
            Ok(count) => ProbedValueOrUnavailableReason::observed(count.get()),
            Err(error) => ProbedValueOrUnavailableReason::unavailable(format!(
                "`available_parallelism` failed: {error}"
            )),
        },
        simultaneous_multithreading_control: read_trimmed_file_contents(
            "/sys/devices/system/cpu/smt/control",
        ),
        frequency_scaling_driver: read_trimmed_file_contents(format!(
            "{CPU_FREQUENCY_SCALING_SYSFS_DIRECTORY}/policy0/scaling_driver"
        )),
        frequency_scaling_governor_per_policy: probe_text_attribute_per_cpu_frequency_policy(
            "scaling_governor",
        ),
        energy_performance_preference_per_policy: probe_text_attribute_per_cpu_frequency_policy(
            "energy_performance_preference",
        ),
        current_frequency_megahertz_per_policy: probe_current_frequency_megahertz_per_policy(),
        boost_is_enabled,
        boost_control_sysfs_path,
        isolated_processor_cpu_list: read_trimmed_file_contents("/sys/devices/system/cpu/isolated"),
        tickless_processor_cpu_list: read_trimmed_file_contents(
            "/sys/devices/system/cpu/nohz_full",
        ),
    }
}

fn probe_processor_model_name() -> ProbedValueOrUnavailableReason<String> {
    match std::fs::read_to_string("/proc/cpuinfo") {
        Err(error) => ProbedValueOrUnavailableReason::unavailable(format!(
            "`/proc/cpuinfo` could not be read: {error}"
        )),
        Ok(contents) => contents
            .lines()
            .find(|line| line.starts_with("model name"))
            .and_then(|line| line.split_once(':'))
            .map(|(_, model_name)| {
                ProbedValueOrUnavailableReason::observed(model_name.trim().to_string())
            })
            .unwrap_or_else(|| {
                ProbedValueOrUnavailableReason::unavailable(
                    "`/proc/cpuinfo` carried no `model name` line".to_string(),
                )
            }),
    }
}

fn probe_processor_topology_counts() -> (
    ProbedValueOrUnavailableReason<usize>,
    ProbedValueOrUnavailableReason<usize>,
) {
    let entries = match std::fs::read_dir(CPU_TOPOLOGY_SYSFS_DIRECTORY) {
        Ok(entries) => entries,
        Err(error) => {
            let reason = format!("`{CPU_TOPOLOGY_SYSFS_DIRECTORY}` could not be listed: {error}");
            return (
                ProbedValueOrUnavailableReason::unavailable(reason.clone()),
                ProbedValueOrUnavailableReason::unavailable(reason),
            );
        }
    };

    let mut distinct_physical_cores: BTreeSet<(String, String)> = BTreeSet::new();
    let mut logical_processor_count = 0usize;
    for entry in entries.flatten() {
        let entry_name = entry.file_name().to_string_lossy().into_owned();
        let Some(processor_number) = entry_name.strip_prefix("cpu") else {
            continue;
        };
        if !processor_number
            .chars()
            .all(|character| character.is_ascii_digit())
            || processor_number.is_empty()
        {
            continue;
        }
        logical_processor_count += 1;
        let topology_directory = entry.path().join("topology");
        let package_identifier =
            read_trimmed_file_contents(topology_directory.join("physical_package_id"));
        let core_identifier = read_trimmed_file_contents(topology_directory.join("core_id"));
        if let (Some(package_identifier), Some(core_identifier)) = (
            package_identifier.observed_value(),
            core_identifier.observed_value(),
        ) {
            distinct_physical_cores.insert((package_identifier.clone(), core_identifier.clone()));
        }
    }

    let physical_core_count = if distinct_physical_cores.is_empty() {
        ProbedValueOrUnavailableReason::unavailable(format!(
            "no `{CPU_TOPOLOGY_SYSFS_DIRECTORY}/cpu*/topology` entry exposed both \
             physical_package_id and core_id"
        ))
    } else {
        ProbedValueOrUnavailableReason::observed(distinct_physical_cores.len())
    };
    let logical_processor_count = if logical_processor_count == 0 {
        ProbedValueOrUnavailableReason::unavailable(format!(
            "`{CPU_TOPOLOGY_SYSFS_DIRECTORY}` listed no `cpuN` entries"
        ))
    } else {
        ProbedValueOrUnavailableReason::observed(logical_processor_count)
    };
    (physical_core_count, logical_processor_count)
}

fn read_cpu_frequency_policy_numbers() -> Result<Vec<u32>, String> {
    let entries = std::fs::read_dir(CPU_FREQUENCY_SCALING_SYSFS_DIRECTORY).map_err(|error| {
        format!("`{CPU_FREQUENCY_SCALING_SYSFS_DIRECTORY}` could not be listed: {error}")
    })?;
    let mut policy_numbers: Vec<u32> = entries
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .strip_prefix("policy")
                .and_then(|policy_number| policy_number.parse::<u32>().ok())
        })
        .collect();
    policy_numbers.sort_unstable();
    Ok(policy_numbers)
}

fn probe_text_attribute_per_cpu_frequency_policy(
    attribute_file_name: &str,
) -> ProbedValueOrUnavailableReason<Vec<CpuFrequencyPolicyObservation<String>>> {
    let policy_numbers = match read_cpu_frequency_policy_numbers() {
        Ok(policy_numbers) => policy_numbers,
        Err(reason) => return ProbedValueOrUnavailableReason::unavailable(reason),
    };
    let mut observations = Vec::new();
    for policy_number in &policy_numbers {
        let path = format!(
            "{CPU_FREQUENCY_SCALING_SYSFS_DIRECTORY}/policy{policy_number}/{attribute_file_name}"
        );
        if let Some(value) = read_trimmed_file_contents(&path).observed_value() {
            observations.push(CpuFrequencyPolicyObservation {
                policy_number: *policy_number,
                observed_attribute: value.clone(),
            });
        }
    }
    if observations.is_empty() && !policy_numbers.is_empty() {
        return ProbedValueOrUnavailableReason::unavailable(format!(
            "none of the {} cpufreq policies exposed `{attribute_file_name}`",
            policy_numbers.len()
        ));
    }
    ProbedValueOrUnavailableReason::observed(observations)
}

fn probe_current_frequency_megahertz_per_policy()
-> ProbedValueOrUnavailableReason<Vec<CpuFrequencyPolicyObservation<f64>>> {
    let policy_numbers = match read_cpu_frequency_policy_numbers() {
        Ok(policy_numbers) => policy_numbers,
        Err(reason) => return ProbedValueOrUnavailableReason::unavailable(reason),
    };
    let mut observations = Vec::new();
    for policy_number in &policy_numbers {
        let path = format!(
            "{CPU_FREQUENCY_SCALING_SYSFS_DIRECTORY}/policy{policy_number}/scaling_cur_freq"
        );
        // sysfs reports cpufreq frequencies in kHz.
        if let Some(kilohertz) = read_file_contents_parsed::<f64>(&path).observed_value() {
            observations.push(CpuFrequencyPolicyObservation {
                policy_number: *policy_number,
                observed_attribute: kilohertz / 1000.0,
            });
        }
    }
    if observations.is_empty() && !policy_numbers.is_empty() {
        return ProbedValueOrUnavailableReason::unavailable(format!(
            "none of the {} cpufreq policies exposed `scaling_cur_freq`",
            policy_numbers.len()
        ));
    }
    ProbedValueOrUnavailableReason::observed(observations)
}

fn probe_boost_state() -> (
    ProbedValueOrUnavailableReason<bool>,
    ProbedValueOrUnavailableReason<String>,
) {
    // The two cpufreq driver families spell boost differently and with opposite
    // polarity: `cpufreq/boost` is 1 when boost is ON, `intel_pstate/no_turbo`
    // is 1 when boost is OFF.
    if let Some(boost_is_enabled) =
        read_sysfs_boolean_flag(AMD_AND_ACPI_BOOST_SYSFS_PATH).observed_value()
    {
        return (
            ProbedValueOrUnavailableReason::observed(*boost_is_enabled),
            ProbedValueOrUnavailableReason::observed(AMD_AND_ACPI_BOOST_SYSFS_PATH.to_string()),
        );
    }
    if let Some(turbo_is_disabled) =
        read_sysfs_boolean_flag(INTEL_PSTATE_NO_TURBO_SYSFS_PATH).observed_value()
    {
        return (
            ProbedValueOrUnavailableReason::observed(!*turbo_is_disabled),
            ProbedValueOrUnavailableReason::observed(INTEL_PSTATE_NO_TURBO_SYSFS_PATH.to_string()),
        );
    }
    let reason = format!(
        "neither `{AMD_AND_ACPI_BOOST_SYSFS_PATH}` nor `{INTEL_PSTATE_NO_TURBO_SYSFS_PATH}` \
         was readable"
    );
    (
        ProbedValueOrUnavailableReason::unavailable(reason.clone()),
        ProbedValueOrUnavailableReason::unavailable(reason),
    )
}

fn probe_kernel() -> KernelSpecification {
    let release = read_trimmed_file_contents("/proc/sys/kernel/osrelease");
    let version_banner = read_trimmed_file_contents("/proc/sys/kernel/version");
    let build_configuration = release
        .observed_value()
        .map(|release| read_trimmed_file_contents(format!("/boot/config-{release}")));

    KernelSpecification {
        build_preemption_model: probe_build_preemption_model(&version_banner),
        is_preempt_realtime: probe_is_preempt_realtime(
            &version_banner,
            build_configuration.as_ref(),
        ),
        runtime_preemption_mode: read_trimmed_file_contents(RUNTIME_PREEMPTION_MODE_DEBUGFS_PATH),
        boot_command_line: read_trimmed_file_contents("/proc/cmdline"),
        transparent_hugepage_policy: probe_transparent_hugepage_policy(),
        address_space_layout_randomization_level: read_file_contents_parsed(
            "/proc/sys/kernel/randomize_va_space",
        ),
        realtime_scheduler_runtime_microseconds: read_file_contents_parsed(
            "/proc/sys/kernel/sched_rt_runtime_us",
        ),
        timer_migration_is_enabled: read_sysfs_boolean_flag("/proc/sys/kernel/timer_migration"),
        performance_event_paranoid_level: read_file_contents_parsed(
            "/proc/sys/kernel/perf_event_paranoid",
        ),
        release,
        version_banner,
    }
}

fn probe_build_preemption_model(
    version_banner: &ProbedValueOrUnavailableReason<String>,
) -> ProbedValueOrUnavailableReason<String> {
    match version_banner {
        ProbedValueOrUnavailableReason::Unavailable { reason } => {
            ProbedValueOrUnavailableReason::unavailable(format!(
                "the kernel build banner was unreadable: {reason}"
            ))
        }
        ProbedValueOrUnavailableReason::Observed { value } => value
            .split_whitespace()
            .find(|token| token.starts_with("PREEMPT"))
            .map(|token| ProbedValueOrUnavailableReason::observed(token.to_string()))
            .unwrap_or_else(|| {
                ProbedValueOrUnavailableReason::unavailable(format!(
                    "the kernel build banner `{value}` carried no PREEMPT token"
                ))
            }),
    }
}

fn probe_is_preempt_realtime(
    version_banner: &ProbedValueOrUnavailableReason<String>,
    build_configuration: Option<&ProbedValueOrUnavailableReason<String>>,
) -> ProbedValueOrUnavailableReason<bool> {
    if let Some(ProbedValueOrUnavailableReason::Observed { value }) = build_configuration {
        return ProbedValueOrUnavailableReason::observed(
            value
                .lines()
                .any(|line| line.trim() == "CONFIG_PREEMPT_RT=y"),
        );
    }
    match version_banner {
        ProbedValueOrUnavailableReason::Observed { value } => {
            ProbedValueOrUnavailableReason::observed(
                value.split_whitespace().any(|token| token == "PREEMPT_RT"),
            )
        }
        ProbedValueOrUnavailableReason::Unavailable { reason } => {
            ProbedValueOrUnavailableReason::unavailable(format!(
                "neither `/boot/config-<release>` nor the kernel build banner was readable: \
                 {reason}"
            ))
        }
    }
}

fn probe_transparent_hugepage_policy() -> ProbedValueOrUnavailableReason<String> {
    // The file lists every policy and brackets the active one:
    // `always [madvise] never`.
    match read_trimmed_file_contents(TRANSPARENT_HUGEPAGE_SYSFS_PATH) {
        ProbedValueOrUnavailableReason::Unavailable { reason } => {
            ProbedValueOrUnavailableReason::unavailable(reason)
        }
        ProbedValueOrUnavailableReason::Observed { value } => value
            .split_whitespace()
            .find_map(|token| {
                token
                    .strip_prefix('[')
                    .and_then(|token| token.strip_suffix(']'))
            })
            .map(|selected| ProbedValueOrUnavailableReason::observed(selected.to_string()))
            .unwrap_or_else(|| {
                ProbedValueOrUnavailableReason::unavailable(format!(
                    "`{TRANSPARENT_HUGEPAGE_SYSFS_PATH}` held `{value}`, which bracketed no \
                     selected policy"
                ))
            }),
    }
}

/// Field order of the single `nvidia-smi` query this module issues. Verified
/// against driver 595.84 — `clocks.applications.graphics` is deliberately
/// absent because that driver answers it with "Requested functionality has
/// been deprecated" rather than a value.
const NVIDIA_SMI_QUERY_FIELDS: &str = "name,driver_version,persistence_mode,pstate,memory.total,\
     power.limit,clocks.current.graphics,clocks.max.graphics,clocks.current.memory,\
     clocks_event_reasons.active,compute_mode";

fn probe_graphics_processing_unit() -> GraphicsProcessingUnitSpecification {
    let graphics_clock_lock_state = ProbedValueOrUnavailableReason::unavailable(
        "NVML exposes no read-back for the `--lock-gpu-clocks` range, and this probe never \
         escalates to root; the lock is a stated protocol variable, not a probed one"
            .to_string(),
    );

    let query_output = run_command_capturing_trimmed_standard_output(
        "nvidia-smi",
        &[
            &format!("--query-gpu={NVIDIA_SMI_QUERY_FIELDS}"),
            "--format=csv,noheader,nounits",
            "-i",
            "0",
        ],
    );
    let first_device_line = match query_output {
        Err(reason) => {
            return unavailable_graphics_processing_unit(&reason, graphics_clock_lock_state);
        }
        Ok(output) => match output.lines().next().map(str::to_string) {
            Some(line) => line,
            None => {
                return unavailable_graphics_processing_unit(
                    "`nvidia-smi` printed no device rows",
                    graphics_clock_lock_state,
                );
            }
        },
    };

    let fields: Vec<String> = first_device_line
        .split(',')
        .map(|field| field.trim().to_string())
        .collect();
    let expected_field_count = NVIDIA_SMI_QUERY_FIELDS.split(',').count();
    if fields.len() != expected_field_count {
        return unavailable_graphics_processing_unit(
            &format!(
                "`nvidia-smi` returned {} comma-separated fields, expected {expected_field_count}: \
                 `{first_device_line}`",
                fields.len()
            ),
            graphics_clock_lock_state,
        );
    }

    let text_field = |index: usize| -> ProbedValueOrUnavailableReason<String> {
        let field = &fields[index];
        if field.is_empty() || field.starts_with("[N/A") || field.starts_with("[Not") {
            ProbedValueOrUnavailableReason::unavailable(format!(
                "`nvidia-smi` reported `{field}` for query field {index}"
            ))
        } else {
            ProbedValueOrUnavailableReason::observed(field.clone())
        }
    };
    let numeric_field = |index: usize| -> ProbedValueOrUnavailableReason<f64> {
        match text_field(index) {
            ProbedValueOrUnavailableReason::Unavailable { reason } => {
                ProbedValueOrUnavailableReason::unavailable(reason)
            }
            ProbedValueOrUnavailableReason::Observed { value } => match value.parse::<f64>() {
                Ok(parsed) => ProbedValueOrUnavailableReason::observed(parsed),
                Err(error) => ProbedValueOrUnavailableReason::unavailable(format!(
                    "`nvidia-smi` query field {index} held `{value}`, which did not parse: {error}"
                )),
            },
        }
    };

    GraphicsProcessingUnitSpecification {
        model_name: text_field(0),
        driver_version: text_field(1),
        persistence_mode: text_field(2),
        performance_state: text_field(3),
        total_memory_mebibytes: numeric_field(4),
        power_limit_watts: numeric_field(5),
        current_graphics_clock_megahertz: numeric_field(6),
        maximum_graphics_clock_megahertz: numeric_field(7),
        current_memory_clock_megahertz: numeric_field(8),
        active_clock_event_reasons_bitmask: text_field(9),
        compute_mode: text_field(10),
        graphics_clock_lock_state,
    }
}

fn unavailable_graphics_processing_unit(
    reason: &str,
    graphics_clock_lock_state: ProbedValueOrUnavailableReason<String>,
) -> GraphicsProcessingUnitSpecification {
    let unavailable_text =
        || ProbedValueOrUnavailableReason::<String>::unavailable(reason.to_string());
    let unavailable_number =
        || ProbedValueOrUnavailableReason::<f64>::unavailable(reason.to_string());
    GraphicsProcessingUnitSpecification {
        model_name: unavailable_text(),
        driver_version: unavailable_text(),
        persistence_mode: unavailable_text(),
        performance_state: unavailable_text(),
        compute_mode: unavailable_text(),
        total_memory_mebibytes: unavailable_number(),
        power_limit_watts: unavailable_number(),
        current_graphics_clock_megahertz: unavailable_number(),
        maximum_graphics_clock_megahertz: unavailable_number(),
        current_memory_clock_megahertz: unavailable_number(),
        active_clock_event_reasons_bitmask: unavailable_text(),
        graphics_clock_lock_state,
    }
}

fn probe_memory() -> MemorySpecification {
    let meminfo_contents = match std::fs::read_to_string("/proc/meminfo") {
        Ok(contents) => contents,
        Err(error) => {
            let reason = format!("`/proc/meminfo` could not be read: {error}");
            return MemorySpecification {
                total_kibibytes: ProbedValueOrUnavailableReason::unavailable(reason.clone()),
                available_kibibytes: ProbedValueOrUnavailableReason::unavailable(reason.clone()),
                swap_total_kibibytes: ProbedValueOrUnavailableReason::unavailable(reason.clone()),
                swap_used_kibibytes: ProbedValueOrUnavailableReason::unavailable(reason.clone()),
                swap_is_enabled: ProbedValueOrUnavailableReason::unavailable(reason),
            };
        }
    };

    let read_kibibytes = |key: &str| -> ProbedValueOrUnavailableReason<u64> {
        // Every size line is `Key:<whitespace><number> kB`.
        meminfo_contents
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}:")))
            .and_then(|remainder| remainder.split_whitespace().next())
            .map(|number| match number.parse::<u64>() {
                Ok(parsed) => ProbedValueOrUnavailableReason::observed(parsed),
                Err(error) => ProbedValueOrUnavailableReason::unavailable(format!(
                    "`/proc/meminfo` line `{key}` held `{number}`, which did not parse: {error}"
                )),
            })
            .unwrap_or_else(|| {
                ProbedValueOrUnavailableReason::unavailable(format!(
                    "`/proc/meminfo` carried no `{key}` line"
                ))
            })
    };

    let swap_total_kibibytes = read_kibibytes("SwapTotal");
    let swap_free_kibibytes = read_kibibytes("SwapFree");
    let swap_used_kibibytes = match (
        swap_total_kibibytes.observed_value(),
        swap_free_kibibytes.observed_value(),
    ) {
        (Some(total), Some(free)) => {
            ProbedValueOrUnavailableReason::observed(total.saturating_sub(*free))
        }
        _ => ProbedValueOrUnavailableReason::unavailable(
            "`/proc/meminfo` did not carry both SwapTotal and SwapFree".to_string(),
        ),
    };
    let swap_is_enabled = match swap_total_kibibytes.observed_value() {
        Some(total) => ProbedValueOrUnavailableReason::observed(*total > 0),
        None => ProbedValueOrUnavailableReason::unavailable(
            "`/proc/meminfo` carried no `SwapTotal` line".to_string(),
        ),
    };

    MemorySpecification {
        total_kibibytes: read_kibibytes("MemTotal"),
        available_kibibytes: read_kibibytes("MemAvailable"),
        swap_total_kibibytes,
        swap_used_kibibytes,
        swap_is_enabled,
    }
}

/// Emitted by the interpreter the subprocess arm actually launches, so the
/// recorded numpy is the one that arm imports rather than one linked at build
/// time.
const PYTHON_RUNTIME_PROBE_SOURCE: &str = "import json, sys\n\
     report = {'executable': sys.executable, 'version': sys.version.split()[0],\n\
     'version_banner': sys.version, 'gil_disabled': not getattr(sys, '_is_gil_enabled', \
     lambda: True)()}\n\
     try:\n\
     \x20   import numpy\n\
     \x20   report['numpy_version'] = numpy.__version__\n\
     except Exception as import_failure:\n\
     \x20   report['numpy_import_failure'] = repr(import_failure)\n\
     print(json.dumps(report))\n";

fn probe_python_runtime() -> PythonRuntimeSpecification {
    let unavailable_python = |reason: String| PythonRuntimeSpecification {
        interpreter_executable_path: ProbedValueOrUnavailableReason::unavailable(reason.clone()),
        interpreter_version: ProbedValueOrUnavailableReason::unavailable(reason.clone()),
        interpreter_version_banner: ProbedValueOrUnavailableReason::unavailable(reason.clone()),
        numpy_version: ProbedValueOrUnavailableReason::unavailable(reason.clone()),
        global_interpreter_lock_is_disabled: ProbedValueOrUnavailableReason::unavailable(reason),
    };

    let report_json = match run_command_capturing_trimmed_standard_output(
        "python3",
        &["-c", PYTHON_RUNTIME_PROBE_SOURCE],
    ) {
        Ok(report_json) => report_json,
        Err(reason) => return unavailable_python(reason),
    };
    let report: serde_json::Value = match serde_json::from_str(&report_json) {
        Ok(report) => report,
        Err(error) => {
            return unavailable_python(format!(
                "the python3 probe printed `{report_json}`, which is not JSON: {error}"
            ));
        }
    };

    let text_member = |key: &str| match report.get(key).and_then(serde_json::Value::as_str) {
        Some(value) => ProbedValueOrUnavailableReason::observed(value.to_string()),
        None => ProbedValueOrUnavailableReason::unavailable(format!(
            "the python3 probe report carried no `{key}`"
        )),
    };

    let numpy_version = match report
        .get("numpy_version")
        .and_then(serde_json::Value::as_str)
    {
        Some(version) => ProbedValueOrUnavailableReason::observed(version.to_string()),
        None => ProbedValueOrUnavailableReason::unavailable(
            report
                .get("numpy_import_failure")
                .and_then(serde_json::Value::as_str)
                .map(|failure| {
                    format!("`import numpy` failed in the probed interpreter: {failure}")
                })
                .unwrap_or_else(|| {
                    "the python3 probe report carried neither `numpy_version` nor \
                     `numpy_import_failure`"
                        .to_string()
                }),
        ),
    };

    PythonRuntimeSpecification {
        interpreter_executable_path: text_member("executable"),
        interpreter_version: text_member("version"),
        interpreter_version_banner: text_member("version_banner"),
        numpy_version,
        global_interpreter_lock_is_disabled: match report
            .get("gil_disabled")
            .and_then(serde_json::Value::as_bool)
        {
            Some(is_disabled) => ProbedValueOrUnavailableReason::observed(is_disabled),
            None => ProbedValueOrUnavailableReason::unavailable(
                "the python3 probe report carried no `gil_disabled`".to_string(),
            ),
        },
    }
}

fn probe_process_scheduling() -> ProbingProcessSchedulingSpecification {
    // SAFETY: pid 0 means "the calling process"; the call reads no memory.
    let raw_policy = unsafe { libc::sched_getscheduler(0) };
    let (scheduling_policy_number, scheduling_policy_name, scheduling_policy_resets_on_fork) =
        if raw_policy < 0 {
            let error = std::io::Error::last_os_error();
            let reason = format!("`sched_getscheduler(0)` failed: {error}");
            (
                ProbedValueOrUnavailableReason::unavailable(reason.clone()),
                ProbedValueOrUnavailableReason::unavailable(reason.clone()),
                ProbedValueOrUnavailableReason::unavailable(reason),
            )
        } else {
            let resets_on_fork = raw_policy & libc::SCHED_RESET_ON_FORK != 0;
            let policy = raw_policy & !libc::SCHED_RESET_ON_FORK;
            (
                ProbedValueOrUnavailableReason::observed(policy),
                ProbedValueOrUnavailableReason::observed(scheduling_policy_name_for_number(policy)),
                ProbedValueOrUnavailableReason::observed(resets_on_fork),
            )
        };

    let mut scheduling_parameters = libc::sched_param { sched_priority: 0 };
    // SAFETY: `scheduling_parameters` is a valid, initialized out-parameter.
    let parameter_return_code = unsafe { libc::sched_getparam(0, &mut scheduling_parameters) };
    let realtime_priority = if parameter_return_code < 0 {
        ProbedValueOrUnavailableReason::unavailable(format!(
            "`sched_getparam(0)` failed: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        ProbedValueOrUnavailableReason::observed(scheduling_parameters.sched_priority)
    };

    ProbingProcessSchedulingSpecification {
        scheduling_policy_name,
        scheduling_policy_number,
        scheduling_policy_resets_on_fork,
        realtime_priority,
        niceness: probe_niceness(),
    }
}

fn scheduling_policy_name_for_number(policy: libc::c_int) -> String {
    match policy {
        libc::SCHED_OTHER => "SCHED_OTHER".to_string(),
        libc::SCHED_FIFO => "SCHED_FIFO".to_string(),
        libc::SCHED_RR => "SCHED_RR".to_string(),
        libc::SCHED_BATCH => "SCHED_BATCH".to_string(),
        libc::SCHED_IDLE => "SCHED_IDLE".to_string(),
        6 => "SCHED_DEADLINE".to_string(),
        other => format!("unrecognized scheduling policy {other}"),
    }
}

fn probe_niceness() -> ProbedValueOrUnavailableReason<i32> {
    // `getpriority` returns -1 both for the legitimate niceness -1 and for
    // failure, so errno must be cleared first and re-read to tell them apart.
    // SAFETY: `__errno_location` returns this thread's errno slot, which is
    // valid for the life of the thread.
    unsafe { *libc::__errno_location() = 0 };
    // SAFETY: `getpriority` reads no caller memory; who=0 means this process.
    let niceness = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
    // SAFETY: same slot as above.
    let errno_after_call = unsafe { *libc::__errno_location() };
    if niceness == -1 && errno_after_call != 0 {
        return ProbedValueOrUnavailableReason::unavailable(format!(
            "`getpriority(PRIO_PROCESS, 0)` failed: {}",
            std::io::Error::from_raw_os_error(errno_after_call)
        ));
    }
    ProbedValueOrUnavailableReason::observed(niceness)
}

fn probe_machine_load() -> MachineLoadSpecification {
    // `/proc/loadavg` is `<1m> <5m> <15m> <runnable>/<total> <last pid>`.
    let loadavg_contents = match read_trimmed_file_contents("/proc/loadavg") {
        ProbedValueOrUnavailableReason::Observed { value } => value,
        ProbedValueOrUnavailableReason::Unavailable { reason } => {
            return MachineLoadSpecification {
                load_average_one_minute: ProbedValueOrUnavailableReason::unavailable(
                    reason.clone(),
                ),
                load_average_five_minutes: ProbedValueOrUnavailableReason::unavailable(
                    reason.clone(),
                ),
                load_average_fifteen_minutes: ProbedValueOrUnavailableReason::unavailable(
                    reason.clone(),
                ),
                runnable_scheduling_entity_count: ProbedValueOrUnavailableReason::unavailable(
                    reason.clone(),
                ),
                total_scheduling_entity_count: ProbedValueOrUnavailableReason::unavailable(reason),
            };
        }
    };

    let fields: Vec<&str> = loadavg_contents.split_whitespace().collect();
    let load_average = |index: usize| -> ProbedValueOrUnavailableReason<f64> {
        match fields.get(index) {
            None => ProbedValueOrUnavailableReason::unavailable(format!(
                "`/proc/loadavg` held `{loadavg_contents}`, which has no field {index}"
            )),
            Some(field) => match field.parse::<f64>() {
                Ok(parsed) => ProbedValueOrUnavailableReason::observed(parsed),
                Err(error) => ProbedValueOrUnavailableReason::unavailable(format!(
                    "`/proc/loadavg` field {index} held `{field}`, which did not parse: {error}"
                )),
            },
        }
    };

    let entity_counts = fields.get(3).and_then(|field| field.split_once('/'));
    let entity_count = |selected: Option<&str>| -> ProbedValueOrUnavailableReason<u64> {
        match selected {
            None => ProbedValueOrUnavailableReason::unavailable(format!(
                "`/proc/loadavg` held `{loadavg_contents}`, whose fourth field is not \
                 `<runnable>/<total>`"
            )),
            Some(count) => match count.parse::<u64>() {
                Ok(parsed) => ProbedValueOrUnavailableReason::observed(parsed),
                Err(error) => ProbedValueOrUnavailableReason::unavailable(format!(
                    "`/proc/loadavg` entity count `{count}` did not parse: {error}"
                )),
            },
        }
    };

    MachineLoadSpecification {
        load_average_one_minute: load_average(0),
        load_average_five_minutes: load_average(1),
        load_average_fifteen_minutes: load_average(2),
        runnable_scheduling_entity_count: entity_count(entity_counts.map(|(runnable, _)| runnable)),
        total_scheduling_entity_count: entity_count(entity_counts.map(|(_, total)| total)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards against a probe path that panics or aborts the harness before a
    /// single measurement is taken, on any field.
    #[test]
    fn probing_the_machine_produces_a_serializable_specification() {
        let specification = probe_machine_specification();
        assert_eq!(
            specification.machine_specification_schema_version,
            MACHINE_SPECIFICATION_SCHEMA_VERSION
        );
        let serialized = serde_json::to_string_pretty(&specification)
            .expect("a machine specification must always serialize");
        assert!(serialized.contains("central_processing_unit"));
        assert!(serialized.contains("probe_outcome"));
    }

    /// Guards against a serde attribute change that makes `machine-spec.json`
    /// unreadable by the report tool that has to consume it later.
    #[test]
    fn the_machine_specification_json_round_trips_byte_for_byte() {
        let specification = probe_machine_specification();
        let first_serialization = serde_json::to_string_pretty(&specification)
            .expect("a machine specification must always serialize");
        let deserialized: MachineSpecification = serde_json::from_str(&first_serialization)
            .expect("a machine specification must deserialize from its own output");
        let second_serialization = serde_json::to_string_pretty(&deserialized)
            .expect("a round-tripped machine specification must serialize");
        assert_eq!(first_serialization, second_serialization);
    }

    /// Guards against the gate silently treating an unreadable knob as locked,
    /// which would let a gated cell run on an unknown machine state.
    #[test]
    fn an_unknown_knob_is_never_counted_as_a_locked_measurement_state() {
        let mut specification = probe_machine_specification();
        specification.central_processing_unit.boost_is_enabled =
            ProbedValueOrUnavailableReason::unavailable(
                "synthetic: the boost sysfs file was removed".to_string(),
            );

        assert!(!machine_is_in_locked_measurement_state(&specification));
        let violations = locked_measurement_state_violations(&specification);
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains("boost") && violation.contains("unknown")),
            "the boost violation must name the knob and say it is unknown: {violations:?}"
        );
    }

    /// Guards against an unknown knob being papered over by an owner checklist
    /// that omits the command needed to resolve it.
    #[test]
    fn an_unknown_knob_still_yields_an_owner_command_to_lock_it() {
        let mut specification = probe_machine_specification();
        specification.memory.swap_is_enabled = ProbedValueOrUnavailableReason::unavailable(
            "synthetic: /proc/meminfo was unreadable".to_string(),
        );

        let checklist = owner_checklist_to_reach_locked_measurement_state(&specification);
        let swap_command = checklist
            .iter()
            .find(|command| command.knob_description == "swap")
            .expect("an unknown swap state must still produce a swapoff command");
        assert_eq!(swap_command.shell_command, "sudo swapoff -a");
        assert!(swap_command.current_observed_state.starts_with("unknown:"));
    }

    /// Guards against a missing sysfs file on a different machine turning into a
    /// panic or an empty-string default instead of a recorded reason.
    #[test]
    fn a_probe_pointed_at_a_nonexistent_sysfs_path_records_a_reason() {
        let missing_path = "/sys/devices/system/cpu/cpufreq/policy_that_does_not_exist/governor";

        let text_probe = read_trimmed_file_contents(missing_path);
        let text_reason = text_probe
            .unavailable_reason()
            .expect("a missing sysfs file must record a reason, not an empty string");
        assert!(text_reason.contains(missing_path));
        assert!(text_probe.observed_value().is_none());

        let numeric_probe = read_file_contents_parsed::<i64>(missing_path);
        assert!(numeric_probe.unavailable_reason().is_some());

        let flag_probe = read_sysfs_boolean_flag(missing_path);
        assert!(flag_probe.unavailable_reason().is_some());
    }

    /// Guards against a sysfs file that exists but holds an unparseable value
    /// being reported as a plausible-looking zero.
    #[test]
    fn an_unparseable_sysfs_value_records_a_reason_rather_than_a_default() {
        let probe = read_file_contents_parsed::<i64>("/proc/sys/kernel/osrelease");
        let reason = probe
            .unavailable_reason()
            .expect("a kernel release string must not parse as an integer");
        assert!(reason.contains("did not parse"));
    }

    /// Guards against a reference lock step that cannot be undone, which would
    /// leave the owner's daily machine silently in measurement configuration.
    #[test]
    fn every_reference_lock_command_has_a_distinct_restore_command() {
        for command_pair in OWNER_CHECKLIST_OBSERVED_ON_REFERENCE_MACHINE {
            assert!(
                !command_pair.locking_shell_command.is_empty()
                    && !command_pair.restoring_shell_command.is_empty(),
                "`{}` is missing a command",
                command_pair.knob_description
            );
            assert_ne!(
                command_pair.locking_shell_command, command_pair.restoring_shell_command,
                "`{}` locks and restores with the same command",
                command_pair.knob_description
            );
        }
    }

    /// Guards against the reference checklist listing a command for a knob the
    /// gate does not actually require, which would send an owner to `sudo` for
    /// nothing.
    #[test]
    fn every_reference_lock_command_is_reachable_from_the_derived_checklist() {
        let mut fully_unlocked_specification = probe_machine_specification();
        let central_processing_unit = &mut fully_unlocked_specification.central_processing_unit;
        central_processing_unit.boost_is_enabled = ProbedValueOrUnavailableReason::observed(true);
        central_processing_unit.boost_control_sysfs_path =
            ProbedValueOrUnavailableReason::observed(AMD_AND_ACPI_BOOST_SYSFS_PATH.to_string());
        central_processing_unit.simultaneous_multithreading_control =
            ProbedValueOrUnavailableReason::observed("on".to_string());
        fully_unlocked_specification.memory.swap_is_enabled =
            ProbedValueOrUnavailableReason::observed(true);
        fully_unlocked_specification
            .graphics_processing_unit
            .persistence_mode = ProbedValueOrUnavailableReason::observed("Disabled".to_string());

        let derived_commands: Vec<String> =
            owner_checklist_to_reach_locked_measurement_state(&fully_unlocked_specification)
                .into_iter()
                .map(|command| command.shell_command)
                .collect();

        for command_pair in OWNER_CHECKLIST_OBSERVED_ON_REFERENCE_MACHINE {
            let lock_command = command_pair.locking_shell_command;
            // The GPU clock lock megahertz is derived from the observed maximum
            // clock, so that one entry is matched by knob rather than by value.
            let knob_prefix = lock_command
                .split_once('=')
                .map(|(knob, _)| format!("{knob}="))
                .unwrap_or_else(|| lock_command.to_string());
            assert!(
                derived_commands
                    .iter()
                    .any(|derived| derived.starts_with(&knob_prefix)),
                "`{lock_command}` is in the reference checklist but the derived checklist \
                 never emits it: {derived_commands:?}"
            );
        }
    }

    /// Guards against this crate ever acquiring a privileged probe: a password
    /// prompt in an unattended multi-hour run wedges the whole measurement.
    #[test]
    fn no_probe_path_invokes_a_privileged_command() {
        let module_source = include_str!("machine_specification_probe.rs");
        for line in module_source.lines() {
            let is_command_invocation = line.contains("Command::new");
            assert!(
                !(is_command_invocation && line.contains("sudo")),
                "a probe must never spawn sudo: {line}"
            );
        }
    }
}
