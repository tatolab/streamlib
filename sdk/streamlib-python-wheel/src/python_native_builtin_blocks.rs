// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native media built-ins the wheel links. Their Python classes are
//! generated into `tatolab.stream` from the same descriptors; nothing here is
//! exported to Python.

/// Register the native built-in processor types on the process-wide registry.
/// Idempotent; called once at module import.
pub(crate) fn register_native_builtin_processor_types() {
    streamlib_media_builtins::register_media_builtin_processor_types();
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    /// The control plane's virtual-camera prompt names the built-in by a path
    /// it cannot derive, because the api-server does not link the media
    /// built-ins; the wheel links both, so this is where the two must agree.
    #[test]
    fn the_virtual_camera_prompt_names_the_path_the_built_in_registers_under() {
        assert_eq!(
            streamlib_api_server::VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH,
            streamlib_media_builtins::VirtualCameraSink::Processor::processor_class_import_path()
                .as_str()
        );
    }
}
