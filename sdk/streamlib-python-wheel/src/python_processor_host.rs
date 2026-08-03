// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine-side host for a processor authored in Python.
//!
//! One of these runs on each Python processor's dedicated engine thread. It
//! attaches the GIL for exactly the span of a user callback and holds it for
//! nothing else — not the wait between ticks, not the reactive loop's block on
//! an input, not teardown's join. That is what lets a processor parked in a
//! native call leave every other Python processor running.

use pyo3::prelude::*;
use streamlib::sdk::descriptors::ProcessorDescriptor;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::execution::ExecutionConfig;
use streamlib::sdk::graph::ProcessorNode;
use streamlib::sdk::iceoryx2::{InputMailboxes, OutputWriter};
use streamlib::sdk::processors::DynGeneratedProcessor;

use crate::python_bag_conversion::json_value_to_python_object;
use crate::python_processor_declaration::PythonProcessorDeclaration;
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

/// The module holding the Python half of processor construction.
const PROCESSOR_HOSTING_MODULE: &str = "streamlib._processor_hosting";

/// A lifecycle callback a processor class may define.
///
/// All of them are optional — a filter implementing only `process` is the
/// common case — so which ones exist is resolved once at construction rather
/// than rediscovered on every tick.
#[derive(Clone, Copy)]
enum ProcessorLifecycleHook {
    Setup,
    Teardown,
    Process,
    Start,
    Stop,
    OnPause,
    OnResume,
}

impl ProcessorLifecycleHook {
    const ALL: [Self; 7] = [
        Self::Setup,
        Self::Teardown,
        Self::Process,
        Self::Start,
        Self::Stop,
        Self::OnPause,
        Self::OnResume,
    ];

    fn python_method_name(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Teardown => "teardown",
            Self::Process => "process",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::OnPause => "on_pause",
            Self::OnResume => "on_resume",
        }
    }
}

pub(crate) struct PythonProcessorHost {
    /// The user's own object. Constructed on the engine's compile thread as the
    /// graph comes up, then used only from this processor's own dedicated
    /// thread — a different thread from the one that built it.
    ///
    /// `Option` so [`Drop`] can take it and release it while attached.
    processor_instance: Option<Py<PyAny>>,
    link_data_access: Option<Py<PythonProcessorLinkDataAccess>>,
    /// Which hooks the class defines, indexed by [`ProcessorLifecycleHook`].
    declared_hooks: [bool; ProcessorLifecycleHook::ALL.len()],
    descriptor: ProcessorDescriptor,
    execution_config: ExecutionConfig,
    /// Names this processor in every log line and error — the graph's display
    /// name, which is what the author sees in `streamlib graph`.
    processor_display_name: String,
}

impl PythonProcessorHost {
    /// Construct the user's processor object and bind its declared ports.
    pub(crate) fn construct(
        declaration: &PythonProcessorDeclaration,
        processor_class: &Py<PyAny>,
        node: &ProcessorNode,
    ) -> Result<Self> {
        let processor_display_name = node.display_name.clone();
        let held_display_name = processor_display_name.clone();
        let configuration = node.config.clone();

        Python::attach(move |python| -> PyResult<Self> {
            let link_data_access = Py::new(python, PythonProcessorLinkDataAccess::new())?;

            let hosting = python.import(PROCESSOR_HOSTING_MODULE)?;
            let processor_instance = hosting.getattr("construct_processor_instance")?.call1((
                processor_class.bind(python),
                configuration
                    .as_ref()
                    .map(|config| json_value_to_python_object(python, config))
                    .transpose()?,
                link_data_access.bind(python),
            ))?;

            let mut declared_hooks = [false; ProcessorLifecycleHook::ALL.len()];
            for hook in ProcessorLifecycleHook::ALL {
                declared_hooks[hook as usize] =
                    processor_instance.hasattr(hook.python_method_name())?;
            }

            Ok(Self {
                processor_instance: Some(processor_instance.unbind()),
                link_data_access: Some(link_data_access),
                declared_hooks,
                descriptor: declaration.descriptor.clone(),
                execution_config: declaration.execution_config,
                processor_display_name: held_display_name,
            })
        })
        .map_err(|construction_failure| {
            Error::Runtime(format_python_failure(
                &processor_display_name,
                "could not be constructed",
                construction_failure,
            ))
        })
    }

    /// Call a lifecycle hook, if the class defined one.
    ///
    /// A class that did not costs no GIL acquisition at all — which is the
    /// per-tick path for a processor driven by something other than `process`.
    fn dispatch_hook(&mut self, hook: ProcessorLifecycleHook) -> Result<()> {
        if !self.declared_hooks[hook as usize] {
            return Ok(());
        }
        let Some(processor_instance) = self.processor_instance.as_ref() else {
            return Ok(());
        };

        Python::attach(|python| -> PyResult<()> {
            processor_instance
                .bind(python)
                .call_method0(hook.python_method_name())?;
            Ok(())
        })
        .map_err(|hook_failure| {
            Error::Runtime(format_python_failure(
                &self.processor_display_name,
                &format!("raised in {}()", hook.python_method_name()),
                hook_failure,
            ))
        })
    }

    fn link_data_access(&self) -> Option<&PythonProcessorLinkDataAccess> {
        self.link_data_access.as_ref().map(|access| access.get())
    }
}

