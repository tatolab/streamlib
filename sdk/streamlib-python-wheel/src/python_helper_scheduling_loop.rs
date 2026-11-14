// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A helper process's run loop: the engine's own scheduling loop, calling the
//! Python instance's callbacks in this process and taking the parent's
//! lifecycle commands between them.

use std::os::fd::RawFd;
use std::sync::Arc;
use std::time::Duration;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use streamlib::sdk::execution::processor_scheduling_loop::{
    ProcessorSchedulingCallback, ProcessorSchedulingLoopControl, ProcessorSchedulingLoopHost,
    run_continuous_scheduling_loop_on_its_stream_clock,
    run_continuous_scheduling_loop_on_real_time, run_reactive_scheduling_loop,
};
use streamlib::sdk::iceoryx2::InputMailboxesInner;

use crate::python_helper_stream_clock::{
    HelperProcessStreamClock, helper_stream_clock_open_step_index,
};
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

/// The Python method that makes one of the processor's callbacks.
const CALL_HOOK_FROM_THE_SCHEDULING_LOOP_METHOD: &str = "call_hook_from_the_scheduling_loop";

/// The Python method that takes the lifecycle commands whose arrival woke the
/// loop, answering whether the processor still runs and whether it is paused.
const TAKE_THE_LIFECYCLE_COMMANDS_THAT_WOKE_THE_SCHEDULING_LOOP_METHOD: &str =
    "take_the_lifecycle_commands_that_woke_the_scheduling_loop";

/// Run the processor this helper hosts on the engine's scheduling loop for
/// `execution_mode`, `"reactive"` or `"continuous"`, until a lifecycle
/// command stops it.
///
/// `processor_interpreter` is the bootstrap's interpreter object: the loop
/// calls its `call_hook_from_the_scheduling_loop(hook_name)` for each
/// callback and its `take_the_lifecycle_commands_that_woke_the_scheduling_loop()`
/// whenever `lifecycle_command_arrival_fd` turns readable. An exception either
/// raises, such as the shutdown ladder's `KeyboardInterrupt`, ends the loop
/// and is raised here.
#[pyfunction]
#[pyo3(signature = (
    *,
    processor_interpreter,
    link_data_access,
    lifecycle_command_arrival_fd,
    execution_mode,
    interval_ms,
    paced_by_real_time,
    stream_clock_may_step,
    paused,
))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_the_scheduling_loop_of_this_helper(
    python: Python<'_>,
    processor_interpreter: Py<PyAny>,
    link_data_access: Py<PythonProcessorLinkDataAccess>,
    lifecycle_command_arrival_fd: RawFd,
    execution_mode: &str,
    interval_ms: u32,
    paced_by_real_time: bool,
    stream_clock_may_step: bool,
    paused: bool,
) -> PyResult<()> {
    let mut host = ProcessorSchedulingLoopHostInThisHelper {
        processor_interpreter,
        link_data_access,
        lifecycle_command_arrival_fd,
        stream_clock_may_step,
        pause_requested: paused,
        stopped: false,
        raised_from_python: None,
    };
    match execution_mode {
        "reactive" => {
            python.detach(|| run_reactive_scheduling_loop("this helper's processor", &mut host))
        }
        "continuous" if paced_by_real_time => {
            python.detach(|| run_continuous_scheduling_loop_on_real_time(&mut host, interval_ms))
        }
        "continuous" => python.detach(|| {
            run_continuous_scheduling_loop_on_its_stream_clock(
                &mut host,
                &HelperProcessStreamClock,
                interval_ms,
            )
        }),
        other => {
            return Err(PyValueError::new_err(format!(
                "execution mode {other:?} has no scheduling loop; a helper runs one for \
                 `reactive` and `continuous`"
            )));
        }
    }
    match host.raised_from_python {
        Some(raised) => Err(raised),
        None => Ok(()),
    }
}

