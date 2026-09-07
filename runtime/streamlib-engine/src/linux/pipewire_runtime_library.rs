// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The one `libpipewire-0.3.so.0` this process loads, and the one resolved
//! entry-point table every PipeWire arm calls through.
//!
//! `libpipewire` is opened with `libloading` and every entry point is a
//! `dlsym` result, the way `vulkan/rhi/drm_modifier_probe.rs` reaches
//! `libEGL.so.1`. Nothing links an audio or video library, so the wheel's
//! `DT_NEEDED` set does not grow — the invariant
//! `sdk/streamlib-python-wheel/tests/test_wheel_portability.py` holds.
//!
//! The half `dlopen` cannot reach at all is SPA's `static inline` pod builders
//! and parsers, which have no shared object behind them. `pipewire_audio_shim.c`
//! compiles those in and owns the X-macro list of entry-point names; this module
//! resolves that list and hands the filled table to whichever shim needs it.

use std::ffi::{CStr, c_char, c_void};
use std::sync::{Arc, OnceLock};

use libloading::Library;

/// The versioned soname, which is the only spelling that resolves on a machine
/// with no PipeWire development package: the `.so` symlink ships in `-dev`, and
/// the wheel's whole point is running where that was never installed.
const PIPEWIRE_LIBRARY_SONAME: &str = "libpipewire-0.3.so.0";

/// How much failure text a shim is given room to write.
const SHIM_FAILURE_TEXT_CAPACITY: usize = 512;

/// The entry-point list and the process-global init, owned by the audio shim's
/// translation unit and shared by every arm that calls into libpipewire.
mod shim {
    use std::ffi::{c_char, c_int, c_void};

    unsafe extern "C" {
        pub fn streamlib_pipewire_entry_point_count() -> usize;
        pub fn streamlib_pipewire_entry_point_names() -> *const *const c_char;
        pub fn streamlib_pipewire_initialize(entry_points: *const *mut c_void);
        pub fn streamlib_pipewire_loaded_library_version(
            entry_points: *const *mut c_void,
        ) -> *const c_char;
        pub fn streamlib_pipewire_daemon_answers(
            entry_points: *const *mut c_void,
            failure_text: *mut c_char,
            failure_text_capacity: usize,
        ) -> c_int;
    }
}

/// The loaded library and one resolved address per entry point the shim names,
/// in the shim's own order.
///
/// The order and the count come from the shim rather than being restated here,
/// so the two halves cannot drift: a name added to the C X-macro is a name this
/// resolves, with no Rust edit and no possibility of an off-by-one that would
/// call the wrong function.
pub(crate) struct PipeWireLibraryEntryPoints {
    /// Kept solely to hold the library open: every address below points into it.
    _library: Library,
    resolved_addresses: Vec<*mut c_void>,
}

// The addresses are entry points of a library this struct itself keeps loaded
// for as long as it lives, so they are valid from any thread and never written
// after construction.
unsafe impl Send for PipeWireLibraryEntryPoints {}
unsafe impl Sync for PipeWireLibraryEntryPoints {}

impl PipeWireLibraryEntryPoints {
    /// The process's one loaded libpipewire, resolved and `pw_init`'d on the
    /// first call and handed back unchanged after that.
    ///
    /// One table rather than one per arm: `pw_init` is process-global state,
    /// and an audio backend and a virtual camera in the same graph must not
    /// race to establish it. A failure is cached too — a machine with no
    /// libpipewire does not grow one while the process runs, and re-running
    /// `dlopen` per arm would only spend time to reach the same answer.
    pub(crate) fn loaded_once_per_process()
    -> std::result::Result<&'static Arc<Self>, &'static PipeWireLibraryUnavailableReason> {
        static LOADED: OnceLock<
            std::result::Result<Arc<PipeWireLibraryEntryPoints>, PipeWireLibraryUnavailableReason>,
        > = OnceLock::new();
        LOADED
            .get_or_init(|| {
                let entry_points = Self::resolve_from(PIPEWIRE_LIBRARY_SONAME)?;
                // SAFETY: the table is fully resolved and, held in the
                // `OnceLock`, outlives every later call.
                unsafe { shim::streamlib_pipewire_initialize(entry_points.as_ptr()) };
                Ok(Arc::new(entry_points))
            })
            .as_ref()
    }

