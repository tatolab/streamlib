// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{Error, Result};
use crate::iceoryx2::Iceoryx2Node;

/// Keeps loaded dylib plugin libraries alive for the process lifetime.
///
/// When a Rust dylib plugin is loaded, the `Library` handle must
/// remain alive so that the registered processor vtables stay valid.
static LOADED_PLUGIN_LIBRARIES: std::sync::LazyLock<parking_lot::Mutex<Vec<libloading::Library>>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(Vec::new()));

/// The rustc target triple this engine was compiled for, captured at
/// build time via `build.rs`. Same string Cargo prints for `--target`
/// (e.g. `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`). Used to
/// resolve plugin cdylibs inside `lib/<triple>/...` during package
/// load — and exposed publicly so consumers (examples, application
/// binaries) can write into the same triple-keyed directory layout
/// without each running their own `build.rs`.
pub fn host_target_triple() -> &'static str {
    env!("STREAMLIB_HOST_TARGET")
}

/// Enumerate the target triples present as subdirectories of a
/// package's `lib/` dir. Used in the error path when the host's triple
/// has no matching artifact, so the user sees which platforms a slpkg
/// WAS packed for instead of a generic "file not found".
pub(super) fn list_available_triples(lib_dir: &std::path::Path) -> Result<Vec<String>> {
    if !lib_dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(lib_dir).map_err(|e| {
        Error::Configuration(format!(
            "Failed to enumerate {}: {}",
            lib_dir.display(),
            e
        ))
    })?;
    let mut triples: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect();
    triples.sort();
    Ok(triples)
}