/// Render a Python exception with its traceback, so a processor failure reads
/// like a Python failure rather than a one-line summary of one.
fn format_python_failure(processor_display_name: &str, what: &str, failure: PyErr) -> String {
    let rendered = Python::attach(|python| match failure.traceback(python) {
        Some(traceback) => match traceback.format() {
            Ok(formatted) => format!("{formatted}{}", failure.value(python)),
            Err(_) => failure.to_string(),
        },
        None => failure.to_string(),
    });
    format!("[{processor_display_name}] {what}:\n{rendered}")
}

impl DynGeneratedProcessor for PythonProcessorHost {
    fn __generated_setup(
        &mut self,
        _ctx: &streamlib::sdk::context::RuntimeContextFullAccess<'_>,
    ) -> Result<()> {
        self.dispatch_hook(ProcessorLifecycleHook::Setup)
    }

    fn __generated_teardown(
        &mut self,
        _ctx: &streamlib::sdk::context::RuntimeContextFullAccess<'_>,
    ) -> Result<()> {
        self.dispatch_hook(ProcessorLifecycleHook::Teardown)
    }

    fn __generated_on_pause(
        &mut self,
        _ctx: &streamlib::sdk::context::RuntimeContextLimitedAccess<'_>,
    ) -> Result<()> {
        self.dispatch_hook(ProcessorLifecycleHook::OnPause)
    }

    fn __generated_on_resume(
        &mut self,
        _ctx: &streamlib::sdk::context::RuntimeContextLimitedAccess<'_>,
    ) -> Result<()> {
        self.dispatch_hook(ProcessorLifecycleHook::OnResume)
    }

    fn process(
        &mut self,
        _ctx: &streamlib::sdk::context::RuntimeContextLimitedAccess<'_>,
    ) -> Result<()> {
        self.dispatch_hook(ProcessorLifecycleHook::Process)
    }

    fn start(
        &mut self,
        _ctx: &streamlib::sdk::context::RuntimeContextFullAccess<'_>,
    ) -> Result<()> {
        self.dispatch_hook(ProcessorLifecycleHook::Start)
    }

    fn stop(&mut self, _ctx: &streamlib::sdk::context::RuntimeContextFullAccess<'_>) -> Result<()> {
        self.dispatch_hook(ProcessorLifecycleHook::Stop)
    }

    fn name(&self) -> &str {
        &self.processor_display_name
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        Some(self.descriptor.clone())
    }

    fn execution_config(&self) -> ExecutionConfig {
        self.execution_config
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        !self.descriptor.outputs.is_empty()
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        !self.descriptor.inputs.is_empty()
    }

    fn set_iceoryx2_resources(
        &mut self,
        output_writer: Option<OutputWriter>,
        input_mailboxes: Option<InputMailboxes>,
    ) -> Result<()> {
        // Reached for the inner Arc rather than kept as the PluginAbiObject
        // pair: the inner is an ordinary `Send + Sync` Rust value that the
        // Python-facing object can hold, and calling it skips the vtable hop
        // the host would otherwise take to reach itself.
        let Some(link_data_access) = self.link_data_access() else {
            return Ok(());
        };
        if let Some(output_writer) = output_writer.and_then(|writer| writer.inner_arc()) {
            link_data_access.install_output_writer(output_writer);
        }
        if let Some(input_mailboxes) = input_mailboxes.and_then(|mailboxes| mailboxes.inner_arc()) {
            link_data_access.install_input_mailboxes(input_mailboxes);
        }
        Ok(())
    }

    fn iceoryx2_output_writer_inner(
        &self,
    ) -> Option<std::sync::Arc<streamlib::sdk::iceoryx2::OutputWriterInner>> {
        self.link_data_access()?.output_writer_inner()
    }

    fn iceoryx2_input_mailboxes_inner(
        &self,
    ) -> Option<std::sync::Arc<streamlib::sdk::iceoryx2::InputMailboxesInner>> {
        self.link_data_access()?.input_mailboxes_inner()
    }

    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> Result<()> {
        let Some(processor_instance) = self.processor_instance.as_ref() else {
            return Ok(());
        };
        Python::attach(|python| -> PyResult<()> {
            let hosting = python.import(PROCESSOR_HOSTING_MODULE)?;
            hosting.getattr("apply_configuration")?.call1((
                processor_instance.bind(python),
                json_value_to_python_object(python, config_json)?,
            ))?;
            Ok(())
        })
        .map_err(|configuration_failure| {
            Error::Runtime(format_python_failure(
                &self.processor_display_name,
                "refused a configuration update",
                configuration_failure,
            ))
        })
    }

    fn to_runtime_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    fn config_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl Drop for PythonProcessorHost {
    /// Releases both Python objects while attached.
    ///
    /// Dropping a `Py` detached only queues the decrement for whichever thread
    /// attaches next, so a processor holding a file, a socket or a device
    /// context would keep it open past the teardown the author can observe. The
    /// fields are taken here rather than left to drop glue, which runs after
    /// this returns and after the attach has been released.
    fn drop(&mut self) {
        Python::attach(|_python| {
            drop(self.processor_instance.take());
            drop(self.link_data_access.take());
        });
    }
}
