// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;

use crate::core::ProcessorDescriptor;
use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::descriptors::SchemaIdent;
use crate::core::error::{Error, Result};
use crate::core::execution::ExecutionConfig;
use crate::core::graph::{PortInfo, ProcessorNode};
use crate::core::processors::{Config, DynGeneratedProcessor, GeneratedProcessor};
use crate::core::pubsub::{Event, PUBSUB, RuntimeEvent, topics};
use streamlib_plugin_abi::ProcessorVTable;
use streamlib_processor_schema::PortSchemaSpec;

/// Scratch buffer the vtable's error-out-params write into. 512 B is
/// enough for the typical "config deserialize failed" message; the
/// vtable's `write_err` truncates cleanly past that.
const VTABLE_ERR_BUF_CAP: usize = 512;

/// A created processor instance for runtime use.
///
/// Two-variant: cdylib registrations (via `STREAMLIB_PLUGIN`) and
/// in-process `PROCESSOR_REGISTRY.register::<P>()` calls both land in
/// [`Self::VTable`] (dispatch via extern "C" fn pointers, retiring
/// the dyn-trait crossing class); legacy non-generic registrations
/// (subprocess host wrappers via [`ProcessorInstanceFactory::register_dynamic`])
/// land in [`Self::LegacyDyn`] (dispatch via Rust trait-object
/// methods, host-only).
///
/// # Iceoryx2 resource ownership (issue #894)
///
/// The host allocates the inner `OutputWriterInner` and
/// `InputMailboxesInner` Arcs at instance-construction time and
/// retains them on the `VTable` variant via the
/// `iceoryx2_output_writer_inner` / `iceoryx2_input_mailboxes_inner`
/// fields. The cdylib's `outputs` / `inputs` PluginAbiObject fields receive
/// `Arc::into_raw`-cloned handles via `set_iceoryx2_resources`.
/// Connection-wiring code on the host operates on the inner Arc
/// directly (no plugin ABI hop).
pub enum ProcessorInstance {
    /// Vtable-dispatched processor — cdylib registrations (via
    /// `STREAMLIB_PLUGIN`) and in-process `register::<P>()` calls
    /// both land here. `instance_ptr` is a
    /// `Box::into_raw(Box::<P>::new(...))` allocation on the
    /// registering artifact's heap (cdylib for plugin loads, host for
    /// in-process registration). Dropped via `vtable.destroy`.
    ///
    /// `any_placeholder` is a ZST anchor whose `&mut` reference
    /// satisfies the `as_any_mut() -> &mut dyn Any` shape without
    /// touching the cdylib-side processor. Downcasts to host-only
    /// subprocess-host types fall through to `None` as expected
    /// (cdylib processors are never subprocess hosts).
    ///
    /// `iceoryx2_output_writer_inner` / `iceoryx2_input_mailboxes_inner`
    /// hold the host's per-instance allocation (issue #894). `None`
    /// for processors without outputs / inputs.
    VTable {
        instance_ptr: *mut c_void,
        vtable: &'static ProcessorVTable,
        any_placeholder: (),
        iceoryx2_output_writer_inner: Option<Arc<crate::iceoryx2::OutputWriterInner>>,
        iceoryx2_input_mailboxes_inner: Option<Arc<crate::iceoryx2::InputMailboxesInner>>,
        /// `true` when the vtable's function pointers target a
        /// cdylib's address space (loaded via `STREAMLIB_PLUGIN`);
        /// `false` when they target the host's address space
        /// (`register::<P>()`). Lifecycle dispatch
        /// ([`Self::setup`] / [`Self::teardown`]) consults this to
        /// pick the cdylib-shaped `ScopeToken` FullAccess wrap vs
        /// the in-process Boxed dispatch — see the variant's doc
        /// at [`super::RegistrationKind`].
        cdylib_resident: bool,
    },
    /// Host-static dyn-trait registration. Used by subprocess host
    /// wrappers (Python / Deno) that register a `Box<dyn Fn>`
    /// constructor via [`ProcessorInstanceFactory::register_dynamic`].
    /// No plugin ABI crossing — these live in the host and
    /// dispatch via standard Rust trait objects.
    LegacyDyn(Box<dyn DynGeneratedProcessor + Send>),
}

// Safety: VTable's `*mut c_void` is bound to the registering artifact's
// process address space, which lives for the process lifetime
// (cdylibs are pinned via `LOADED_PLUGIN_LIBRARIES`). LegacyDyn's
// inner Box<dyn ... + Send> is already Send.
unsafe impl Send for ProcessorInstance {}

impl Drop for ProcessorInstance {
    fn drop(&mut self) {
        if let Self::VTable {
            instance_ptr,
            vtable,
            ..
        } = self
        {
            if !instance_ptr.is_null() {
                // SAFETY: instance_ptr came from the same artifact's
                // Box::into_raw via vtable.construct; destroy
                // performs Box::from_raw + drop on that artifact's heap.
                unsafe {
                    (vtable.destroy)(*instance_ptr);
                }
            }
        }
    }
}

