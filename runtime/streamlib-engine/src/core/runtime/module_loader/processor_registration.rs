// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{Error, Result};
use crate::iceoryx2::Iceoryx2Node;

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
        Error::Configuration(format!("Failed to enumerate {}: {}", lib_dir.display(), e))
    })?;
    let mut triples: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect();
    triples.sort();
    Ok(triples)
}

/// Validate a plugin's `STREAMLIB_PLUGIN` declaration against this
/// host's build before its `register` callback is invoked.
///
/// Check order is load-bearing:
///
/// 1. `abi_version` (pinned at offset 0) — read through a **raw
///    pointer** first, WITHOUT forming a `&PluginDeclaration`. A pre-v5
///    plugin's `STREAMLIB_PLUGIN` static is smaller than
///    `size_of::<PluginDeclaration>()` (v4 was 16 bytes, v5 is 48), so
///    materializing the full reference before the version is confirmed
///    would be undefined behavior on exactly the plugins this check
///    exists to reject. A mismatch returns
///    [`Error::PluginAbiVersionMismatch`] without touching the appended
///    v5 fields.
/// 2. `abi_layout_fingerprint` — the `#[repr(C)]` dispatch-surface
///    layout must match; else [`Error::PluginBuildMismatch`].
/// 3. `engine_transit_fingerprint` — `0` (engine-free plugin, no transit
///    surface) OR the host's engine transit fingerprint; else
///    [`Error::PluginBuildMismatch`].
///
/// # Safety
///
/// `decl_ptr` must point at the loaded plugin's `STREAMLIB_PLUGIN`
/// static, which the caller keeps alive for the process lifetime via
/// `LOADED_PLUGIN_LIBRARIES`. The static is at least 4 bytes (the
/// pinned `abi_version` field); the full 48-byte `PluginDeclaration`
/// is only dereferenced after `abi_version` confirms a v5 layout.
#[tracing::instrument(
    level = "debug",
    skip(decl_ptr),
    fields(plugin = %dylib_path.display())
)]
pub(crate) unsafe fn validate_plugin_declaration(
    decl_ptr: *const streamlib_plugin_abi::PluginDeclaration,
    dylib_path: &std::path::Path,
) -> Result<()> {
    let host_abi_version = streamlib_plugin_abi::STREAMLIB_ABI_VERSION;
    // Read `abi_version` (pinned at offset 0) through the raw pointer.
    // `addr_of!` computes the field's address without asserting the
    // whole `PluginDeclaration` is valid, so a shorter pre-v5 static is
    // read soundly here.
    // SAFETY: `abi_version` is `u32` at offset 0; the static is at least
    // that large for any plugin that exports the symbol.
    let plugin_abi_version = unsafe { std::ptr::addr_of!((*decl_ptr).abi_version).read() };
    if plugin_abi_version != host_abi_version {
        // Do NOT form `&*decl_ptr` or read the appended v5 fields — a
        // non-v5 declaration has a smaller byte shape and those fields
        // may not exist.
        return Err(Error::PluginAbiVersionMismatch {
            plugin_path: dylib_path.display().to_string(),
            plugin_abi_version,
            host_abi_version,
        });
    }

    // `abi_version == host_abi_version` ⇒ this is a full v5 declaration;
    // materializing the reference and reading the appended fields is now
    // sound.
    // SAFETY: the version match guarantees the 48-byte v5 layout.
    let decl = unsafe { &*decl_ptr };
    let host_abi_fingerprint = streamlib_plugin_abi::PLUGIN_ABI_LAYOUT_FINGERPRINT;
    let host_transit_fingerprint =
        crate::core::plugin::build_fingerprint::ENGINE_TRANSIT_FINGERPRINT;
    let host_identity = crate::core::plugin::build_fingerprint::BUILD_IDENTITY.to_string();

    let build_mismatch = |plugin_transit_fingerprint: u64| Error::PluginBuildMismatch {
        plugin_path: dylib_path.display().to_string(),
        plugin_identity: read_plugin_build_identity(decl),
        host_identity: host_identity.clone(),
        plugin_abi_fingerprint: decl.abi_layout_fingerprint,
        host_abi_fingerprint,
        plugin_transit_fingerprint,
        host_transit_fingerprint,
    };

    if decl.abi_layout_fingerprint != host_abi_fingerprint {
        return Err(build_mismatch(decl.engine_transit_fingerprint));
    }

    // `0` = engine-free plugin (no transit surface). Any non-zero value
    // must match this host's engine transit fingerprint exactly.
    if decl.engine_transit_fingerprint != 0
        && decl.engine_transit_fingerprint != host_transit_fingerprint
    {
        return Err(build_mismatch(decl.engine_transit_fingerprint));
    }

    Ok(())
}