/// Register this package's processors with the global processor
/// registry. Non-recursive — the caller is responsible for having
/// walked any transitive deps first.
pub(super) fn register_manifest_processors(
    iceoryx2_node: &Iceoryx2Node,
    project_path: &std::path::Path,
    config: &crate::core::config::ProjectConfig,
) -> Result<()> {
    use super::schema_registration::resolve_config_schema_canonical_id;
    use crate::core::compiler::compiler_ops::create_deno_subprocess_host_constructor;
    use crate::core::compiler::compiler_ops::create_python_native_subprocess_host_constructor;
    use crate::core::compiler::compiler_ops::resolve_python_native_lib_path;
    use crate::core::config::ProjectConfig;
    use crate::core::descriptors::{PortDescriptor, ProcessorRuntime};
    use crate::core::execution::{ExecutionConfig, ProcessExecution};
    use crate::core::ProcessorDescriptor;

    if config.processors.is_empty() {
        tracing::debug!(
            "No processors declared in {} at {} (schemas-only package or fixture).",
            ProjectConfig::FILE_NAME,
            project_path.display()
        );
        return Ok(());
    }

    let mut registered_count = 0usize;
    let mut rust_dylib_loaded = false;

    // Resolve package metadata once per project — every dynamically
    // registered processor in this manifest uses these typed segments
    // to compose its full structured `SchemaIdent`.
    let package_metadata = config.package.as_ref().ok_or_else(|| {
        Error::Configuration(format!(
            "{} at {} declares processors but is missing a `package:` block. \
             The structured-everywhere processor identity rule requires \
             `package: {{ org, name, version }}` to compose each processor's \
             SchemaIdent.",
            ProjectConfig::FILE_NAME,
            project_path.display(),
        ))
    })?;

    // Eagerly resolve the dependency graph once per project — used
    // below by `resolve_config_schema_canonical_id` for each
    // processor's bare-name config schema (#767). The resolver walks
    // declared paths/git/.slpkg sources and validates the `schemas:`
    // map; we only invoke it when at least one processor actually
    // declares a config block.
    let config_resolved: Option<streamlib_idents::ResolvedPackages> =
        if config.processors.iter().any(|p| p.config.is_some()) {
            Some(
                streamlib_idents::resolve_with(
                    project_path,
                    &streamlib_idents::ResolverOptions::default(),
                )
                .map_err(|e| {
                    Error::Configuration(format!(
                        "failed to resolve manifest dependencies for bare-name \
                         config schema lookup at {}: {}",
                        project_path.display(),
                        e
                    ))
                })?,
            )
        } else {
            None
        };

    for proc_schema in &config.processors {
        // Compose the structured processor ident from the manifest's
        // package metadata + the processor's PascalCase short name.
        let proc_type_name = streamlib_processor_schema::TypeName::new(&proc_schema.name)
            .map_err(|e| {
                Error::Configuration(format!(
                    "processor short name `{}` in {} is not valid PascalCase: {}",
                    proc_schema.name,
                    project_path.display(),
                    e
                ))
            })?;
        let proc_schema_ident = crate::core::descriptors::SchemaIdent::new(
            package_metadata.org.clone(),
            package_metadata.name.clone(),
            proc_type_name,
            package_metadata.version.clone(),
        );

        // Map runtime language to ProcessorRuntime
        let runtime = match proc_schema.runtime.language {
            streamlib_processor_schema::ProcessorLanguage::Python => ProcessorRuntime::Python,
            streamlib_processor_schema::ProcessorLanguage::TypeScript => {
                ProcessorRuntime::TypeScript
            }
            streamlib_processor_schema::ProcessorLanguage::Rust => {
                // Rust dylib plugins self-register via export_plugin! macro.
                // Load the dylib once per project (all Rust processors in the
                // same YAML share one dylib), then validate each processor
                // was actually registered.
                if !rust_dylib_loaded {
                    let host_triple = host_target_triple();
                    let lib_dir = project_path.join("lib");
                    let triple_dir = lib_dir.join(host_triple);
                    let dylib_ext = if cfg!(target_os = "macos") {
                        "dylib"
                    } else if cfg!(target_os = "windows") {
                        "dll"
                    } else {
                        "so"
                    };

                    let dylib_path = std::fs::read_dir(&triple_dir)
                        .map_err(|e| {
                            // If `lib/` exists but the triple-keyed
                            // subdir is absent, surface the available
                            // triples so the user sees exactly which
                            // platforms this slpkg was packed for.
                            let available =
                                list_available_triples(&lib_dir).unwrap_or_default();
                            let detail = if available.is_empty() {
                                format!(
                                    "Failed to read {}: {}. \
                                     The package may be missing a Rust artifact for this host.",
                                    triple_dir.display(),
                                    e
                                )
                            } else {
                                format!(
                                    "Failed to read {}: {}. \
                                     This slpkg was packed for: [{}]. \
                                     The host triple is `{}` — repack on a matching host \
                                     or run this pipeline on a host that matches one of the \
                                     packed triples.",
                                    triple_dir.display(),
                                    e,
                                    available.join(", "),
                                    host_triple
                                )
                            };
                            Error::Configuration(detail)
                        })?
                        .filter_map(|entry| entry.ok())
                        .map(|entry| entry.path())
                        .find(|path| path.extension().is_some_and(|ext| ext == dylib_ext))
                        .ok_or_else(|| {
                            Error::Configuration(format!(
                                "No .{} file found in {} (host triple `{}`)",
                                dylib_ext,
                                triple_dir.display(),
                                host_triple
                            ))
                        })?;

                    tracing::info!("Loading Rust dylib plugin: {}", dylib_path.display());

                    // Safety: Loading a dynamic library is inherently unsafe.
                    // The dylib must be a valid StreamLib plugin built with
                    // a compatible streamlib-plugin-abi version.
                    let lib = unsafe {
                        libloading::Library::new(&dylib_path).map_err(|e| {
                            Error::Configuration(format!(
                                "Failed to load dylib {}: {}",
                                dylib_path.display(),
                                e
                            ))
                        })?
                    };

                    let decl: &streamlib_plugin_abi::PluginDeclaration = unsafe {
                        let symbol = lib
                            .get::<*const streamlib_plugin_abi::PluginDeclaration>(
                                b"STREAMLIB_PLUGIN\0",
                            )
                            .map_err(|e| {
                                Error::Configuration(format!(
                                    "Plugin '{}' missing STREAMLIB_PLUGIN symbol. \
                                     Ensure the plugin uses the export_plugin! macro: {}",
                                    dylib_path.display(),
                                    e
                                ))
                            })?;
                        &**symbol
                    };

                    if decl.abi_version != streamlib_plugin_abi::STREAMLIB_ABI_VERSION {
                        return Err(Error::Configuration(format!(
                            "ABI version mismatch for '{}': plugin has v{}, \
                             runtime expects v{}. Rebuild the plugin with a \
                             compatible streamlib-plugin-abi version.",
                            dylib_path.display(),
                            decl.abi_version,
                            streamlib_plugin_abi::STREAMLIB_ABI_VERSION,
                        )));
                    }

                    // Build the HostServices payload from the host's
                    // process-wide statics + this runtime's iceoryx2
                    // node, hand it to the cdylib's register callback.
                    // The cdylib's macro-emitted prologue calls
                    // `install_host_services` which bridges every
                    // per-DSO static (tracing dispatch, PUBSUB,
                    // schema registry, iceoryx2 logger) into the
                    // host's instances before processor registration.
                    let host_services =
                        crate::core::plugin::host_services::runtime_facing::host_services_for_self(
                            iceoryx2_node,
                        );
                    // SAFETY: `host_services` outlives the call;
                    // the cdylib's callback returns before this
                    // function frame is dropped.
                    unsafe {
                        (decl.register)(
                            &host_services as *const _ as *const ::std::ffi::c_void,
                        );
                    }

                    // Keep the library alive for the process lifetime
                    LOADED_PLUGIN_LIBRARIES.lock().push(lib);

                    rust_dylib_loaded = true;
                    tracing::info!(
                        "Rust dylib plugin loaded and registered: {}",
                        dylib_path.display()
                    );
                }

                // Validate the processor was registered by the dylib —
                // compare by structured `SchemaIdent`, not bare PascalCase
                // (two packages can declare the same short name).
                let registered = crate::core::processors::PROCESSOR_REGISTRY
                    .list_registered()
                    .iter()
                    .any(|desc| desc.name == proc_schema_ident);
                if !registered {
                    return Err(Error::Configuration(format!(
                        "Processor '{}' declared in streamlib.yaml but not \
                         registered by the dylib. Ensure export_plugin!() \
                         includes this processor.",
                        proc_schema_ident
                    )));
                }

                tracing::info!("Validated Rust dylib processor '{}'", proc_schema.name);
                registered_count += 1;
                continue;
            }
        };

        let inputs: Vec<PortDescriptor> = proc_schema
            .inputs
            .iter()
            .map(|p| {
                PortDescriptor::new(
                    &p.name,
                    p.description.as_deref().unwrap_or(""),
                    p.schema.clone(),
                    true,
                )
            })
            .collect();

        let outputs: Vec<PortDescriptor> = proc_schema
            .outputs
            .iter()
            .map(|p| {
                PortDescriptor::new(
                    &p.name,
                    p.description.as_deref().unwrap_or(""),
                    p.schema.clone(),
                    true,
                )
            })
            .collect();

        let mut descriptor = ProcessorDescriptor::new(
            proc_schema_ident.clone(),
            proc_schema.description.as_deref().unwrap_or(""),
        )
        .with_version(&proc_schema.version)
        .with_runtime(runtime.clone());

        if let Some(entrypoint) = &proc_schema.entrypoint {
            descriptor = descriptor.with_entrypoint(entrypoint);
        }

        if let Some(config) = &proc_schema.config {
            // Resolve the bare-name `TypeName` (#767) against the
            // manifest's `schemas:` map to its canonical id string.
            // The lookup walks the manifest's declarations, locates
            // the owning package + schema file, and reads the
            // schema's `metadata.type` (new shape) or
            // `metadata.name` (legacy reverse-DNS) to compose the
            // id. This must match what
            // `register_package_schemas` / `canonical_identifier_for_schema`
            // registered when the same manifest was loaded.
            let canonical =
                resolve_config_schema_canonical_id(project_path, config, &config_resolved)
                    .map_err(|msg| {
                        Error::Configuration(format!(
                            "processor `{}` config schema `{}`: {}",
                            proc_schema.name,
                            config.schema.as_str(),
                            msg
                        ))
                    })?;
            descriptor = descriptor.with_config_schema(canonical);
        }

        if let Some(scheduling) = &proc_schema.scheduling {
            descriptor = descriptor.with_scheduling(scheduling.clone());
        }

        descriptor.inputs = inputs;
        descriptor.outputs = outputs;

        // Convert schema execution mode to runtime ExecutionConfig
        let execution = match &proc_schema.execution {
            streamlib_processor_schema::ProcessorSchemaExecution::Reactive => {
                ProcessExecution::Reactive
            }
            streamlib_processor_schema::ProcessorSchemaExecution::Manual => {
                ProcessExecution::Manual
            }
            streamlib_processor_schema::ProcessorSchemaExecution::Continuous { interval_ms } => {
                ProcessExecution::Continuous {
                    interval_ms: *interval_ms,
                }
            }
        };
        let execution_config = ExecutionConfig::new(execution);

        // Create constructor based on runtime language.
        // Python and TypeScript subprocesses both use native FFI for direct
        // iceoryx2 access — no pipe bridge for data I/O.
        let constructor = match runtime {
            ProcessorRuntime::Python => {
                let native_lib_path = resolve_python_native_lib_path()?;
                // The build orchestrator provisions the package's venv at
                // `{staged_package_dir}/.venv/bin/python` (Unix) /
                // `.venv/Scripts/python.exe` (Windows) as the tail of
                // `materialize`. `project_path` is that staged dir, so the
                // interpreter is a pure path join — no venv creation here.
                #[cfg(unix)]
                let python_executable =
                    project_path.join(".venv").join("bin").join("python");
                #[cfg(windows)]
                let python_executable = project_path
                    .join(".venv")
                    .join("Scripts")
                    .join("python.exe");
                create_python_native_subprocess_host_constructor(
                    &descriptor,
                    execution_config,
                    project_path.to_path_buf(),
                    python_executable,
                    native_lib_path,
                )
            }
            ProcessorRuntime::TypeScript => create_deno_subprocess_host_constructor(
                &descriptor,
                execution_config,
                project_path.to_path_buf(),
            ),
            _ => unreachable!(),
        };

        crate::core::processors::PROCESSOR_REGISTRY
            .register_dynamic(descriptor, constructor)?;

        tracing::info!(
            "Registered processor '{}' ({:?})",
            proc_schema.name,
            runtime
        );

        registered_count += 1;
    }

    tracing::info!(
        "Loaded {} processor(s) from {}",
        registered_count,
        project_path.display()
    );

    Ok(())
}