impl ProcessorInstance {
    /// Issue one vtable lifecycle call against the VTable variant.
    /// Returns the host-side error chained off the extern "C" return
    /// code + scratch buffer.
    fn vtable_call_full(
        instance_ptr: *mut c_void,
        method: unsafe extern "C" fn(*mut c_void, *const c_void, *mut u8, usize, *mut usize) -> i32,
        ctx: &RuntimeContextFullAccess<'_>,
        method_name: &str,
    ) -> Result<()> {
        let mut err_buf = [0u8; VTABLE_ERR_BUF_CAP];
        let mut err_len = 0usize;
        let rc = unsafe {
            method(
                instance_ptr,
                ctx as *const RuntimeContextFullAccess<'_> as *const c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            let msg = std::str::from_utf8(&err_buf[..err_len])
                .unwrap_or("<non-utf8 error>")
                .to_string();
            Err(Error::Runtime(format!("{method_name}: {msg}")))
        }
    }

    fn vtable_call_limited(
        instance_ptr: *mut c_void,
        method: unsafe extern "C" fn(*mut c_void, *const c_void, *mut u8, usize, *mut usize) -> i32,
        ctx: &RuntimeContextLimitedAccess<'_>,
        method_name: &str,
    ) -> Result<()> {
        let mut err_buf = [0u8; VTABLE_ERR_BUF_CAP];
        let mut err_len = 0usize;
        let rc = unsafe {
            method(
                instance_ptr,
                ctx as *const RuntimeContextLimitedAccess<'_> as *const c_void,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            let msg = std::str::from_utf8(&err_buf[..err_len])
                .unwrap_or("<non-utf8 error>")
                .to_string();
            Err(Error::Runtime(format!("{method_name}: {msg}")))
        }
    }

    /// Run the processor's `setup` lifecycle.
    ///
    /// Dispatch shape depends on where the body's code lives:
    /// - **`VTable { cdylib_resident: true }`** (loaded via
    ///   `STREAMLIB_PLUGIN` dlopen): wraps the call in
    ///   [`RuntimeContextFullAccess::with_cdylib_scope`] so the
    ///   cdylib body's `ctx.gpu_full_access()` is `ScopeToken`-
    ///   flavored and dispatches through the FullAccess vtable
    ///   instead of tripping the Boxed branch's `host_inner` panic
    ///   guard. Scope acquisition serializes against other
    ///   privileged GPU work (same gate as in-process escalate)
    ///   and ends with `wait_device_idle`.
    /// - **`VTable { cdylib_resident: false }`** (in-process
    ///   `register::<P>()`): serialize via
    ///   `gpu_limited_access().escalate(|_| ...)`, matching the
    ///   pre-#912 `processor_setup_lock` contract — the in-process
    ///   body uses the Boxed FullAccess on `ctx` directly (no
    ///   vtable hop) and the escalate wrap provides the gate +
    ///   wait-idle. Using `with_cdylib_scope` for the in-process
    ///   VTable case would hand the body a `ScopeToken` handle
    ///   whose memory layout (a u64 serial) doesn't match
    ///   `Box<Arc<GpuContext>>`, and any FullAccess method that
    ///   reaches `host_inner()` without first matching `handle_kind`
    ///   (e.g. `device()`) would execute UB instead of the expected
    ///   direct deref.
    /// - **`LegacyDyn`** (subprocess host wrappers — Python /
    ///   TypeScript via `register_dynamic`): pure passthrough, no
    ///   gate wrap. Subprocess host setup does no host-side GPU
    ///   work — it spawns the child, constructs the bridge, sends a
    ///   `setup` lifecycle, then blocks on the subprocess's `ready`
    ///   reply. Wrapping that IPC wait in escalate would hold the
    ///   gate against every escalate the subprocess issues during
    ///   its own init — the bridge-reader thread dispatches each
    ///   `escalate_request` inline through `sandbox.escalate(|full|
    ///   ...)` and would deadlock on the same gate. Per-call
    ///   bridge-handler escalates still acquire the gate + wait
    ///   device idle on their own, so GPU-resource serialization
    ///   is preserved (#867).
    ///
    /// For the wrapped variants the escalate gate is held for
    /// exactly one nesting depth across the setup body, so a body
    /// that itself calls `.escalate(...)` trips the gate's
    /// same-thread re-entry panic instead of silently deadlocking.
    pub fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                cdylib_resident: true,
                ..
            } => {
                let instance_ptr = *instance_ptr;
                let setup_fn = vtable.setup;
                ctx.with_cdylib_scope(|cdylib_ctx| {
                    Self::vtable_call_full(instance_ptr, setup_fn, cdylib_ctx, "setup")
                })
            }
            Self::VTable {
                instance_ptr,
                vtable,
                cdylib_resident: false,
                ..
            } => {
                let instance_ptr = *instance_ptr;
                let setup_fn = vtable.setup;
                let sandbox = ctx.gpu_limited_access().clone();
                sandbox
                    .escalate(|_full| Self::vtable_call_full(instance_ptr, setup_fn, ctx, "setup"))
            }
            Self::LegacyDyn(inner) => inner.__generated_setup(ctx),
        }
    }

    /// Run the processor's `teardown` lifecycle.
    ///
    /// Mirrors [`Self::setup`]'s variant-aware dispatch — see that
    /// doc for the cdylib-resident vs in-process VTable vs
    /// LegacyDyn shape rationale.
    pub fn teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                cdylib_resident: true,
                ..
            } => {
                let instance_ptr = *instance_ptr;
                let teardown_fn = vtable.teardown;
                ctx.with_cdylib_scope(|cdylib_ctx| {
                    Self::vtable_call_full(instance_ptr, teardown_fn, cdylib_ctx, "teardown")
                })
            }
            Self::VTable {
                instance_ptr,
                vtable,
                cdylib_resident: false,
                ..
            } => {
                let instance_ptr = *instance_ptr;
                let teardown_fn = vtable.teardown;
                let sandbox = ctx.gpu_limited_access().clone();
                sandbox.escalate(|_full| {
                    Self::vtable_call_full(instance_ptr, teardown_fn, ctx, "teardown")
                })
            }
            Self::LegacyDyn(inner) => inner.__generated_teardown(ctx),
        }
    }

    /// Run the processor's `on_pause` hook.
    pub fn on_pause(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                ..
            } => Self::vtable_call_limited(*instance_ptr, vtable.on_pause, ctx, "on_pause"),
            Self::LegacyDyn(inner) => inner.__generated_on_pause(ctx),
        }
    }

    /// Run the processor's `on_resume` hook.
    pub fn on_resume(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                ..
            } => Self::vtable_call_limited(*instance_ptr, vtable.on_resume, ctx, "on_resume"),
            Self::LegacyDyn(inner) => inner.__generated_on_resume(ctx),
        }
    }

    /// Run one tick of the processor's `process` body.
    pub fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                ..
            } => Self::vtable_call_limited(*instance_ptr, vtable.process, ctx, "process"),
            Self::LegacyDyn(inner) => inner.process(ctx),
        }
    }

    /// Start a Manual-mode processor.
    ///
    /// Variant-aware dispatch matching the historical "FullAccess
    /// in signature → direct access in body" contract for every
    /// runtime variant:
    /// - **`VTable { cdylib_resident: true }`**: wraps in
    ///   [`RuntimeContextFullAccess::with_cdylib_scope`] so cdylib
    ///   bodies see a `ScopeToken` FullAccess (direct access
    ///   becomes vtable dispatch — no `host_inner()` panic).
    /// - **`VTable { cdylib_resident: false }`** (in-process
    ///   `register::<P>()`) and **`LegacyDyn`** (subprocess hosts):
    ///   pure passthrough. Historical: start/stop were never
    ///   gate-wrapped (thread_runner calls them directly), so adding
    ///   a wrap here would change semantics for in-process bodies
    ///   that legitimately escalate or do their own thread spawning.
    ///   In-process bodies use `ctx.gpu_full_access()` directly
    ///   (Boxed deref, host-only); subprocess host bodies do their
    ///   own per-call gate management via the bridge handlers (#867
    ///   contract).
    pub fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                cdylib_resident: true,
                ..
            } => {
                let instance_ptr = *instance_ptr;
                let start_fn = vtable.start;
                ctx.with_cdylib_scope(|cdylib_ctx| {
                    Self::vtable_call_full(instance_ptr, start_fn, cdylib_ctx, "start")
                })
            }
            Self::VTable {
                instance_ptr,
                vtable,
                cdylib_resident: false,
                ..
            } => Self::vtable_call_full(*instance_ptr, vtable.start, ctx, "start"),
            Self::LegacyDyn(inner) => inner.start(ctx),
        }
    }

    /// Stop a Manual-mode processor.
    ///
    /// Mirrors [`Self::start`]'s variant-aware dispatch — see that
    /// doc for the rationale.
    pub fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                cdylib_resident: true,
                ..
            } => {
                let instance_ptr = *instance_ptr;
                let stop_fn = vtable.stop;
                ctx.with_cdylib_scope(|cdylib_ctx| {
                    Self::vtable_call_full(instance_ptr, stop_fn, cdylib_ctx, "stop")
                })
            }
            Self::VTable {
                instance_ptr,
                vtable,
                cdylib_resident: false,
                ..
            } => Self::vtable_call_full(*instance_ptr, vtable.stop, ctx, "stop"),
            Self::LegacyDyn(inner) => inner.stop(ctx),
        }
    }

    /// Read the processor's execution config. For VTable variants
    /// the call crosses extern "C" once; for LegacyDyn it dispatches
    /// through the trait object.
    pub fn execution_config(&self) -> ExecutionConfig {
        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                ..
            } => {
                let mut buf = [0u8; 64];
                let mut out_len = 0usize;
                let required = unsafe {
                    (vtable.execution_config_msgpack)(
                        *instance_ptr,
                        buf.as_mut_ptr(),
                        buf.len(),
                        &mut out_len as *mut usize,
                    )
                };
                if required == 0 || required > buf.len() {
                    // Either no payload or too-big payload (won't
                    // happen for ExecutionConfig in practice). Fall
                    // back to default.
                    return ExecutionConfig::default();
                }
                match rmp_serde::from_slice(&buf[..out_len]) {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "ProcessorInstance::execution_config: failed to decode msgpack payload; falling back to default",
                        );
                        ExecutionConfig::default()
                    }
                }
            }
            Self::LegacyDyn(inner) => inner.execution_config(),
        }
    }

    pub fn has_iceoryx2_outputs(&self) -> bool {
        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                ..
            } => unsafe { (vtable.has_iceoryx2_outputs)(*instance_ptr) },
            Self::LegacyDyn(inner) => inner.has_iceoryx2_outputs(),
        }
    }

    pub fn has_iceoryx2_inputs(&self) -> bool {
        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                ..
            } => unsafe { (vtable.has_iceoryx2_inputs)(*instance_ptr) },
            Self::LegacyDyn(inner) => inner.has_iceoryx2_inputs(),
        }
    }

    /// Borrow the host-side `OutputWriterInner` Arc this processor
    /// instance is wired to. Returns `None` if the processor has no
    /// output ports.
    ///
    /// Used by the host's connection-wiring path (compiler ops) to
    /// mutate the inner directly via
    /// [`crate::iceoryx2::OutputWriterInner::add_connection`] —
    /// no plugin ABI hop to the cdylib.
    pub fn iceoryx2_output_writer_inner(&self) -> Option<Arc<crate::iceoryx2::OutputWriterInner>> {
        match self {
            Self::VTable {
                iceoryx2_output_writer_inner,
                ..
            } => iceoryx2_output_writer_inner.clone(),
            Self::LegacyDyn(inner) => inner.iceoryx2_output_writer_inner(),
        }
    }

    /// Borrow the host-side `InputMailboxesInner` Arc this
    /// processor instance is wired to. Returns `None` if the
    /// processor has no input ports.
    ///
    /// Used by the host's wiring + scheduler paths to call
    /// `add_port`, `set_subscriber`, `set_listener`, `listener_fd`,
    /// `drain_listener`, `any_port_has_data`, etc. directly — all
    /// host-side, no plugin ABI hop to the cdylib.
    pub fn iceoryx2_input_mailboxes_inner(
        &self,
    ) -> Option<Arc<crate::iceoryx2::InputMailboxesInner>> {
        match self {
            Self::VTable {
                iceoryx2_input_mailboxes_inner,
                ..
            } => iceoryx2_input_mailboxes_inner.clone(),
            Self::LegacyDyn(inner) => inner.iceoryx2_input_mailboxes_inner(),
        }
    }

    /// Install host-allocated iceoryx2 inner Arcs into this
    /// processor instance. Called once by the factory after
    /// `construct` returns; the host owns the Arcs and clones them
    /// into the cdylib via `set_iceoryx2_resources`.
    ///
    /// Returns the resulting error (if any) from the cdylib's
    /// `set_iceoryx2_resources` vtable slot, plus stashes the Arcs
    /// on `self` so subsequent
    /// `iceoryx2_output_writer_inner` / `iceoryx2_input_mailboxes_inner`
    /// calls see them.
    pub fn install_iceoryx2_resources(&mut self) -> Result<()> {
        let needs_outputs = self.has_iceoryx2_outputs();
        let needs_inputs = self.has_iceoryx2_inputs();
        let output_inner =
            needs_outputs.then(|| Arc::new(crate::iceoryx2::OutputWriterInner::new()));
        let input_inner =
            needs_inputs.then(|| Arc::new(crate::iceoryx2::InputMailboxesInner::new()));

        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                iceoryx2_output_writer_inner,
                iceoryx2_input_mailboxes_inner,
                ..
            } => {
                // Stash host-side Arcs first so the connection-
                // wiring path can see them even if the vtable hop
                // returns an error.
                *iceoryx2_output_writer_inner = output_inner.clone();
                *iceoryx2_input_mailboxes_inner = input_inner.clone();

                // Build the (handle, vtable) pairs for the cdylib.
                let output_writer_handle = output_inner
                    .as_ref()
                    .map(|arc| Arc::into_raw(arc.clone()) as *const c_void)
                    .unwrap_or(std::ptr::null());
                let output_writer_vtable = if output_inner.is_some() {
                    crate::core::plugin::host_services::host_output_writer_vtable()
                } else {
                    std::ptr::null()
                };
                let input_mailboxes_handle = input_inner
                    .as_ref()
                    .map(|arc| Arc::into_raw(arc.clone()) as *const c_void)
                    .unwrap_or(std::ptr::null());
                let input_mailboxes_vtable = if input_inner.is_some() {
                    crate::core::plugin::host_services::host_input_mailboxes_vtable()
                } else {
                    std::ptr::null()
                };

                let mut err_buf = [0u8; VTABLE_ERR_BUF_CAP];
                let mut err_len = 0usize;
                let rc = unsafe {
                    (vtable.set_iceoryx2_resources)(
                        *instance_ptr,
                        output_writer_handle,
                        output_writer_vtable,
                        input_mailboxes_handle,
                        input_mailboxes_vtable,
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if rc == 0 {
                    Ok(())
                } else {
                    // The cdylib refused the install; balance the
                    // leaked Arc handles so we don't leak refs.
                    if !output_writer_handle.is_null() {
                        unsafe {
                            Arc::<crate::iceoryx2::OutputWriterInner>::decrement_strong_count(
                                output_writer_handle as *const _,
                            );
                        }
                    }
                    if !input_mailboxes_handle.is_null() {
                        unsafe {
                            Arc::<crate::iceoryx2::InputMailboxesInner>::decrement_strong_count(
                                input_mailboxes_handle as *const _,
                            );
                        }
                    }
                    let msg = std::str::from_utf8(&err_buf[..err_len])
                        .unwrap_or("<non-utf8 error>")
                        .to_string();
                    Err(Error::Runtime(format!("set_iceoryx2_resources: {msg}")))
                }
            }
            Self::LegacyDyn(inner) => {
                let ow = output_inner
                    .clone()
                    .map(crate::iceoryx2::OutputWriter::from_inner_arc);
                let im = input_inner
                    .clone()
                    .map(crate::iceoryx2::InputMailboxes::from_inner_arc);
                inner.set_iceoryx2_resources(ow, im)
            }
        }
    }

    pub fn apply_config_json(&mut self, config_json: &serde_json::Value) -> Result<()> {
        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                ..
            } => {
                let bytes = rmp_serde::to_vec_named(config_json).map_err(|e| {
                    Error::Configuration(format!("apply_config_json msgpack encode: {e}"))
                })?;
                let mut err_buf = [0u8; VTABLE_ERR_BUF_CAP];
                let mut err_len = 0usize;
                let rc = unsafe {
                    (vtable.apply_config_msgpack)(
                        *instance_ptr,
                        bytes.as_ptr(),
                        bytes.len(),
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if rc == 0 {
                    Ok(())
                } else {
                    let msg = std::str::from_utf8(&err_buf[..err_len])
                        .unwrap_or("<non-utf8 error>")
                        .to_string();
                    Err(Error::Configuration(format!("apply_config_json: {msg}")))
                }
            }
            Self::LegacyDyn(inner) => inner.apply_config_json(config_json),
        }
    }

    pub fn to_runtime_json(&self) -> serde_json::Value {
        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                ..
            } => {
                let mut buf = vec![0u8; 4096];
                let mut out_len = 0usize;
                let required = unsafe {
                    (vtable.to_runtime_msgpack)(
                        *instance_ptr,
                        buf.as_mut_ptr(),
                        buf.len(),
                        &mut out_len as *mut usize,
                    )
                };
                if required == 0 {
                    return serde_json::Value::Null;
                }
                if required > buf.len() {
                    // Resize and retry. Runtime-state payloads in
                    // practice fit well under 4 KiB, but this keeps
                    // the contract honest.
                    buf.resize(required, 0);
                    let _ = unsafe {
                        (vtable.to_runtime_msgpack)(
                            *instance_ptr,
                            buf.as_mut_ptr(),
                            buf.len(),
                            &mut out_len as *mut usize,
                        )
                    };
                }
                match rmp_serde::from_slice(&buf[..out_len]) {
                    Ok(value) => value,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "ProcessorInstance::to_runtime_json: failed to decode msgpack payload; returning Null",
                        );
                        serde_json::Value::Null
                    }
                }
            }
            Self::LegacyDyn(inner) => inner.to_runtime_json(),
        }
    }

    pub fn config_json(&self) -> serde_json::Value {
        match self {
            Self::VTable {
                instance_ptr,
                vtable,
                ..
            } => {
                let mut buf = vec![0u8; 4096];
                let mut out_len = 0usize;
                let required = unsafe {
                    (vtable.config_msgpack)(
                        *instance_ptr,
                        buf.as_mut_ptr(),
                        buf.len(),
                        &mut out_len as *mut usize,
                    )
                };
                if required == 0 {
                    return serde_json::Value::Null;
                }
                if required > buf.len() {
                    buf.resize(required, 0);
                    let _ = unsafe {
                        (vtable.config_msgpack)(
                            *instance_ptr,
                            buf.as_mut_ptr(),
                            buf.len(),
                            &mut out_len as *mut usize,
                        )
                    };
                }
                match rmp_serde::from_slice(&buf[..out_len]) {
                    Ok(value) => value,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "ProcessorInstance::config_json: failed to decode msgpack payload; returning Null",
                        );
                        serde_json::Value::Null
                    }
                }
            }
            Self::LegacyDyn(inner) => inner.config_json(),
        }
    }

    /// Downcast handle. Only meaningful for the LegacyDyn variant —
    /// cdylib-registered processors return a placeholder reference
    /// that downcasts to nothing. Used by the host's compiler ops to
    /// reach host-only subprocess host wrappers
    /// (`PythonNativeSubprocessHostProcessor`, `DenoSubprocessHostProcessor`)
    /// which only register via the legacy path.
    pub fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        match self {
            Self::LegacyDyn(inner) => inner.as_any_mut(),
            Self::VTable {
                any_placeholder, ..
            } => any_placeholder,
        }
    }
}