/// Read a plugin's build-identity string defensively — the plugin's
/// memory is never trusted on the error path. Null pointer or zero
/// length → `"unknown"`; the length is capped and the bytes are
/// lossily decoded.
fn read_plugin_build_identity(decl: &streamlib_plugin_abi::PluginDeclaration) -> String {
    /// A build identity is `engine-version / rustc -V / triple / profile`
    /// — a few hundred bytes at most. Cap well above that so a corrupt
    /// length can't drive an unbounded read.
    const MAX_IDENTITY_LEN: usize = 4096;

    if decl.build_identity_ptr.is_null() || decl.build_identity_len == 0 {
        return "unknown".to_string();
    }
    let len = decl.build_identity_len.min(MAX_IDENTITY_LEN);
    // SAFETY: for a v5 declaration (guaranteed by the `abi_version`
    // gate above) `build_identity_ptr` / `build_identity_len` describe
    // a `'static str` in the plugin's image, kept alive for the process
    // lifetime via `LOADED_PLUGIN_LIBRARIES`. The length is bounded and
    // the bytes are decoded lossily.
    let bytes = unsafe { std::slice::from_raw_parts(decl.build_identity_ptr, len) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// Stage this package's processors into the load's registration staging
/// buffer — applied to the global processor registry only at the
/// whole-load commit. Non-recursive — the caller is responsible for
/// having walked any transitive deps first.
pub(super) fn register_manifest_processors(
    iceoryx2_node: &Iceoryx2Node,
    project_path: &std::path::Path,
    config: &crate::core::config::ProjectConfig,
    link_checkout: Option<&std::path::Path>,
    staging: &std::sync::Arc<super::staging::ModuleLoadRegistrationStaging>,
    owner_package: &streamlib_idents::PackageRef,
) -> Result<()> {
    use super::schema_registration::resolve_config_schema_canonical_id;
    use crate::core::ProcessorDescriptor;
    use crate::core::compiler::compiler_ops::create_deno_subprocess_host_constructor;
    use crate::core::compiler::compiler_ops::create_python_native_subprocess_host_constructor;
    use crate::core::compiler::compiler_ops::resolve_python_native_lib_path;
    use crate::core::config::ProjectConfig;
    use crate::core::descriptors::{PortDescriptor, ProcessorRuntime};
    use crate::core::execution::{ExecutionConfig, ProcessExecution};

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
            // Runtime package-load boundary — read the registry config from the
            // environment so a registry-only package resolves its schema deps
            // from the registry (not a dev path patch). An active `streamlib
            // link` (threaded by the module loader) additionally redirects a
            // schema dep present in the checkout to the checkout — the load-time
            // half of the zero-registry dev loop; `None` leaves this unchanged.
            let mut resolver_options = streamlib_idents::ResolverOptions::from_env();
            if let Some(checkout) = link_checkout {
                resolver_options.link_checkout = Some(checkout.to_path_buf());
            }
            Some(
                streamlib_idents::resolve_with(project_path, &resolver_options).map_err(|e| {
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
        let proc_schema_ident = compose_processor_schema_ident(package_metadata, &proc_schema.name)
            .map_err(|e| {
                Error::Configuration(format!(
                    "processor short name `{}` in {} is not valid PascalCase: {}",
                    proc_schema.name,
                    project_path.display(),
                    e
                ))
            })?;

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
                            let available = list_available_triples(&lib_dir).unwrap_or_default();
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

                    // Take the RAW pointer to `STREAMLIB_PLUGIN`; do not
                    // form a `&PluginDeclaration` yet. A pre-v5 plugin's
                    // static is smaller than the v5 struct, so borrowing
                    // it before the version is confirmed would be UB.
                    let decl_ptr: *const streamlib_plugin_abi::PluginDeclaration = unsafe {
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
                        *symbol
                    };

                    // Refuse — with a typed, actionable error naming both
                    // build identities — any plugin whose wire ABI, dispatch-
                    // surface layout, or engine-internal transit layout could
                    // skew from this host's, BEFORE invoking `register`. Reads
                    // `abi_version` through the raw pointer first, so a pre-v5
                    // static is never over-read.
                    // SAFETY: `decl_ptr` is the `STREAMLIB_PLUGIN` static in
                    // `lib`, kept alive for the process lifetime below.
                    unsafe { validate_plugin_declaration(decl_ptr, &dylib_path)? };

                    // Validated as a v5 declaration ⇒ the full struct is
                    // present; borrowing to invoke `register` is now sound.
                    // SAFETY: version-confirmed v5 layout; `lib` outlives use.
                    let decl: &streamlib_plugin_abi::PluginDeclaration = unsafe { &*decl_ptr };

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
                    // The cdylib's registration prologue runs synchronously
                    // on this thread and lands in the host's
                    // `host_schema_register` / `host_processor_register`
                    // callbacks — install the thread-local staging sink
                    // around the call so those registrations stage into
                    // this load instead of writing the global registries.
                    // The RAII guard clears the sink on every exit.
                    {
                        let _cdylib_registration_sink_guard =
                            super::staging::CdylibRegistrationSinkGuard::install(
                                std::sync::Arc::clone(staging),
                                owner_package.clone(),
                            );
                        // SAFETY: `host_services` outlives the call;
                        // the cdylib's callback returns before this
                        // function frame is dropped.
                        unsafe {
                            (decl.register)(
                                &host_services as *const _ as *const ::std::ffi::c_void,
                            );
                        }
                    }

                    // Stage the image; retained for the process lifetime
                    // whether this load commits or fails.
                    staging.stage_plugin_library(lib, dylib_path.clone(), owner_package.clone());

                    rust_dylib_loaded = true;
                    tracing::info!(
                        "Rust dylib plugin loaded and registrations staged: {}",
                        dylib_path.display()
                    );
                }

                // Validate the processor was registered by the dylib —
                // compare by structured `SchemaIdent`, not bare PascalCase
                // (two packages can declare the same short name). Reads the
                // load's staging buffer: mid-walk, nothing has landed in
                // the global registry yet.
                if !staging.contains_staged_processor(&proc_schema_ident) {
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
                let python_executable = project_path.join(".venv").join("bin").join("python");
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

        staging.stage_processor(
            descriptor,
            super::staging::StagedProcessorRegistrationKind::Dynamic { constructor },
            owner_package.clone(),
        );

        tracing::info!("Staged processor '{}' ({:?})", proc_schema.name, runtime);

        registered_count += 1;
    }

    tracing::info!(
        "Staged {} processor(s) from {}",
        registered_count,
        project_path.display()
    );

    Ok(())
}

/// Compose a processor's structured schema ident from its package's metadata
/// + PascalCase short name. `SchemaIdent::new` projects a prerelease package
/// version onto its release core (constructor invariant), so a dev-versioned
/// package registers release-core processor idents — matching the idents the
/// Python / Deno decorator side mints for the same processors.
fn compose_processor_schema_ident(
    package_metadata: &crate::core::config::PackageMetadata,
    short_name: &str,
) -> std::result::Result<crate::core::descriptors::SchemaIdent, streamlib_idents::IdentError> {
    let type_name = streamlib_processor_schema::TypeName::new(short_name)?;
    Ok(crate::core::descriptors::SchemaIdent::new(
        package_metadata.org.clone(),
        package_metadata.name.clone(),
        type_name,
        package_metadata.version,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processor_ident_from_dev_versioned_package_is_release_core() {
        // The module-load registration path must register the same identity
        // the Python / Deno decorators mint — a dev-versioned package's
        // processor idents carry the release core, or full-ident comparisons
        // and registry map keys split across the host/subprocess boundary.
        let meta: crate::core::config::PackageMetadata =
            serde_yaml::from_str("org: tatolab\nname: camera\nversion: 0.4.33-dev.2\n").unwrap();
        let ident = compose_processor_schema_ident(&meta, "Camera").unwrap();
        assert_eq!(
            ident.version,
            streamlib_idents::SemVer::new(0, 4, 33),
            "prerelease must not survive into the registered processor ident"
        );
        assert_eq!(ident.to_string(), "@tatolab/camera/Camera@0.4.33");
        // Invalid short names still surface as errors.
        assert!(compose_processor_schema_ident(&meta, "not-pascal").is_err());
    }

    // ---- validate_plugin_declaration ----

    unsafe extern "C" fn noop_register(_host_services: *const ::std::ffi::c_void) {}

    const PLUGIN_TEST_IDENTITY: &str =
        "streamlib-test-plugin 9.9.9 / rustc-test / x-triple / debug";

    /// A declaration whose fingerprints match this host exactly.
    fn matched_declaration() -> streamlib_plugin_abi::PluginDeclaration {
        streamlib_plugin_abi::PluginDeclaration {
            abi_version: streamlib_plugin_abi::STREAMLIB_ABI_VERSION,
            _reserved_padding: 0,
            register: noop_register,
            abi_layout_fingerprint: streamlib_plugin_abi::PLUGIN_ABI_LAYOUT_FINGERPRINT,
            engine_transit_fingerprint:
                crate::core::plugin::build_fingerprint::ENGINE_TRANSIT_FINGERPRINT,
            build_identity_ptr: PLUGIN_TEST_IDENTITY.as_ptr(),
            build_identity_len: PLUGIN_TEST_IDENTITY.len(),
        }
    }

    fn probe_path() -> &'static std::path::Path {
        std::path::Path::new("/tmp/libtest_plugin.so")
    }

    // SAFETY (all tests below): the pointer passed is `&<local
    // PluginDeclaration>`, a full valid v5 struct that outlives the call.
    #[test]
    fn validate_accepts_matched_declaration() {
        unsafe { validate_plugin_declaration(&matched_declaration(), probe_path()) }
            .expect("a build-matched declaration must load");
    }

    #[test]
    fn validate_accepts_engine_free_transit_zero() {
        // `engine_transit_fingerprint == 0` is the "engine-free plugin"
        // sentinel — no transit surface, so the transit check is skipped.
        let mut decl = matched_declaration();
        decl.engine_transit_fingerprint = 0;
        unsafe { validate_plugin_declaration(&decl, probe_path()) }
            .expect("an engine-free plugin (transit fingerprint 0) must load");
    }

    #[test]
    fn validate_rejects_wrong_abi_version_without_reading_appended_fields() {
        let mut decl = matched_declaration();
        decl.abi_version = streamlib_plugin_abi::STREAMLIB_ABI_VERSION + 1;
        // Poison the appended fields: a correct implementation gates on
        // `abi_version` FIRST and never dereferences these, so a null
        // pointer + absurd length must not be touched.
        decl.build_identity_ptr = std::ptr::null();
        decl.build_identity_len = usize::MAX;

        let err = unsafe { validate_plugin_declaration(&decl, probe_path()) }
            .expect_err("a wrong abi_version must be refused");
        match &err {
            Error::PluginAbiVersionMismatch {
                plugin_abi_version,
                host_abi_version,
                ..
            } => {
                assert_eq!(
                    *plugin_abi_version,
                    streamlib_plugin_abi::STREAMLIB_ABI_VERSION + 1
                );
                assert_eq!(
                    *host_abi_version,
                    streamlib_plugin_abi::STREAMLIB_ABI_VERSION
                );
            }
            other => panic!("expected PluginAbiVersionMismatch, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("Rebuild the plugin"), "remedy missing: {msg}");
    }

    #[test]
    fn validate_rejects_mismatched_abi_fingerprint() {
        let mut decl = matched_declaration();
        decl.abi_layout_fingerprint ^= 0xDEAD_BEEF;
        let err = unsafe { validate_plugin_declaration(&decl, probe_path()) }
            .expect_err("a mismatched abi_layout_fingerprint must be refused");
        assert!(
            matches!(err, Error::PluginBuildMismatch { .. }),
            "expected PluginBuildMismatch, got {err:?}"
        );
        let msg = err.to_string();
        // Both identities + the remedy appear in the operator-facing message.
        assert!(
            msg.contains(PLUGIN_TEST_IDENTITY),
            "plugin identity missing: {msg}"
        );
        assert!(
            msg.contains(crate::core::plugin::build_fingerprint::BUILD_IDENTITY),
            "host identity missing: {msg}"
        );
        assert!(msg.contains("Rebuild the plugin"), "remedy missing: {msg}");
    }

    #[test]
    fn validate_rejects_mismatched_transit_fingerprint() {
        let mut decl = matched_declaration();
        // Non-zero (so not treated as engine-free) and non-matching.
        decl.engine_transit_fingerprint =
            crate::core::plugin::build_fingerprint::ENGINE_TRANSIT_FINGERPRINT ^ 0x1;
        let err = unsafe { validate_plugin_declaration(&decl, probe_path()) }
            .expect_err("a mismatched engine_transit_fingerprint must be refused");
        match &err {
            Error::PluginBuildMismatch {
                plugin_transit_fingerprint,
                host_transit_fingerprint,
                ..
            } => {
                assert_ne!(plugin_transit_fingerprint, host_transit_fingerprint);
            }
            other => panic!("expected PluginBuildMismatch, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(
            msg.contains(PLUGIN_TEST_IDENTITY),
            "plugin identity missing: {msg}"
        );
        assert!(
            msg.contains(crate::core::plugin::build_fingerprint::BUILD_IDENTITY),
            "host identity missing: {msg}"
        );
        assert!(msg.contains("Rebuild the plugin"), "remedy missing: {msg}");
    }

    #[test]
    fn validate_reads_garbage_identity_pointer_safely() {
        // A build mismatch with a null identity pointer must still
        // construct the error (identity → "unknown"), never deref null.
        let mut decl = matched_declaration();
        decl.abi_layout_fingerprint ^= 0x1;
        decl.build_identity_ptr = std::ptr::null();
        decl.build_identity_len = 128; // non-zero, but ptr is null
        let err = unsafe { validate_plugin_declaration(&decl, probe_path()) }
            .expect_err("mismatch must be refused even with a null identity");
        match &err {
            Error::PluginBuildMismatch {
                plugin_identity, ..
            } => {
                assert_eq!(plugin_identity, "unknown");
            }
            other => panic!("expected PluginBuildMismatch, got {other:?}"),
        }
    }
}