    /// The soname is a parameter so that both refusals — the library is not
    /// there, and the library is there but exports none of this — are reachable
    /// from a test without a PipeWire-shaped stub on disk.
    pub(crate) fn resolve_from(
        library_soname: &str,
    ) -> std::result::Result<Self, PipeWireLibraryUnavailableReason> {
        // SAFETY: `dlopen` of a soname. Loading the library can run its
        // initialisers, which is what probing by opening accepts; nothing here
        // dereferences anything until `get` succeeds.
        let library = unsafe { Library::new(library_soname) }.map_err(|e| {
            PipeWireLibraryUnavailableReason(format!("{library_soname} could not be loaded: {e}"))
        })?;

        // SAFETY: both return pointers into the shim's own `static const`
        // storage, so the array and every name in it are `'static` and
        // immutable.
        let name_count = unsafe { shim::streamlib_pipewire_entry_point_count() };
        let names = unsafe { shim::streamlib_pipewire_entry_point_names() };
        let mut resolved_addresses = Vec::with_capacity(name_count);
        for name_index in 0..name_count {
            // SAFETY: `name_index < name_count`, and the shim guarantees that
            // many NUL-terminated names at that address.
            let name = unsafe { CStr::from_ptr(*names.add(name_index)) };
            // SAFETY: `dlsym` with a NUL-terminated name; the resulting address
            // is kept alive by the `Library` this struct stores beside it.
            let symbol: libloading::Symbol<'_, unsafe extern "C" fn()> =
                unsafe { library.get(name.to_bytes_with_nul()) }.map_err(|_| {
                    PipeWireLibraryUnavailableReason(format!(
                        "{library_soname} exports no {}, so this host's PipeWire is older \
                         than the 0.3.50 floor these arms bind against",
                        name.to_string_lossy()
                    ))
                })?;
            resolved_addresses.push(*symbol as *mut c_void);
        }

        Ok(Self {
            _library: library,
            resolved_addresses,
        })
    }

    /// The filled table, as every shim entry point takes it.
    pub(crate) fn as_ptr(&self) -> *const *mut c_void {
        self.resolved_addresses.as_ptr()
    }

    /// Whether a PipeWire daemon actually answers, by connecting a core and
    /// disconnecting it.
    ///
    /// An arm is chosen by opening rather than by loading: `libpipewire`
    /// present with no daemon behind it is the common container case, and it
    /// has to demote — or refuse by name — exactly as a missing library does.
    pub(crate) fn daemon_answers(&self) -> std::result::Result<(), String> {
        let mut failure_text = ShimFailureText::new();
        let (failure_text_ptr, failure_text_capacity) = failure_text.as_shim_out_parameters();
        // SAFETY: the table is fully resolved, and the out-buffer's pointer and
        // capacity come from the buffer itself, which outlives the call.
        let answered = unsafe {
            shim::streamlib_pipewire_daemon_answers(
                self.as_ptr(),
                failure_text_ptr,
                failure_text_capacity,
            )
        } == 0;
        if answered {
            Ok(())
        } else {
            Err(failure_text.read())
        }
    }

    /// The version string of the libpipewire that was actually loaded, which is
    /// what a probe log line should name — the vendored headers say what the
    /// API looks like, not what the host shipped.
    pub(crate) fn loaded_library_version(&self) -> String {
        // SAFETY: `pw_get_library_version` returns a pointer to libpipewire's
        // own static version string, valid for as long as the library is loaded.
        unsafe {
            let version = shim::streamlib_pipewire_loaded_library_version(self.as_ptr());
            if version.is_null() {
                "unknown".to_string()
            } else {
                CStr::from_ptr(version).to_string_lossy().into_owned()
            }
        }
    }
}