/// Legacy-path factory function signature used by
/// [`ProcessorInstanceFactory::register_dynamic`] for subprocess
/// host wrappers (Python / Deno) that don't fit the generic vtable
/// monomorphization shape.
pub type DynamicProcessorConstructorFn =
    Box<dyn Fn(&ProcessorNode) -> Result<Box<dyn DynGeneratedProcessor + Send>> + Send + Sync>;

/// Per-type registration entry the factory stores.
enum RegistrationKind {
    /// VTable-based dispatch. Used by both cdylib registrations
    /// (extern "C" wrappers landing in the cdylib's address space) and
    /// inventory-registered host processors (extern "C" wrappers
    /// landing in the host's address space).
    ///
    /// `cdylib_resident` distinguishes the two — `true` when the
    /// vtable's function pointers target a cdylib's address space
    /// (loaded via `STREAMLIB_PLUGIN` dlopen), `false` when they
    /// target the host's address space (`register::<P>()`). The
    /// lifecycle dispatch in [`ProcessorInstance::setup`] /
    /// [`ProcessorInstance::teardown`] consults this flag to pick
    /// between the cdylib-shaped `ScopeToken` FullAccess wrap and
    /// the in-process Boxed FullAccess dispatch — only cdylib-
    /// resident bodies need the vtable hop to dodge the
    /// `host_inner()` panic guard. Mis-tagging an in-process
    /// VTable as `cdylib_resident: true` would route its `device()`
    /// (and other `host_inner()`-only) calls through an opaque
    /// scope token whose memory layout doesn't match `Box<Arc<…>>`
    /// — UB.
    VTable {
        vtable: &'static ProcessorVTable,
        cdylib_resident: bool,
    },
    /// Box<dyn Fn> closure constructor — used for subprocess host
    /// wrappers via `register_dynamic`.
    LegacyDyn {
        constructor: DynamicProcessorConstructorFn,
    },
}

