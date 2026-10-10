// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Every process signal `tatolab` handles: the signals that stop the stream `run` and `dev` hold
//! attached, and the interrupt that ends a `logs` read.

use std::io;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, Ordering};

/// The signals that stop an attached stream, less a SIGHUP inherited as ignored.
pub(crate) const ATTACHED_STREAM_STOP_SIGNALS: [libc::c_int; 3] =
    [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// The stop signals whose inherited disposition is reset to the default before listening.
///
/// A non-interactive shell starts a background command with SIGINT ignored, and XNU discards an
/// ignored signal at generation even while it is blocked, so `sigwait` would never see it. An
/// inherited ignored SIGHUP is kept: it is how `nohup` keeps an attached stream past a closed
/// terminal.
const ATTACHED_STREAM_STOP_SIGNALS_RESET_TO_DEFAULT_DISPOSITION: [libc::c_int; 2] =
    [libc::SIGINT, libc::SIGTERM];

/// Set by SIGINT while `logs` reads the disk, so Ctrl-C ends the read — a `--follow` above all —
/// with exit 0 rather than killing the process mid-line.
static INTERRUPT_DELIVERED_DURING_THE_READ: AtomicBool = AtomicBool::new(false);

/// Whether this process inherited `signal` with its disposition set to ignore; asked before
/// `tatolab` changes the disposition, so the answer is the one its parent left.
pub(crate) fn signal_was_inherited_as_ignored(signal: libc::c_int) -> io::Result<bool> {
    // SAFETY: a null `act` is POSIX's read-only query; `inherited_disposition` is a zeroed
    // `sigaction` this frame owns for the kernel to write into.
    let inherited_disposition = unsafe {
        let mut inherited_disposition: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(signal, std::ptr::null(), &mut inherited_disposition) != 0 {
            return Err(io::Error::last_os_error());
        }
        inherited_disposition
    };
    Ok(inherited_disposition.sa_sigaction == libc::SIG_IGN)
}

/// Block [`ATTACHED_STREAM_STOP_SIGNALS`] in every thread and hand each delivery to
/// `on_signal_delivered` from a dedicated `sigwait` thread, leaving a SIGHUP inherited as ignored
/// unblocked and ignored.
///
/// Linux queues a blocked signal whatever its disposition, so a blocked SIGHUP that `nohup` told
/// `tatolab` to ignore would still reach `sigwait` and stop the stream.
///
/// Call before any other thread exists: a thread spawned earlier keeps its unblocked mask and
/// takes the signal's default action.
pub(crate) fn block_the_stop_signals_and_listen(
    on_signal_delivered: impl Fn(libc::c_int) + Send + 'static,
) -> io::Result<()> {
    let mut stop_signals_to_listen_for = Vec::with_capacity(ATTACHED_STREAM_STOP_SIGNALS.len());
    for stop_signal in ATTACHED_STREAM_STOP_SIGNALS {
        let hangup_inherited_as_ignored =
            stop_signal == libc::SIGHUP && signal_was_inherited_as_ignored(libc::SIGHUP)?;
        if !hangup_inherited_as_ignored {
            stop_signals_to_listen_for.push(stop_signal);
        }
    }
    let stop_signal_set_to_listen_for = signal_set_of(&stop_signals_to_listen_for);
    // SAFETY: the set is initialised and outlives the call; a null old-mask pointer is allowed.
    let block_result = unsafe {
        libc::pthread_sigmask(
            libc::SIG_BLOCK,
            &stop_signal_set_to_listen_for,
            std::ptr::null_mut(),
        )
    };
    if block_result != 0 {
        return Err(io::Error::from_raw_os_error(block_result));
    }
    for reset_signal in ATTACHED_STREAM_STOP_SIGNALS_RESET_TO_DEFAULT_DISPOSITION {
        // SAFETY: installing SIG_DFL registers no handler, so no handler can run unsafely.
        if unsafe { libc::signal(reset_signal, libc::SIG_DFL) } == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    std::thread::Builder::new()
        .name("tatolab-stop-signal-listener".to_owned())
        .spawn(move || {
            loop {
                let mut delivered_signal: libc::c_int = 0;
                // SAFETY: both pointers are to live locals this thread owns for the call.
                let wait_result =
                    unsafe { libc::sigwait(&stop_signal_set_to_listen_for, &mut delivered_signal) };
                if wait_result == 0 {
                    on_signal_delivered(delivered_signal);
                }
            }
        })?;
    Ok(())
}

fn signal_set_of(signals: &[libc::c_int]) -> libc::sigset_t {
    // SAFETY: `sigemptyset` initialises the set before `sigaddset` and `assume_init` read it.
    unsafe {
        let mut signal_set = MaybeUninit::<libc::sigset_t>::uninit();
        libc::sigemptyset(signal_set.as_mut_ptr());
        for &signal in signals {
            libc::sigaddset(signal_set.as_mut_ptr(), signal);
        }
        signal_set.assume_init()
    }
}

extern "C" fn record_interrupt_delivered_during_the_read(_delivered_signal: libc::c_int) {
    INTERRUPT_DELIVERED_DURING_THE_READ.store(true, Ordering::SeqCst);
}

/// Route SIGINT to the flag [`an_interrupt_was_delivered_during_the_read`] reads. A SIGINT
/// inherited as ignored stays ignored.
pub(crate) fn end_the_read_on_interrupt() -> io::Result<()> {
    if signal_was_inherited_as_ignored(libc::SIGINT)? {
        return Ok(());
    }
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe; the action is
    // fully initialised before it is installed, and a null old-action pointer is allowed.
    unsafe {
        let mut interrupt_action: libc::sigaction = std::mem::zeroed();
        interrupt_action.sa_sigaction =
            record_interrupt_delivered_during_the_read as extern "C" fn(libc::c_int) as usize;
        libc::sigemptyset(&mut interrupt_action.sa_mask);
        interrupt_action.sa_flags = libc::SA_RESTART;
        if libc::sigaction(libc::SIGINT, &interrupt_action, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Whether a SIGINT has arrived since [`end_the_read_on_interrupt`] routed it.
pub(crate) fn an_interrupt_was_delivered_during_the_read() -> bool {
    INTERRUPT_DELIVERED_DURING_THE_READ.load(Ordering::SeqCst)
}
