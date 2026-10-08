// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::io;
use std::mem::MaybeUninit;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// The signals `tatolab` forwards to its attached `tatolabd`, one for one.
pub(crate) const FORWARDED_SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// The forwarded signals whose inherited disposition is reset to the default before listening.
///
/// A non-interactive shell starts a background command with SIGINT ignored, and XNU discards an
/// ignored signal at generation even while it is blocked, so `sigwait` would never see it. An
/// inherited ignored SIGHUP is kept: it is how `nohup` reaches `tatolabd`.
const FORWARDED_SIGNALS_RESET_TO_DEFAULT_DISPOSITION: [libc::c_int; 2] =
    [libc::SIGINT, libc::SIGTERM];

/// Block [`FORWARDED_SIGNALS`] in every thread and hand each delivery to `on_signal_delivered`
/// from a dedicated `sigwait` thread.
///
/// Call before any other thread exists: a thread spawned earlier keeps its unblocked mask and
/// takes the signal's default action. A child inherits the mask, so every child is started
/// through [`unblock_forwarded_signals_in_the_child`].
pub(crate) fn block_forwarded_signals_and_listen(
    on_signal_delivered: impl Fn(libc::c_int) + Send + 'static,
) -> io::Result<()> {
    let forwarded_signal_set = forwarded_signal_set();
    // SAFETY: the set is initialised and outlives the call; a null old-mask pointer is allowed.
    let block_result = unsafe {
        libc::pthread_sigmask(libc::SIG_BLOCK, &forwarded_signal_set, std::ptr::null_mut())
    };
    if block_result != 0 {
        return Err(io::Error::from_raw_os_error(block_result));
    }
    for reset_signal in FORWARDED_SIGNALS_RESET_TO_DEFAULT_DISPOSITION {
        // SAFETY: installing SIG_DFL registers no handler, so no handler can run unsafely.
        if unsafe { libc::signal(reset_signal, libc::SIG_DFL) } == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    std::thread::Builder::new()
        .name("tatolab-forwarded-signal-listener".to_owned())
        .spawn(move || {
            loop {
                let mut delivered_signal: libc::c_int = 0;
                // SAFETY: both pointers are to live locals this thread owns for the call.
                let wait_result =
                    unsafe { libc::sigwait(&forwarded_signal_set, &mut delivered_signal) };
                if wait_result == 0 {
                    on_signal_delivered(delivered_signal);
                }
            }
        })?;
    Ok(())
}

fn forwarded_signal_set() -> libc::sigset_t {
    // SAFETY: `sigemptyset` initialises the set before `sigaddset` and `assume_init` read it.
    unsafe {
        let mut forwarded_signal_set = MaybeUninit::<libc::sigset_t>::uninit();
        libc::sigemptyset(forwarded_signal_set.as_mut_ptr());
        for forwarded_signal in FORWARDED_SIGNALS {
            libc::sigaddset(forwarded_signal_set.as_mut_ptr(), forwarded_signal);
        }
        forwarded_signal_set.assume_init()
    }
}

/// Start `child_command`'s process with [`FORWARDED_SIGNALS`] unblocked.
///
/// `std::process::Command` hands a child the parent's signal mask, so without this a child
/// starts with every forwarded signal blocked and no signal can ever stop it.
pub(crate) fn unblock_forwarded_signals_in_the_child(child_command: &mut Command) {
    let forwarded_signal_set = forwarded_signal_set();
    // SAFETY: the closure only calls `pthread_sigmask`, which is async-signal-safe after fork.
    unsafe {
        child_command.pre_exec(move || {
            let unblock_result = libc::pthread_sigmask(
                libc::SIG_UNBLOCK,
                &forwarded_signal_set,
                std::ptr::null_mut(),
            );
            if unblock_result != 0 {
                return Err(io::Error::from_raw_os_error(unblock_result));
            }
            Ok(())
        });
    }
}