/// Everything the registry held for one processor type, removed by
/// [`ProcessorInstanceFactory::unregister_processor_types`] and held so a
/// refused `remove_module` can reinstate the registration exactly.
pub(crate) struct UnregisteredProcessorTypeRecord {
    processor_type: SchemaIdent,
    registration: Option<RegistrationKind>,
    port_info: Option<(Vec<PortInfo>, Vec<PortInfo>)>,
    descriptor: Option<ProcessorDescriptor>,
}

/// Factory for compile-time registered Rust processors.
pub struct ProcessorInstanceFactory {
    registrations: RwLock<HashMap<SchemaIdent, RegistrationKind>>,
    port_info: RwLock<HashMap<SchemaIdent, (Vec<PortInfo>, Vec<PortInfo>)>>,
    descriptors: RwLock<HashMap<SchemaIdent, ProcessorDescriptor>>,
    /// Set of port-data-type schema specs ([`PortSchemaSpec`]).
    /// Orthogonal to the processor-identity HashMaps above — tracks the
    /// universe of port schemas any registered processor exposes, for
    /// `known_schemas()` / `is_schema_known()` debugging surface only.
    schemas: RwLock<HashSet<PortSchemaSpec>>,
}

/// Global processor registry for runtime lookups.
///
/// Starts empty. Callers populate it through one of two paths:
///
/// - **Cdylib packages** loaded via `runtime.add_module(...)` register
///   their processors through the plugin ABI's `STREAMLIB_PLUGIN`
///   symbol, which calls the host's `processor_register` callback
///   (see [`crate::core::plugin::host_services`]).
/// - **In-process Rust callers** invoke
///   [`ProcessorInstanceFactory::register`] (typed) or
///   [`ProcessorInstanceFactory::register_dynamic`] (subprocess host
///   wrappers) directly on the registry.
pub static PROCESSOR_REGISTRY: LazyLock<ProcessorInstanceFactory> =
    LazyLock::new(ProcessorInstanceFactory::new);