/// Why libpipewire could not be reached, in the words the caller's demotion or
/// refusal line should carry.
#[derive(Debug, Clone)]
pub(crate) struct PipeWireLibraryUnavailableReason(String);

impl std::fmt::Display for PipeWireLibraryUnavailableReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A buffer a shim writes a failure into, read back as an owned message.
///
/// Held as bytes rather than `c_char` so reading it needs no `unsafe` and does
/// not depend on `c_char`'s platform signedness; the cast happens once, at the
/// boundary that hands the pointer over.
pub(crate) struct ShimFailureText([u8; SHIM_FAILURE_TEXT_CAPACITY]);

impl ShimFailureText {
    pub(crate) fn new() -> Self {
        Self([0; SHIM_FAILURE_TEXT_CAPACITY])
    }

    /// The pointer and the capacity together, so the two cannot be passed to
    /// the shim disagreeing about the same buffer.
    pub(crate) fn as_shim_out_parameters(&mut self) -> (*mut c_char, usize) {
        (self.0.as_mut_ptr().cast::<c_char>(), self.0.len())
    }

    /// Read as a bounded slice rather than through `CStr::from_ptr`, so a shim
    /// that forgot to terminate its text cannot walk off the end.
    pub(crate) fn read(&self) -> String {
        CStr::from_bytes_until_nul(&self.0)
            .map(|text| text.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "PipeWire reported a failure with no readable text".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two halves agree on how many entry points there are and on their
    /// order, because only the C side states it. A Rust-side restatement is
    /// what this exists to make impossible.
    #[test]
    fn the_shim_names_every_entry_point_it_expects_rust_to_resolve() {
        let count = unsafe { shim::streamlib_pipewire_entry_point_count() };
        assert!(count > 0, "the shim names no entry points at all");

        let names = unsafe { shim::streamlib_pipewire_entry_point_names() };
        for name_index in 0..count {
            let name = unsafe { CStr::from_ptr(*names.add(name_index)) }
                .to_str()
                .expect("entry-point names are ASCII C identifiers");
            assert!(
                name.starts_with("pw_"),
                "every entry point is a libpipewire symbol, but slot {name_index} is {name:?}"
            );
        }
    }

    /// The demotion path, driven through the production loader rather than a
    /// copy of it: a container without libpipewire has to say which library it
    /// went looking for, or its log names only the arm that was chosen and
    /// never the one that was skipped.
    ///
    /// Mental revert: drop the soname from `resolve_from`'s not-found message
    /// and this fails.
    #[test]
    fn a_missing_library_demotes_and_names_the_library_it_looked_for() {
        let missing_soname = "libpipewire-0.3.so.0.streamlib-test-no-such-library";
        let Err(reason) = PipeWireLibraryEntryPoints::resolve_from(missing_soname) else {
            panic!("a library with no such file cannot load");
        };
        assert!(
            reason.to_string().contains(missing_soname),
            "the demotion reason must name the library it could not load: {reason}"
        );
    }

    /// A library that loads but exports none of this is the older-PipeWire
    /// case, and it has to name the symbol rather than crash on a null address.
    /// libc stands in for it: it always loads, and it exports no `pw_*`.
    #[test]
    fn a_library_missing_an_entry_point_names_the_symbol_rather_than_crashing() {
        let Err(reason) = PipeWireLibraryEntryPoints::resolve_from("libc.so.6") else {
            panic!("libc exports no pw_* symbol, so the table cannot resolve against it");
        };
        let reason = reason.to_string();
        assert!(
            reason.contains("libc.so.6") && reason.contains("pw_"),
            "the reason must name both the library and the symbol it wanted: {reason}"
        );
    }

    /// The shim's failure buffer is read as a bounded slice, so text that fills
    /// it exactly — or one that was never written at all — is a message rather
    /// than a walk off the end.
    #[test]
    fn failure_text_the_shim_never_wrote_reads_as_an_empty_message() {
        assert_eq!(ShimFailureText::new().read(), "");
    }
}