/// A helper process's [`ProcessorSchedulingLoopHost`]: the Python processor
/// interpreter, the node's link data access, and the pipe its lifecycle
/// commands arrive on.
struct ProcessorSchedulingLoopHostInThisHelper {
    processor_interpreter: Py<PyAny>,
    link_data_access: Py<PythonProcessorLinkDataAccess>,
    lifecycle_command_arrival_fd: RawFd,
    /// Whether the stream's clock may run behind the step barrier, so the
    /// node's callbacks count on the step ledger.
    stream_clock_may_step: bool,
    /// The pause the last lifecycle command left in force.
    pause_requested: bool,
    /// Latched once a command or a Python exception stops the loop.
    stopped: bool,
    /// The first exception a Python call raised, raised again once the loop
    /// returns.
    raised_from_python: Option<PyErr>,
}

impl ProcessorSchedulingLoopHostInThisHelper {
    fn link_data_access(&self) -> &PythonProcessorLinkDataAccess {
        self.link_data_access.get()
    }

    fn stop_on(&mut self, raised: PyErr) -> ProcessorSchedulingLoopControl {
        self.raised_from_python.get_or_insert(raised);
        self.stopped = true;
        ProcessorSchedulingLoopControl::Stop
    }

    fn control_after_what_arrived(&self) -> ProcessorSchedulingLoopControl {
        if self.stopped {
            ProcessorSchedulingLoopControl::Stop
        } else {
            ProcessorSchedulingLoopControl::KeepRunning
        }
    }

    /// Wait on `fds` for up to `longest_wait`, each readable entry marked.
    fn poll_readable(fds: &mut [libc::pollfd], longest_wait: Duration) -> std::io::Result<usize> {
        let timeout_ms = longest_wait
            .as_nanos()
            .div_ceil(1_000_000)
            .min(i32::MAX as u128) as libc::c_int;
        // SAFETY: `fds` is a valid, exclusively borrowed array of pollfd for
        // the duration of the call.
        let ready_count =
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if ready_count < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(ready_count as usize)
    }

    fn lifecycle_command_arrival_pollfd(&self) -> libc::pollfd {
        libc::pollfd {
            fd: self.lifecycle_command_arrival_fd,
            events: libc::POLLIN,
            revents: 0,
        }
    }

    /// Wait for a lifecycle command for up to `longest_wait`, taking any that
    /// arrived.
    fn wait_for_a_lifecycle_command(
        &mut self,
        longest_wait: Duration,
    ) -> ProcessorSchedulingLoopControl {
        let mut fds = [self.lifecycle_command_arrival_pollfd()];
        match Self::poll_readable(&mut fds, longest_wait) {
            Ok(0) => self.control_after_what_arrived(),
            Ok(_) => self.take_the_control_its_fd_announced(),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                self.after_a_signal_interrupted_a_wait()
            }
            Err(e) => {
                tracing::warn!("this helper's wait for a lifecycle command failed: {e}");
                std::thread::sleep(longest_wait);
                self.take_arrived_control()
            }
        }
    }
}

impl ProcessorSchedulingLoopHost for ProcessorSchedulingLoopHostInThisHelper {
    fn call_processor(
        &mut self,
        callback: ProcessorSchedulingCallback,
        counted_on_the_step_ledger: bool,
    ) {
        let hook_name = match callback {
            ProcessorSchedulingCallback::OnPause => "on_pause",
            ProcessorSchedulingCallback::OnResume => "on_resume",
            ProcessorSchedulingCallback::Process => "process",
        };
        let step_ledger = (counted_on_the_step_ledger && self.stream_clock_may_step)
            .then(|| {
                self.link_data_access()
                    .loss_count_board_writer_of_this_helper()
            })
            .flatten();
        if let Some(step_ledger) = step_ledger {
            step_ledger.note_a_callback_started();
        }
        let called = Python::attach(|python| {
            self.processor_interpreter
                .call_method1(
                    python,
                    CALL_HOOK_FROM_THE_SCHEDULING_LOOP_METHOD,
                    (hook_name,),
                )
                .map(drop)
        });
        if let Some(step_ledger) = step_ledger {
            step_ledger.note_a_callback_returned(None);
        }
        if let Err(raised) = called {
            self.stop_on(raised);
        }
    }