impl Default for ProcessorInstanceFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessorInstanceFactory {
    pub fn new() -> Self {
        Self {
            registrations: RwLock::new(HashMap::new()),
            port_info: RwLock::new(HashMap::new()),
            descriptors: RwLock::new(HashMap::new()),
            schemas: RwLock::new(HashSet::new()),
        }
    }

    /// Register a processor type with the vtable shape. Monomorphizes a
    /// `&'static ProcessorVTable` for `P` and stores it alongside the
    /// processor's descriptor + port info.
    pub fn register<P>(&self)
    where
        P: GeneratedProcessor + 'static,
        P::Config: Config,
    {
        let descriptor = match <P as GeneratedProcessor>::descriptor() {
            Some(d) => d,
            None => {
                tracing::warn!(
                    "Processor {} has no descriptor, skipping registration",
                    std::any::type_name::<P>()
                );
                return;
            }
        };

        let vtable = crate::core::plugin::processor_vtable::vtable_for::<P>();

        // In-process registration — vtable's function pointers
        // target the host's address space; lifecycle dispatch uses
        // the Boxed FullAccess directly (no plugin ABI hop).
        if let Err(e) =
            self.register_via_vtable(descriptor, vtable, /* cdylib_resident */ false)
        {
            tracing::warn!(
                "Processor registration for {} failed: {}",
                std::any::type_name::<P>(),
                e
            );
        }
    }