    fn note_step_finished(&mut self, step_index: u64) {
        if let Some(step_ledger) = self
            .link_data_access()
            .loss_count_board_writer_of_this_helper()
        {
            step_ledger.note_a_callback_started();
            step_ledger.note_a_callback_returned(Some(step_index));
        }
    }

    fn pause_requested(&self) -> bool {
        self.pause_requested
    }

    fn input_mailboxes(&self) -> Option<Arc<InputMailboxesInner>> {
        self.link_data_access().input_mailboxes_of_this_helper()
    }

    fn control_arrival_fd(&self) -> Option<RawFd> {
        Some(self.lifecycle_command_arrival_fd)
    }

    fn take_arrived_control(&mut self) -> ProcessorSchedulingLoopControl {
        if self.stopped {
            return ProcessorSchedulingLoopControl::Stop;
        }
        self.wait_for_a_lifecycle_command(Duration::ZERO)
    }

    fn take_the_control_its_fd_announced(&mut self) -> ProcessorSchedulingLoopControl {
        let taken = Python::attach(|python| {
            self.processor_interpreter
                .call_method0(
                    python,
                    TAKE_THE_LIFECYCLE_COMMANDS_THAT_WOKE_THE_SCHEDULING_LOOP_METHOD,
                )
                .and_then(|answer| answer.extract::<(bool, bool)>(python))
        });
        match taken {
            Ok((still_running, paused)) => {
                self.pause_requested = paused;
                if !still_running {
                    self.stopped = true;
                }
                self.control_after_what_arrived()
            }
            Err(raised) => self.stop_on(raised),
        }
    }

    fn after_a_signal_interrupted_a_wait(&mut self) -> ProcessorSchedulingLoopControl {
        match Python::attach(|python| python.check_signals()) {
            Ok(()) => self.control_after_what_arrived(),
            Err(raised) => self.stop_on(raised),
        }
    }

    fn wait_on_the_machine_clock_or_a_control(
        &mut self,
        machine_wait: Duration,
    ) -> ProcessorSchedulingLoopControl {
        // `poll` counts whole milliseconds, so a shorter wait sleeps on the
        // machine clock and then looks for a command.
        if machine_wait < Duration::from_millis(1) {
            std::thread::sleep(machine_wait);
            return self.take_arrived_control();
        }
        self.wait_for_a_lifecycle_command(machine_wait)
    }

    fn wait_for_a_step_opened_after(
        &mut self,
        after: Option<u64>,
        longest_wait: Duration,
    ) -> Option<u64> {
        let a_step_opened_after = |step_index: Option<u64>| {
            step_index.filter(|step_index| after.is_none_or(|after| *step_index > after))
        };
        if let Some(step_index) = a_step_opened_after(helper_stream_clock_open_step_index()) {
            return Some(step_index);
        }
        let Some(publish_listener) = self
            .link_data_access()
            .stream_clock_board_publish_listener_of_this_helper()
        else {
            std::thread::sleep(longest_wait);
            return a_step_opened_after(helper_stream_clock_open_step_index());
        };
        let mut fds = [
            libc::pollfd {
                fd: publish_listener.file_descriptor(),
                events: libc::POLLIN,
                revents: 0,
            },
            self.lifecycle_command_arrival_pollfd(),
        ];
        match Self::poll_readable(&mut fds, longest_wait) {
            Ok(_) => {
                if fds[0].revents & libc::POLLIN != 0 {
                    publish_listener.take_every_publish_noted();
                }
                if fds[1].revents & libc::POLLIN != 0 {
                    self.take_the_control_its_fd_announced();
                    return None;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                self.after_a_signal_interrupted_a_wait();
                return None;
            }
            Err(e) => {
                tracing::warn!("this helper's wait for its next step failed: {e}");
                std::thread::sleep(longest_wait);
            }
        }
        a_step_opened_after(helper_stream_clock_open_step_index())
    }
}