    /// Insert a descriptor + vtable pair under the descriptor's
    /// structured ident. Idempotent on `(ident)` keys — a duplicate
    /// registration logs `debug!` and skips.
    ///
    /// `cdylib_resident` is `true` when the vtable's function
    /// pointers target a cdylib's address space (loaded via
    /// `STREAMLIB_PLUGIN`) and `false` when they target the host's
    /// address space (`register::<P>()`). The flag propagates onto
    /// the [`ProcessorInstance::VTable`] variant so lifecycle
    /// dispatch can pick the right FullAccess shape.
    ///
    /// Used by:
    /// - `register::<P>()` (inventory + in-tree host-side
    ///   registrations) — passes the vtable from `vtable_for::<P>()`
    ///   with `cdylib_resident: false`.
    /// - The cdylib-bridge `processor_register` callback in
    ///   `core::plugin::host_services` — passes the cdylib's
    ///   `&'static ProcessorVTable` with `cdylib_resident: true`.
    pub fn register_via_vtable(
        &self,
        descriptor: ProcessorDescriptor,
        vtable: &'static ProcessorVTable,
        cdylib_resident: bool,
    ) -> Result<()> {
        let type_name = descriptor.name.clone();

        // Duplicate check FIRST — a duplicate must leave all four maps
        // untouched. The previous ordering overwrote `port_info` /
        // `schemas` / `descriptors` before checking `registrations`, so a
        // duplicate re-registration could leave the four maps describing
        // two different descriptors.
        if self.registrations.read().contains_key(&type_name) {
            tracing::debug!(
                "Processor '{}' already registered, skipping duplicate",
                type_name
            );
            return Ok(());
        }

        let inputs: Vec<PortInfo> = descriptor
            .inputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
                overflow: p.overflow.clone(),
            })
            .collect();

        let outputs: Vec<PortInfo> = descriptor
            .outputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
                overflow: p.overflow.clone(),
            })
            .collect();

        self.port_info
            .write()
            .insert(type_name.clone(), (inputs.clone(), outputs.clone()));

        {
            let mut schemas = self.schemas.write();
            for port in inputs.iter().chain(outputs.iter()) {
                schemas.insert(port.data_type.clone());
            }
        }

        self.descriptors
            .write()
            .insert(type_name.clone(), descriptor);

        self.registrations.write().insert(
            type_name.clone(),
            RegistrationKind::VTable {
                vtable,
                cdylib_resident,
            },
        );

        tracing::info!("[register] new processor type registered '{}'", type_name);

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRegisterProcessorType {
                processor_type: type_name.clone(),
            }),
        );

        Ok(())
    }

    /// Register a processor dynamically at runtime with a non-generic
    /// `Box<dyn Fn>` constructor. Used for subprocess host wrappers
    /// (Python / Deno) where the constructor isn't expressible as a
    /// generic `register::<P>()` call.
    ///
    /// # Arguments
    /// * `descriptor` - Processor metadata including name, ports, and config schema
    /// * `constructor` - Factory function that creates processor instances
    ///
    /// # Returns
    /// Error if a processor with the same name is already registered.
    pub fn register_dynamic(
        &self,
        descriptor: ProcessorDescriptor,
        constructor: DynamicProcessorConstructorFn,
    ) -> Result<()> {
        let type_name = descriptor.name.clone();

        // Check for duplicate registration
        if self.registrations.read().contains_key(&type_name) {
            return Err(Error::Configuration(format!(
                "Processor '{}' already registered",
                type_name
            )));
        }

        // Build port info from descriptor
        let inputs: Vec<PortInfo> = descriptor
            .inputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
                overflow: p.overflow.clone(),
            })
            .collect();

        let outputs: Vec<PortInfo> = descriptor
            .outputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
                overflow: p.overflow.clone(),
            })
            .collect();

        self.port_info
            .write()
            .insert(type_name.clone(), (inputs.clone(), outputs.clone()));

        {
            let mut schemas = self.schemas.write();
            for port in inputs.iter().chain(outputs.iter()) {
                schemas.insert(port.data_type.clone());
            }
        }

        self.descriptors
            .write()
            .insert(type_name.clone(), descriptor);

        self.registrations.write().insert(
            type_name.clone(),
            RegistrationKind::LegacyDyn { constructor },
        );

        tracing::info!(
            "[register_dynamic] new processor type registered '{}'",
            type_name
        );

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRegisterProcessorType {
                processor_type: type_name.clone(),
            }),
        );

        Ok(())
    }

    /// Register a processor descriptor without a constructor.
    ///
    /// Used for subprocess processors (Python, TypeScript) where no Rust-side
    /// `ProcessorInstance` is created. The graph needs the descriptor and port info
    /// for validation and wiring, but `create()` will return an error if called.
    pub fn register_descriptor_only(&self, descriptor: ProcessorDescriptor) -> Result<()> {
        let type_name = descriptor.name.clone();

        if self.descriptors.read().contains_key(&type_name) {
            return Err(Error::Configuration(format!(
                "Processor '{}' already registered",
                type_name
            )));
        }

        let inputs: Vec<PortInfo> = descriptor
            .inputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
                overflow: p.overflow.clone(),
            })
            .collect();

        let outputs: Vec<PortInfo> = descriptor
            .outputs
            .iter()
            .map(|p| PortInfo {
                name: p.name.clone(),
                data_type: p.schema.clone(),
                port_kind: Default::default(),
                overflow: p.overflow.clone(),
            })
            .collect();

        self.port_info
            .write()
            .insert(type_name.clone(), (inputs.clone(), outputs.clone()));

        {
            let mut schemas = self.schemas.write();
            for port in inputs.iter().chain(outputs.iter()) {
                schemas.insert(port.data_type.clone());
            }
        }

        self.descriptors
            .write()
            .insert(type_name.clone(), descriptor);

        // No constructor registered - create() will fail with ProcessorNotFound,
        // which is correct since subprocess processors are never instantiated in Rust.

        tracing::info!(
            "[register_descriptor_only] subprocess processor type registered '{}'",
            type_name
        );

        PUBSUB.publish(
            topics::RUNTIME_GLOBAL,
            &Event::RuntimeGlobal(RuntimeEvent::RuntimeDidRegisterProcessorType {
                processor_type: type_name.clone(),
            }),
        );

        Ok(())
    }

    /// Remove every registry entry for the given processor types across
    /// all four maps (`registrations`, `port_info`, `descriptors`, plus a
    /// rebuild of the port-schema universe). Returns the removed entries
    /// so a refused `remove_module` can reinstate them exactly via
    /// [`Self::reinstate_unregistered_processor_types`]. Idents with no
    /// entry are skipped.
    pub(crate) fn unregister_processor_types(
        &self,
        processor_type_idents: &[SchemaIdent],
    ) -> Vec<UnregisteredProcessorTypeRecord> {
        let mut removed = Vec::new();
        for ident in processor_type_idents {
            let registration = self.registrations.write().remove(ident);
            let port_info = self.port_info.write().remove(ident);
            let descriptor = self.descriptors.write().remove(ident);
            if registration.is_none() && port_info.is_none() && descriptor.is_none() {
                continue;
            }
            removed.push(UnregisteredProcessorTypeRecord {
                processor_type: ident.clone(),
                registration,
                port_info,
                descriptor,
            });
        }
        if !removed.is_empty() {
            self.rebuild_port_schema_universe_from_descriptors();
        }
        removed
    }

    /// Reinstate entries previously removed by
    /// [`Self::unregister_processor_types`] — the restore half of
    /// `remove_module`'s remove-then-check-then-restore in-use check.
    pub(crate) fn reinstate_unregistered_processor_types(
        &self,
        unregistered: Vec<UnregisteredProcessorTypeRecord>,
    ) {
        if unregistered.is_empty() {
            return;
        }
        for record in unregistered {
            if let Some(registration) = record.registration {
                self.registrations
                    .write()
                    .insert(record.processor_type.clone(), registration);
            }
            if let Some(port_info) = record.port_info {
                self.port_info
                    .write()
                    .insert(record.processor_type.clone(), port_info);
            }
            if let Some(descriptor) = record.descriptor {
                self.descriptors
                    .write()
                    .insert(record.processor_type.clone(), descriptor);
            }
        }
        self.rebuild_port_schema_universe_from_descriptors();
    }

    /// Recompute the port-schema universe from the remaining descriptors.
    /// The `schemas` set is additive-only on registration, so removal has
    /// to rebuild it — a schema stays known only while some registered
    /// processor still exposes it on a port.
    fn rebuild_port_schema_universe_from_descriptors(&self) {
        let rebuilt: HashSet<PortSchemaSpec> = self
            .descriptors
            .read()
            .values()
            .flat_map(|descriptor| descriptor.inputs.iter().chain(descriptor.outputs.iter()))
            .map(|port| port.schema.clone())
            .collect();
        *self.schemas.write() = rebuilt;
    }

    pub fn can_create(&self, processor_type: &SchemaIdent) -> bool {
        self.registrations.read().contains_key(processor_type)
    }

    pub fn create(&self, node: &ProcessorNode) -> Result<ProcessorInstance> {
        let registrations = self.registrations.read();
        let registration = registrations.get(&node.processor_type).ok_or_else(|| {
            Error::ProcessorNotFound(format!(
                "No factory registered for processor type '{}'",
                node.processor_type
            ))
        })?;

        match registration {
            RegistrationKind::VTable {
                vtable,
                cdylib_resident,
            } => {
                // Serialize node.config (serde_json::Value) to msgpack
                // for the cdylib's construct fn to deserialize into
                // P::Config.
                let config_msgpack = match &node.config {
                    Some(json) => rmp_serde::to_vec_named(json).map_err(|e| {
                        Error::Configuration(format!(
                            "Failed to encode config to msgpack for '{}': {}",
                            node.id, e
                        ))
                    })?,
                    None => Vec::new(),
                };

                let mut err_buf = [0u8; VTABLE_ERR_BUF_CAP];
                let mut err_len = 0usize;
                let ptr = unsafe {
                    (vtable.construct)(
                        config_msgpack.as_ptr(),
                        config_msgpack.len(),
                        err_buf.as_mut_ptr(),
                        err_buf.len(),
                        &mut err_len as *mut usize,
                    )
                };
                if ptr.is_null() {
                    let msg = std::str::from_utf8(&err_buf[..err_len])
                        .unwrap_or("<non-utf8 error>")
                        .to_string();
                    return Err(Error::Configuration(format!(
                        "construct for '{}': {}",
                        node.processor_type, msg
                    )));
                }
                let mut instance = ProcessorInstance::VTable {
                    instance_ptr: ptr,
                    vtable: *vtable,
                    any_placeholder: (),
                    iceoryx2_output_writer_inner: None,
                    iceoryx2_input_mailboxes_inner: None,
                    cdylib_resident: *cdylib_resident,
                };
                // Issue #894: host-allocates iceoryx2 inner Arcs +
                // hands the cdylib opaque (handle, vtable) PluginAbiObjects
                // via the new `set_iceoryx2_resources` slot.
                instance.install_iceoryx2_resources()?;
                Ok(instance)
            }
            RegistrationKind::LegacyDyn { constructor } => {
                let mut instance = ProcessorInstance::LegacyDyn(constructor(node)?);
                instance.install_iceoryx2_resources()?;
                Ok(instance)
            }
        }
    }

    pub fn port_info(
        &self,
        processor_type: &SchemaIdent,
    ) -> Option<(Vec<PortInfo>, Vec<PortInfo>)> {
        self.port_info.read().get(processor_type).cloned()
    }

    pub fn is_registered(&self, processor_type: &SchemaIdent) -> bool {
        self.registrations.read().contains_key(processor_type)
    }

    /// Get the descriptor for a processor type, if registered.
    pub fn descriptor(&self, processor_type: &SchemaIdent) -> Option<ProcessorDescriptor> {
        self.descriptors.read().get(processor_type).cloned()
    }

    /// List all registered processor types with their full descriptors.
    pub fn list_registered(&self) -> Vec<ProcessorDescriptor> {
        self.descriptors.read().values().cloned().collect()
    }

    /// Resolve `(org, package, type)` against the registry by picking the
    /// highest-`SemVer` match across all registered idents. Returns
    /// [`Error::UnknownProcessorType`] when nothing matches.
    ///
    /// Iterates over `descriptors` (the truth for registered idents),
    /// not `registrations`, so subprocess-only processors registered via
    /// [`Self::register_descriptor_only`] participate in resolution.
    pub fn resolve_any_version(
        &self,
        org: &crate::core::descriptors::Org,
        package: &crate::core::descriptors::Package,
        type_name: &crate::core::descriptors::TypeName,
    ) -> Result<SchemaIdent> {
        let descriptors = self.descriptors.read();
        let highest = descriptors
            .keys()
            .filter(|id| &id.org == org && &id.package == package && &id.r#type == type_name)
            .max_by_key(|id| id.version.clone())
            .cloned();
        highest.ok_or_else(|| Error::UnknownProcessorType {
            // No version was supplied; we render the search target as
            // `(org, package, type)@0.0.0` so the diagnostic still names
            // the offending tuple. Callers who want the exact "any
            // version" semantics in the message string should match on
            // the variant and re-render.
            ident: SchemaIdent::new(
                org.clone(),
                package.clone(),
                type_name.clone(),
                crate::core::descriptors::SemVer::new(0, 0, 0),
            ),
        })
    }

    /// All known port-schema specs from registered processor ports,
    /// sorted by Display rendering for diff-stable output.
    pub fn known_schemas(&self) -> Vec<PortSchemaSpec> {
        let mut schemas: Vec<PortSchemaSpec> = self.schemas.read().iter().cloned().collect();
        schemas.sort_by(|a, b| a.to_string().cmp(&b.to_string()));
        schemas
    }

    /// Check if a port-schema spec is known from any registered processor port.
    pub fn is_schema_known(&self, schema: &PortSchemaSpec) -> bool {
        self.schemas.read().contains(schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::descriptors::{Org, Package, SemVer, TypeName};

    fn ident(org: &str, pkg: &str, ty: &str, v: SemVer) -> SchemaIdent {
        SchemaIdent::new(
            Org::new(org).unwrap(),
            Package::new(pkg).unwrap(),
            TypeName::new(ty).unwrap(),
            v,
        )
    }

    fn unit_descriptor(name: SchemaIdent) -> ProcessorDescriptor {
        ProcessorDescriptor::new(name, "test")
    }

    #[test]
    fn identical_pascal_case_from_different_org_package_pairs_coexist() {
        // Two packages each ship a `Camera` processor — same PascalCase
        // short name, different `(org, package)` pair. Pre-#707 this
        // collided in the `String`-keyed registry; post-#707 the
        // structured key disambiguates them and both registrations
        // succeed cleanly.
        let factory = ProcessorInstanceFactory::new();

        let camera_a = ident("acme", "core", "Camera", SemVer::new(1, 0, 0));
        let camera_b = ident("contoso", "core", "Camera", SemVer::new(1, 0, 0));

        factory
            .register_descriptor_only(unit_descriptor(camera_a.clone()))
            .expect("first Camera must register cleanly");
        factory
            .register_descriptor_only(unit_descriptor(camera_b.clone()))
            .expect(
                "second Camera (different org) must register cleanly — \
                 the structured key disambiguates @acme/core/Camera@1.0.0 \
                 from @contoso/core/Camera@1.0.0",
            );

        assert!(factory.descriptor(&camera_a).is_some());
        assert!(factory.descriptor(&camera_b).is_some());
        assert_eq!(factory.list_registered().len(), 2);
    }

    #[test]
    fn duplicate_full_4_tuple_returns_clear_error() {
        // Two registrations of the SAME structured ident must fail with
        // an actionable error variant — the new typed key doesn't
        // accidentally tolerate exact 4-tuple collisions.
        let factory = ProcessorInstanceFactory::new();
        let id = ident("acme", "core", "Camera", SemVer::new(1, 0, 0));

        factory
            .register_descriptor_only(unit_descriptor(id.clone()))
            .expect("first registration succeeds");

        let err = factory
            .register_descriptor_only(unit_descriptor(id.clone()))
            .expect_err("duplicate 4-tuple must be rejected");

        match err {
            Error::Configuration(msg) => {
                assert!(
                    msg.contains("already registered"),
                    "error must name the collision; got: {msg}"
                );
                // The Display form of the offending ident is in the
                // message — that's what humans need to see.
                assert!(
                    msg.contains("@acme/core/Camera@1.0.0"),
                    "error must render the structured ident; got: {msg}"
                );
            }
            other => panic!("expected Configuration variant; got {other:?}"),
        }
    }

    #[test]
    fn version_difference_disambiguates_otherwise_identical_ident() {
        // Major-version bumps of the same `(org, package, type)` are
        // distinct registrations — locks the package-as-publication-unit
        // invariant from the milestone description.
        let factory = ProcessorInstanceFactory::new();
        let v1 = ident("acme", "core", "Camera", SemVer::new(1, 0, 0));
        let v2 = ident("acme", "core", "Camera", SemVer::new(2, 0, 0));

        factory
            .register_descriptor_only(unit_descriptor(v1.clone()))
            .unwrap();
        factory
            .register_descriptor_only(unit_descriptor(v2.clone()))
            .unwrap();

        assert!(factory.descriptor(&v1).is_some());
        assert!(factory.descriptor(&v2).is_some());
    }

    #[test]
    fn resolve_any_version_picks_highest_semver_when_multiple_registered() {
        let factory = ProcessorInstanceFactory::new();
        let org = Org::new("acme").unwrap();
        let pkg = Package::new("core").unwrap();
        let ty = TypeName::new("Camera").unwrap();

        let v1 = SchemaIdent::new(org.clone(), pkg.clone(), ty.clone(), SemVer::new(1, 0, 0));
        let v2 = SchemaIdent::new(org.clone(), pkg.clone(), ty.clone(), SemVer::new(1, 2, 0));
        let v3 = SchemaIdent::new(org.clone(), pkg.clone(), ty.clone(), SemVer::new(2, 0, 0));

        // Insert out of order to prove the resolver picks max, not last-inserted.
        factory
            .register_descriptor_only(unit_descriptor(v2.clone()))
            .unwrap();
        factory
            .register_descriptor_only(unit_descriptor(v3.clone()))
            .unwrap();
        factory
            .register_descriptor_only(unit_descriptor(v1.clone()))
            .unwrap();

        let resolved = factory.resolve_any_version(&org, &pkg, &ty).unwrap();
        assert_eq!(
            resolved, v3,
            "resolve_any_version must return the highest semver"
        );
    }

    #[test]
    fn resolve_any_version_returns_unknown_processor_type_when_nothing_matches() {
        let factory = ProcessorInstanceFactory::new();
        // Register an unrelated ident — must not satisfy the lookup.
        factory
            .register_descriptor_only(unit_descriptor(ident(
                "other",
                "core",
                "Camera",
                SemVer::new(1, 0, 0),
            )))
            .unwrap();

        let org = Org::new("acme").unwrap();
        let pkg = Package::new("core").unwrap();
        let ty = TypeName::new("Camera").unwrap();

        let err = factory.resolve_any_version(&org, &pkg, &ty).unwrap_err();
        match err {
            Error::UnknownProcessorType { ident } => {
                assert_eq!(ident.org, org);
                assert_eq!(ident.package, pkg);
                assert_eq!(ident.r#type, ty);
            }
            other => panic!("expected UnknownProcessorType, got {other:?}"),
        }
    }

    #[test]
    fn resolve_any_version_does_not_cross_org_or_package_or_type_boundaries() {
        let factory = ProcessorInstanceFactory::new();

        // Same type name + version, different (org, package) tuples must
        // not satisfy a lookup against the wrong tuple.
        factory
            .register_descriptor_only(unit_descriptor(ident(
                "acme",
                "core",
                "Camera",
                SemVer::new(1, 0, 0),
            )))
            .unwrap();
        factory
            .register_descriptor_only(unit_descriptor(ident(
                "acme",
                "audio",
                "Camera",
                SemVer::new(9, 9, 9),
            )))
            .unwrap();
        factory
            .register_descriptor_only(unit_descriptor(ident(
                "contoso",
                "core",
                "Camera",
                SemVer::new(9, 9, 9),
            )))
            .unwrap();
        factory
            .register_descriptor_only(unit_descriptor(ident(
                "acme",
                "core",
                "Microphone",
                SemVer::new(9, 9, 9),
            )))
            .unwrap();

        let resolved = factory
            .resolve_any_version(
                &Org::new("acme").unwrap(),
                &Package::new("core").unwrap(),
                &TypeName::new("Camera").unwrap(),
            )
            .unwrap();
        assert_eq!(resolved.version, SemVer::new(1, 0, 0));
    }
}
