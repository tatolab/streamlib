// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Module-reachability resolution for the processor source-scan.
//!
//! [`crate::extract_rust_processors`] visits every `.rs` under `processors/`,
//! including platform arms a given host does not compile (`camera_linux.rs` vs
//! `camera_apple.rs`) and parked directories (`_apple_impl_pending_/`). That raw scan
//! over-collects: two platform arms declaring the same processor both surface,
//! and a parked module surfaces a `#[processor(...)]` that never compiles on any
//! target. Before extraction can replace the hand-authored `processors:` as the
//! authoritative truth-source — and before a drift check between the two can be
//! a hard `pkg publish` error without false positives on cfg-gated packages — the
//! scan must resolve to the set of modules the build **target** actually
//! compiles.
//!
//! [`extract_reachable_rust_processors`] does that. The crate root is generated
//! (see [`crate::crate_root`]), so the walk does not read it: it enumerates the
//! top-level arms under `processors/` exactly the way the generator declares
//! them, then follows each `mod` the way `rustc` does (honoring
//! `#[path = "..."]`), evaluates the `#[cfg(...)]` predicate on every `mod` and
//! every `#[processor(...)]`-bearing struct against a
//! [`ModuleReachabilityTarget`], and collects only the processors that survive.
//! A module file's own inner `#![cfg(...)]` gates it the same way, pruning the
//! whole subtree it declares.
//!
//! The parked-directory convention needs no special case: a parked module
//! declares `#![cfg(any())]` (an always-false predicate), so cfg evaluation
//! skips it exactly as `rustc` does — one rule, not a hard-coded directory name.
//!
//! [`extract_processors_across_every_build_target`] runs the same walk with cfg
//! pruning turned off, recording each predicate it passed through instead. That
//! is what lets the crate-root generator mirror the author's `#[cfg]` onto the
//! `export_plugin!` entry verbatim rather than re-deriving a platform rule.
//!
//! Both walks then group what they collected by processor `Type` name — the one
//! segment the derived `processors:` entry keeps, and the segment the runtime
//! re-composes each processor's ident from — and refuse the two ways two arms
//! declaring one processor can be wrong: they OVERLAP (some build target
//! compiles both, so walk order decides whose entry ships) or they
//! DIVERGE (they derive different manifest entries, so the shipped
//! `processors:` section depends on which arm the publishing host compiled). A
//! GAP is not refused: a package legitimately declares a processor on some
//! targets and not others, which is exactly what
//! [`ProcessorAvailabilityAcrossBuildTargets`] makes readable data instead of
//! prose in a description string.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use quote::ToTokens;
use syn::punctuated::Punctuated;
use syn::{Meta, Token};

use streamlib_idents::PACKAGE_PROCESSOR_SOURCE_DIR_NAME as PROCESSOR_SOURCE_DIR_NAME;

use crate::derive::describe_divergent_processor_declarations;
use crate::{
    ExtractError, ExtractedProcessor, ProcessorDeclarationSite, parse_processor_attr,
    processor_attr,
};

/// The `#[cfg(...)]` evaluation environment a module-reachability walk resolves
/// against — the set of cfg atoms the build **target** defines.
///
/// A `#[cfg(target_os = "linux")]` module is reachable iff `("target_os",
/// "linux")` is in [`ModuleReachabilityTarget::key_values`]; a `#[cfg(unix)]`
/// module iff `"unix"` is in [`ModuleReachabilityTarget::flags`]. An atom the
/// target does not define evaluates to `false` — the same way `rustc` treats an
/// unset cfg — so a cross-target platform arm and a `#[cfg(any())]` parked
/// module are both excluded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleReachabilityTarget {
    /// Key/value cfg atoms the target defines, e.g. `("target_os", "linux")`,
    /// `("target_arch", "x86_64")`, `("feature", "cuda")`.
    pub key_values: BTreeSet<(String, String)>,
    /// Bare flag cfg atoms the target defines, e.g. `"unix"`, `"windows"`.
    pub flags: BTreeSet<String>,
}

impl ModuleReachabilityTarget {
    /// An empty target defining no cfg atoms — every `#[cfg(...)]`-gated module
    /// is excluded, only unconditional modules are reachable.
    pub fn new() -> Self {
        Self::default()
    }

    /// The cfg atoms for the **host** the extractor is running on, derived from
    /// [`std::env::consts`]: `target_os`, `target_arch`, `target_family`, and
    /// the `unix` / `windows` family flag. This is the target `streamlib
    /// pkg publish` extracts for — the package is built for the invoking host's
    /// triple, so the reachable processor set is the set that host compiles.
    ///
    /// Cargo features are NOT inferred here (the extractor cannot know which
    /// features a downstream build enables); add each enabled feature with
    /// [`ModuleReachabilityTarget::with_feature`].
    pub fn for_host() -> Self {
        let os = std::env::consts::OS; // "linux" / "macos" / "windows"
        let arch = std::env::consts::ARCH; // "x86_64" / "aarch64" / …
        let family = std::env::consts::FAMILY; // "unix" / "windows"
        Self::new()
            .with_key_value("target_os", os)
            .with_key_value("target_arch", arch)
            .with_key_value("target_family", family)
            .with_flag(family)
    }

    /// Add a key/value cfg atom (e.g. `("target_os", "linux")`).
    pub fn with_key_value(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.key_values.insert((key.into(), value.into()));
        self
    }

    /// Add a bare flag cfg atom (e.g. `unix`).
    pub fn with_flag(mut self, flag: impl Into<String>) -> Self {
        self.flags.insert(flag.into());
        self
    }

    /// Add an enabled cargo feature (`#[cfg(feature = "<name>")]`).
    pub fn with_feature(self, name: impl Into<String>) -> Self {
        self.with_key_value("feature", name)
    }

    /// Whether `("key", "value")` is defined by this target.
    fn has_key_value(&self, key: &str, value: &str) -> bool {
        self.key_values
            .iter()
            .any(|(defined_key, defined_value)| defined_key == key && defined_value == value)
    }

    /// Whether the bare flag `name` is defined by this target.
    fn has_flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    /// The single value this target defines for a cfg key, if it defines
    /// exactly one. `None` for a key the target leaves unset.
    fn single_defined_value_of_cfg_key(&self, key: &str) -> Option<&str> {
        let mut values = self
            .key_values
            .iter()
            .filter(|(defined_key, _)| defined_key == key)
            .map(|(_, value)| value.as_str());
        let first = values.next()?;
        values.next().is_none().then_some(first)
    }

    /// The cfg atoms this target defines, rendered the way a `#[cfg(...)]`
    /// predicate spells them (`target_os = "linux", unix`) — how a diagnostic
    /// names the build target it is talking about.
    pub(crate) fn describe_defined_cfg_atoms(&self) -> String {
        let mut atoms: Vec<String> = self
            .key_values
            .iter()
            .map(|(key, value)| format!("{key} = \"{value}\""))
            .collect();
        atoms.extend(self.flags.iter().cloned());
        if atoms.is_empty() {
            return "no cfg atoms".to_string();
        }
        atoms.join(", ")
    }

    /// Whether a `#[cfg(...)]` predicate, given as the source text the generated
    /// crate root mirrors, holds for this target. An unparseable predicate is
    /// `false`, matching how the walk treats one it cannot prove true.
    ///
    /// This is how a consumer re-evaluates a predicate the generator emitted
    /// without re-implementing cfg semantics beside the one evaluator.
    pub fn cfg_predicate_source_holds(&self, predicate_source: &str) -> bool {
        match syn::parse_str::<Meta>(predicate_source) {
            Ok(meta) => eval_cfg_meta(&meta, self),
            Err(_) => false,
        }
    }
}

/// One top-level module arm under a package's `processors/` directory — the
/// unit the generated crate root declares with a `#[path]`-attributed
/// `pub mod`, and the unit the reachability walk starts each descent from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessorSourceModuleArm {
    /// The module name the generated crate root declares — the file stem for a
    /// flat arm, the directory name for a `mod.rs`-backed one.
    pub module_name: String,
    /// Absolute path to the arm's own module file.
    pub module_file: PathBuf,
    /// The `#[path = "..."]` value the generated crate root carries, relative
    /// to the directory holding the generated crate root file. Always
    /// forward-slashed — `#[path]` is a source literal, not a host path.
    pub crate_root_relative_module_path: String,
    /// The file-level `#![cfg(...)]` predicates the arm's own file declares, in
    /// source order and as written. The generator mirrors these verbatim onto
    /// the `pub mod` and onto every `export_plugin!` entry the arm contributes.
    pub file_level_cfg_predicates: Vec<String>,
}

impl ProcessorSourceModuleArm {
    /// The directory this arm's own `mod <name>;` children resolve against.
    /// The generated declaration always carries `#[path]`, and `rustc` gives a
    /// `#[path]` module the directory holding its file — so a `mod.rs` arm owns
    /// its directory and a flat arm's children are its file's SIBLINGS.
    fn top_level_arm_child_module_search_dir(&self) -> PathBuf {
        self.module_file
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf()
    }
}

/// Enumerate the top-level module arms under `<package_dir>/processors`.
///
/// A `processors/<name>.rs` is a flat arm; a `processors/<name>/mod.rs` is a
/// directory arm. A directory without a `mod.rs` and a file with any other
/// extension are skipped rather than refused: `processors/` is the shared
/// discovery root for every language, so a Rust package in a polyglot package
/// legitimately sits beside `.py` / `.ts` modules and their directories. A
/// skipped directory that holds Rust source anyway is logged at `warn` — a
/// nested Rust processor that vanishes from the register list is otherwise
/// indistinguishable from one that was never written.
///
/// A missing `processors/` yields an empty list — a schema-only Rust package is
/// first-class. Arms are returned sorted by module name so generation and
/// extraction are deterministic.
#[tracing::instrument(skip_all, fields(package_dir = %package_dir.display()))]
pub fn enumerate_processor_source_module_arms(
    package_dir: &Path,
) -> Result<Vec<ProcessorSourceModuleArm>, ExtractError> {
    let processor_source_dir = package_dir.join(PROCESSOR_SOURCE_DIR_NAME);
    if !processor_source_dir.is_dir() {
        return Ok(Vec::new());
    }

    let entries = std::fs::read_dir(&processor_source_dir).map_err(|e| ExtractError::Io {
        path: processor_source_dir.clone(),
        source: e,
    })?;

    let arm_path_prefix = crate::crate_root::generated_crate_root_arm_path_prefix();
    let mut arms = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| ExtractError::Io {
            path: processor_source_dir.clone(),
            source: e,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| ExtractError::Io {
            path: path.clone(),
            source: e,
        })?;

        let (module_name, module_file, relative_module_path) = if file_type.is_dir() {
            let mod_rs = path.join("mod.rs");
            if !mod_rs.is_file() {
                warn_if_skipped_subdirectory_holds_rust_source(&path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            (
                name.to_string(),
                mod_rs,
                format!("{arm_path_prefix}{name}/mod.rs"),
            )
        } else {
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|n| n.to_str()) else {
                continue;
            };
            (
                stem.to_string(),
                path.clone(),
                format!("{arm_path_prefix}{stem}.rs"),
            )
        };

        let file_level_cfg_predicates = read_file_level_cfg_predicates(&module_file)?;
        arms.push(ProcessorSourceModuleArm {
            module_name,
            module_file,
            crate_root_relative_module_path: relative_module_path,
            file_level_cfg_predicates,
        });
    }

    arms.sort_by(|a, b| a.module_name.cmp(&b.module_name));
    tracing::debug!(arms = arms.len(), "enumerated processor source arms");
    Ok(arms)
}

/// A `processors/` subdirectory with no `mod.rs` is not an arm. When it holds
/// `.rs` source anyway, say so — the author gets a reason instead of an
/// unexplained absence from the generated register list.
fn warn_if_skipped_subdirectory_holds_rust_source(dir: &Path) {
    if directory_holds_rust_source(dir) {
        tracing::warn!(
            dir = %dir.display(),
            "`processors/` subdirectory holds Rust source but declares no `mod.rs` — \
             it is not a module arm, so no processor under it is discovered; add a \
             `mod.rs` naming its children"
        );
    }
}

fn directory_holds_rust_source(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(|entry| entry.ok()).any(|entry| {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            return directory_holds_rust_source(&path);
        }
        path.extension().and_then(|ext| ext.to_str()) == Some("rs")
    })
}

/// The file-level inner `#![cfg(...)]` predicates a module file declares on
/// itself, as written.
fn read_file_level_cfg_predicates(file: &Path) -> Result<Vec<String>, ExtractError> {
    let body = std::fs::read_to_string(file).map_err(|e| ExtractError::Io {
        path: file.to_path_buf(),
        source: e,
    })?;
    let parsed = syn::parse_file(&body).map_err(|e| ExtractError::Syntax {
        path: file.to_path_buf(),
        source: e,
    })?;
    Ok(cfg_predicate_sources(&parsed.attrs))
}

/// The source text of every `#[cfg(<predicate>)]` on an item, in order. A
/// predicate the parser cannot read is rendered as its raw token text so the
/// generator still mirrors it rather than dropping it.
fn cfg_predicate_sources(attrs: &[syn::Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .map(|attr| match attr.parse_args::<Meta>() {
            Ok(meta) => render_cfg_predicate(&meta),
            Err(_) => match &attr.meta {
                Meta::List(list) => list.tokens.to_token_stream().to_string(),
                other => other.to_token_stream().to_string(),
            },
        })
        .collect()
}

/// Render a parsed cfg predicate back to source text. `TokenStream::to_string`
/// alone would emit `any (a , b)` — legal but unreadable in a generated crate
/// root a human is expected to open when a build breaks.
fn render_cfg_predicate(meta: &Meta) -> String {
    match meta {
        Meta::Path(path) => path.to_token_stream().to_string(),
        Meta::NameValue(name_value) => format!(
            "{} = {}",
            name_value.path.to_token_stream(),
            name_value.value.to_token_stream()
        ),
        Meta::List(list) => match parse_cfg_combinator(list) {
            Some((combinator, inner)) => {
                let rendered: Vec<String> = inner.iter().map(render_cfg_predicate).collect();
                format!("{combinator}({})", rendered.join(", "))
            }
            None => format!(
                "{}({})",
                list.path.to_token_stream(),
                list.tokens.to_token_stream()
            ),
        },
    }
}

/// Decode a `all(..)` / `any(..)` / `not(..)` cfg combinator into its name and
/// operands. The one place the predicate grammar's list form is parsed, so a
/// new form lands in one decoder rather than three.
fn parse_cfg_combinator(list: &syn::MetaList) -> Option<(String, Punctuated<Meta, Token![,]>)> {
    let combinator = list.path.get_ident()?.to_string();
    let operands = list
        .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        .ok()?;
    Some((combinator, operands))
}

/// Every `#[processor(...)]` a package declares on ANY build target, plus the
/// per-processor availability those declarations resolve to.
///
/// The two halves answer two different questions off one walk: the declaration
/// list is per-ARM (two platform arms declaring one processor contribute two
/// entries, each with its own predicates) and is what the crate-root generator
/// turns into `export_plugin!` entries; the availability list is per-PROCESSOR
/// and is what answers "does this processor exist on target X".
#[derive(Debug, Clone, Default)]
pub struct ProcessorSetAcrossEveryBuildTarget {
    /// One entry per `#[processor(...)]` declaration, in walk order, each
    /// carrying the `#[cfg(...)]` predicates that gate its own arm.
    pub processor_declarations: Vec<ExtractedProcessor>,
    /// One entry per distinct processor `Type` name, in first-declaration
    /// order.
    pub processor_availability: Vec<ProcessorAvailabilityAcrossBuildTargets>,
}

impl ProcessorSetAcrossEveryBuildTarget {
    /// The availability of the processor a `Type` name identifies.
    pub fn availability_of_processor_type_name(
        &self,
        processor_type_name: &str,
    ) -> Option<&ProcessorAvailabilityAcrossBuildTargets> {
        self.processor_availability
            .iter()
            .find(|entry| entry.processor_type_name == processor_type_name)
    }
}

/// Which build targets one processor exists on, as the `#[cfg(...)]` predicate
/// its declaring arms resolve to.
///
/// A predicate rather than an enumerated target set: there is no closed target
/// universe — an arm may gate on `redox`, `android`, a cargo feature, a custom
/// cfg — so a fixed list of targets would go stale and misreport. A caller
/// answers "is it available on target X?" with
/// [`Self::is_available_on_build_target`], which routes through the one cfg
/// evaluator ([`ModuleReachabilityTarget::cfg_predicate_source_holds`]) rather
/// than reimplementing cfg semantics beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessorAvailabilityAcrossBuildTargets {
    /// The `Type` segment — the only segment the derived `processors:` entry
    /// keeps, and therefore the name two arms collide under.
    pub processor_type_name: String,
    /// The disjunction over each declaring arm's conjoined `#[cfg(...)]`
    /// predicates, as source text. `None` is unconditional: some arm is gated
    /// by nothing, so the processor exists on every build target.
    pub availability_cfg_predicate: Option<String>,
    /// Each declaring arm's source file, relative to the scanned package
    /// directory, in walk order.
    pub declaring_arm_source_files: Vec<PathBuf>,
}

impl ProcessorAvailabilityAcrossBuildTargets {
    /// Whether a build target compiles this processor.
    pub fn is_available_on_build_target(&self, target: &ModuleReachabilityTarget) -> bool {
        match &self.availability_cfg_predicate {
            None => true,
            Some(predicate) => target.cfg_predicate_source_holds(predicate),
        }
    }
}

/// How a module walk treats the `#[cfg(...)]` predicates it meets.
enum ModuleWalkCfgResolution<'walk> {
    /// Prune every module / struct the build target does not compile. The
    /// collected processors carry no predicates — they are unconditionally live
    /// for this target.
    AgainstBuildTarget(&'walk ModuleReachabilityTarget),
    /// Keep every arm that is satisfiable on SOME target, recording the
    /// predicates in force so a generator can mirror them verbatim. A
    /// statically-unsatisfiable predicate (`any()` with no operands — the
    /// parked-module convention) is still pruned: it compiles nowhere, so no
    /// generated arm should name it.
    AcrossEveryBuildTarget,
}

/// Derive the `processors:` manifest section from the modules a Rust package
/// compiles **for `target`** — the reachability-resolved counterpart to
/// [`crate::extract_rust_processors`].
///
/// Starts at each top-level arm under `processors/`, follows every reachable
/// `mod` the way `rustc` resolves module files, and evaluates `#[cfg(...)]` on
/// each `mod` and each `#[processor(...)]`-bearing struct against `target`, plus
/// the inner `#![cfg(...)]` a module file declares on itself. A `#[processor]`
/// under a cfg-excluded module — a cross-platform arm, a disabled feature, or a
/// `#[cfg(any())]` parked directory — is never collected. The result is
/// deterministic (source order within a file, arm order across files) and
/// de-duplicated by resolved source file.
///
/// Two surviving declarations of one processor `Type` are refused as
/// [`ExtractError::OverlappingProcessorDeclarations`]: this target compiles
/// both, which is a proof of overlap needing no reasoning about predicates.
#[tracing::instrument(skip_all, fields(package_dir = %package_dir.display()))]
pub fn extract_reachable_rust_processors(
    package_dir: &Path,
    target: &ModuleReachabilityTarget,
) -> Result<Vec<ExtractedProcessor>, ExtractError> {
    let processors = walk_processor_source_arms(
        package_dir,
        ModuleWalkCfgResolution::AgainstBuildTarget(target),
    )?;
    refuse_processors_declared_twice_for_one_build_target(&processors, target)?;
    Ok(processors)
}

/// Every `#[processor(...)]` a Rust package declares on ANY target, each
/// carrying the `#[cfg(...)]` predicates that gate it, plus the per-processor
/// availability they resolve to.
///
/// This is the crate-root generator's input: it needs the union across targets
/// (a Linux host still generates the macOS arm's `export_plugin!` entry, gated)
/// together with the author's predicates, mirrored verbatim so the generated
/// root never re-derives a platform rule. Parked arms — anything gated by a
/// statically-unsatisfiable predicate such as `#[cfg(any())]` — are excluded:
/// they compile on no target, so naming them in a generated arm would only
/// produce a declaration entry that is always stripped.
///
/// Two arms declaring one processor `Type` are refused when some build target
/// compiles both ([`ExtractError::OverlappingProcessorDeclarations`]) or when
/// they derive different manifest entries
/// ([`ExtractError::DivergentProcessorDeclarations`]).
#[tracing::instrument(skip_all, fields(package_dir = %package_dir.display()))]
pub fn extract_processors_across_every_build_target(
    package_dir: &Path,
) -> Result<ProcessorSetAcrossEveryBuildTarget, ExtractError> {
    let processor_declarations =
        walk_processor_source_arms(package_dir, ModuleWalkCfgResolution::AcrossEveryBuildTarget)?;
    let processor_availability =
        resolve_processor_availability_across_build_targets(&processor_declarations)?;
    tracing::debug!(
        declarations = processor_declarations.len(),
        processors = processor_availability.len(),
        "resolved per-processor availability"
    );
    Ok(ProcessorSetAcrossEveryBuildTarget {
        processor_declarations,
        processor_availability,
    })
}

/// Refuse a processor `Type` that one build target collected twice.
///
/// No satisfiability argument is needed here: the walk already resolved a
/// concrete target and it compiled both arms. This is the reasoning-free net
/// behind [`find_build_target_satisfying_both_cfg_predicate_sets`] — an exotic
/// predicate pair the atom model calls disjoint still fails loudly on the host
/// that actually compiles both.
fn refuse_processors_declared_twice_for_one_build_target(
    processors: &[ExtractedProcessor],
    target: &ModuleReachabilityTarget,
) -> Result<(), ExtractError> {
    for (processor_type_name, declaring_arms) in
        group_declarations_by_processor_type_name(processors)
    {
        let [first, second, ..] = declaring_arms.as_slice() else {
            continue;
        };
        return Err(ExtractError::OverlappingProcessorDeclarations {
            processor_type_name: processor_type_name.to_string(),
            first_declared_in: first.source_file.clone(),
            second_declared_in: second.source_file.clone(),
            witness_build_target_atoms: target.describe_defined_cfg_atoms(),
        });
    }
    Ok(())
}

/// Group the across-every-target declarations by processor `Type` name, refuse
/// the overlapping and divergent groups, and fold each surviving group into its
/// availability predicate.
fn resolve_processor_availability_across_build_targets(
    declarations: &[ExtractedProcessor],
) -> Result<Vec<ProcessorAvailabilityAcrossBuildTargets>, ExtractError> {
    let mut out = Vec::new();
    for (processor_type_name, declaring_arms) in
        group_declarations_by_processor_type_name(declarations)
    {
        refuse_overlapping_processor_declarations(processor_type_name, &declaring_arms)?;
        refuse_divergent_processor_declarations(processor_type_name, &declaring_arms)?;

        let arm_predicates: Vec<Option<String>> = declaring_arms
            .iter()
            .map(|arm| conjoin_cfg_predicates(&arm.cfg_predicates))
            .collect();

        out.push(ProcessorAvailabilityAcrossBuildTargets {
            processor_type_name: processor_type_name.to_string(),
            availability_cfg_predicate:
                disjoin_cfg_predicates_when_every_alternative_is_conditional(
                    arm_predicates.iter().map(Option::as_deref),
                ),
            declaring_arm_source_files: declaring_arms
                .iter()
                .map(|arm| arm.source_file.clone())
                .collect(),
        });
    }
    Ok(out)
}

/// Declarations grouped by processor `Type` name, groups in first-declaration
/// order and arms within a group in walk order.
///
/// `Type` name rather than the full `@org/package/Type`: the derived
/// `processors:` entry keeps only that segment, and at load the runtime
/// composes each processor's structured ident from the PACKAGE's own org / name
/// plus it. Two arms sharing a `Type` therefore fold into one manifest entry
/// and one composed ident whatever `@org/package` their attributes named, so
/// they must be caught here rather than pass as two unrelated processors.
fn group_declarations_by_processor_type_name(
    declarations: &[ExtractedProcessor],
) -> Vec<(&str, Vec<&ExtractedProcessor>)> {
    let mut groups: Vec<(&str, Vec<&ExtractedProcessor>)> = Vec::new();
    for declaration in declarations {
        let processor_type_name = declaration.schema.name.as_str();
        match groups
            .iter_mut()
            .find(|(grouped_name, _)| *grouped_name == processor_type_name)
        {
            Some((_, declaring_arms)) => declaring_arms.push(declaration),
            None => groups.push((processor_type_name, vec![declaration])),
        }
    }
    groups
}

/// Refuse a processor group whose arms are not mutually exclusive, naming a
/// build target that compiles both.
fn refuse_overlapping_processor_declarations(
    processor_type_name: &str,
    declaring_arms: &[&ExtractedProcessor],
) -> Result<(), ExtractError> {
    for (index, first) in declaring_arms.iter().enumerate() {
        for second in &declaring_arms[index + 1..] {
            let Some(witness) = find_build_target_satisfying_both_cfg_predicate_sets(
                &first.cfg_predicates,
                &second.cfg_predicates,
            ) else {
                continue;
            };
            return Err(ExtractError::OverlappingProcessorDeclarations {
                processor_type_name: processor_type_name.to_string(),
                first_declared_in: first.source_file.clone(),
                second_declared_in: second.source_file.clone(),
                witness_build_target_atoms: witness.describe_defined_cfg_atoms(),
            });
        }
    }
    Ok(())
}

/// Refuse a processor group whose arms derive different manifest entries.
fn refuse_divergent_processor_declarations(
    processor_type_name: &str,
    declaring_arms: &[&ExtractedProcessor],
) -> Result<(), ExtractError> {
    let Some((first, rest)) = declaring_arms.split_first() else {
        return Ok(());
    };
    for second in rest {
        let Some(difference) = describe_divergent_processor_declarations(first, second) else {
            continue;
        };
        return Err(ExtractError::DivergentProcessorDeclarations {
            processor_type_name: processor_type_name.to_string(),
            first_declared_in: first.source_file.clone(),
            second_declared_in: second.source_file.clone(),
            difference,
        });
    }
    Ok(())
}

fn walk_processor_source_arms(
    package_dir: &Path,
    cfg_resolution: ModuleWalkCfgResolution<'_>,
) -> Result<Vec<ExtractedProcessor>, ExtractError> {
    let arms = enumerate_processor_source_module_arms(package_dir)?;

    let mut walker = ReachableModuleWalker {
        package_dir,
        cfg_resolution,
        top_level_arm_module_files: arms.iter().map(|arm| arm.module_file.clone()).collect(),
        visited: BTreeSet::new(),
        active_cfg_predicates: Vec::new(),
        module_path_segments: Vec::new(),
        enclosing_private_module_name: None,
        out: Vec::new(),
    };

    for arm in &arms {
        walker.module_path_segments.push(arm.module_name.clone());
        let arm_child_module_search_dir = arm.top_level_arm_child_module_search_dir();
        walker.walk_file(&arm.module_file, &arm_child_module_search_dir)?;
        walker.module_path_segments.pop();
    }

    tracing::debug!(processors = walker.out.len(), "extracted (reachable)");
    Ok(walker.out)
}

/// Carries the walk state so the recursive descent doesn't thread six
/// parameters through every call.
struct ReachableModuleWalker<'walk> {
    package_dir: &'walk Path,
    cfg_resolution: ModuleWalkCfgResolution<'walk>,
    /// Every top-level arm's own module file, so a child `mod` resolving onto
    /// one is refused instead of silently deciding the module path by arm order.
    top_level_arm_module_files: BTreeSet<PathBuf>,
    visited: BTreeSet<PathBuf>,
    /// `#[cfg]` predicates passed through to reach the current item, outermost
    /// first. Only recorded under [`ModuleWalkCfgResolution::AcrossEveryBuildTarget`].
    active_cfg_predicates: Vec<String>,
    /// `mod` segments from the crate root to the current module.
    module_path_segments: Vec<String>,
    /// The outermost `mod` on the current descent that carries no visibility
    /// modifier, if any — everything under it is unnameable from the crate root.
    enclosing_private_module_name: Option<String>,
    out: Vec<ExtractedProcessor>,
}

impl ReachableModuleWalker<'_> {
    /// Parse `file` and process its items. `mod_dir` is the directory that this
    /// file's `mod <name>;` children resolve against: the arm's own directory
    /// for a `#[path]`-declared arm, `<parent>/foo/` for a plain `mod foo;`
    /// that resolved to `foo.rs`.
    fn walk_file(&mut self, file: &Path, mod_dir: &Path) -> Result<(), ExtractError> {
        // A module file is reachable via exactly one `mod` path in valid Rust,
        // but guard against re-processing (and any pathological `#[path]` alias).
        let canonical = file.to_path_buf();
        if !self.visited.insert(canonical) {
            return Ok(());
        }

        let body = std::fs::read_to_string(file).map_err(|e| ExtractError::Io {
            path: file.to_path_buf(),
            source: e,
        })?;
        let parsed = syn::parse_file(&body).map_err(|e| ExtractError::Syntax {
            path: file.to_path_buf(),
            source: e,
        })?;

        // A file-level inner `#![cfg(...)]` gates the module the file *is*, so a
        // false predicate strips the file and everything it declares — including
        // its `mod` children. `syn` keeps these on `File::attrs`; an inline
        // `mod foo { #![cfg] }` instead folds them into `ItemMod::attrs`, which
        // `walk_item` gates.
        let Some(pushed_predicate_count) = self.enter_cfg_scope(&parsed.attrs) else {
            self.warn_if_an_undefined_cargo_feature_pruned_a_processor(&parsed.attrs, file, || {
                self.pruned_module_file_declares_a_processor(file, mod_dir)
            });
            tracing::trace!(file = %file.display(), "module file excluded by file-level cfg");
            return Ok(());
        };

        let rel = file
            .strip_prefix(self.package_dir)
            .unwrap_or(file)
            .to_path_buf();
        // `#[path]` on a non-inline `mod` resolves relative to the directory
        // holding the DECLARING file, which differs from `mod_dir` for a
        // non-`mod.rs` file (`src/a/b.rs` declares against `src/a/`, while its
        // plain `mod` children resolve against `src/a/b/`).
        let path_attribute_base_dir = file.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        for item in &parsed.items {
            self.walk_item(item, file, mod_dir, &path_attribute_base_dir, &rel)?;
        }
        self.leave_cfg_scope(pushed_predicate_count);
        Ok(())
    }

    /// Process one item: collect a reachable `#[processor(...)]` struct, or
    /// descend into a cfg-reachable module (inline or external).
    fn walk_item(
        &mut self,
        item: &syn::Item,
        declaring_file: &Path,
        mod_dir: &Path,
        path_attribute_base_dir: &Path,
        rel_path: &Path,
    ) -> Result<(), ExtractError> {
        match item {
            syn::Item::Struct(item_struct) => {
                // A struct behind a false `#[cfg(...)]` is not compiled, so its
                // `#[processor]` (if any) is not a real processor for this target.
                let Some(pushed_predicate_count) = self.enter_cfg_scope(&item_struct.attrs) else {
                    self.warn_if_an_undefined_cargo_feature_pruned_a_processor(
                        &item_struct.attrs,
                        declaring_file,
                        || processor_attr(&item_struct.attrs).is_some(),
                    );
                    return Ok(());
                };
                if let Some(attr) = processor_attr(&item_struct.attrs) {
                    if let Some(private_module) = &self.enclosing_private_module_name {
                        return Err(ExtractError::ProcessorBehindPrivateModule {
                            struct_name: item_struct.ident.to_string(),
                            private_module: private_module.clone(),
                            declared_in: declaring_file.to_path_buf(),
                        });
                    }
                    self.out.push(parse_processor_attr(
                        attr,
                        &item_struct.ident,
                        ProcessorDeclarationSite {
                            source_file: rel_path,
                            module_path_segments: &self.module_path_segments,
                            cfg_predicates: &self.active_cfg_predicates,
                        },
                    )?);
                }
                self.leave_cfg_scope(pushed_predicate_count);
            }
            syn::Item::Mod(item_mod) => {
                let Some(pushed_predicate_count) = self.enter_cfg_scope(&item_mod.attrs) else {
                    self.warn_if_an_undefined_cargo_feature_pruned_a_processor(
                        &item_mod.attrs,
                        declaring_file,
                        || {
                            self.pruned_module_item_declares_a_processor(
                                item,
                                declaring_file,
                                mod_dir,
                                path_attribute_base_dir,
                                rel_path,
                            )
                        },
                    );
                    return Ok(());
                };
                self.module_path_segments.push(item_mod.ident.to_string());
                let opened_private_module = self.enclosing_private_module_name.is_none()
                    && matches!(item_mod.vis, syn::Visibility::Inherited);
                if opened_private_module {
                    self.enclosing_private_module_name = Some(item_mod.ident.to_string());
                }
                match &item_mod.content {
                    // Inline `mod foo { ... }` introduces a `foo` directory
                    // component for its children's file resolution — including
                    // for a `#[path]` on a child, which is why the base dir
                    // tracks the inline components from here down.
                    Some((_, items)) => {
                        let inner_dir = mod_dir.join(item_mod.ident.to_string());
                        for inner in items {
                            self.walk_item(
                                inner,
                                declaring_file,
                                &inner_dir,
                                &inner_dir,
                                rel_path,
                            )?;
                        }
                    }
                    // External `mod foo;` resolves to a sibling file.
                    None => {
                        let resolved = self.resolve_module_file(
                            item_mod,
                            declaring_file,
                            mod_dir,
                            path_attribute_base_dir,
                        )?;
                        if self
                            .top_level_arm_module_files
                            .contains(&resolved.module_file)
                        {
                            return Err(ExtractError::ProcessorSourceArmDeclaredAsChildModule {
                                module: item_mod.ident.to_string(),
                                declared_in: declaring_file.to_path_buf(),
                                resolved: resolved.module_file,
                            });
                        }
                        let child_mod_dir = resolved_child_module_search_dir(
                            &resolved,
                            &item_mod.ident.to_string(),
                        );
                        self.walk_file(&resolved.module_file, &child_mod_dir)?;
                    }
                }
                self.module_path_segments.pop();
                if opened_private_module {
                    self.enclosing_private_module_name = None;
                }
                self.leave_cfg_scope(pushed_predicate_count);
            }
            _ => {}
        }
        Ok(())
    }

    /// Resolve an external `mod <name>;` to its source file, honoring a
    /// `#[path = "..."]` override and otherwise the standard
    /// `<mod_dir>/<name>.rs` then `<mod_dir>/<name>/mod.rs` search.
    ///
    /// `rustc` resolves a `#[path]` on a non-inline module against the directory
    /// holding the declaring file (`path_attribute_base_dir`), which is NOT the
    /// directory that module's plain `mod` children resolve against.
    fn resolve_module_file(
        &self,
        item_mod: &syn::ItemMod,
        declaring_file: &Path,
        mod_dir: &Path,
        path_attribute_base_dir: &Path,
    ) -> Result<ResolvedChildModule, ExtractError> {
        let name = item_mod.ident.to_string();

        if let Some(path_attr) = path_override(&item_mod.attrs) {
            let candidate = path_attribute_base_dir.join(&path_attr);
            if candidate.is_file() {
                return Ok(ResolvedChildModule {
                    module_file: candidate,
                    resolved_via_path_attribute: true,
                });
            }
            return Err(ExtractError::UnresolvedModule {
                module: name,
                declared_in: declaring_file.to_path_buf(),
                candidates: candidate.display().to_string(),
            });
        }

        let flat = mod_dir.join(format!("{name}.rs"));
        if flat.is_file() {
            return Ok(ResolvedChildModule {
                module_file: flat,
                resolved_via_path_attribute: false,
            });
        }
        let nested = mod_dir.join(&name).join("mod.rs");
        if nested.is_file() {
            return Ok(ResolvedChildModule {
                module_file: nested,
                resolved_via_path_attribute: false,
            });
        }
        Err(ExtractError::UnresolvedModule {
            module: name,
            declared_in: declaring_file.to_path_buf(),
            candidates: format!("{} or {}", flat.display(), nested.display()),
        })
    }

    /// Enter the cfg scope an item's attributes open. Returns `None` when the
    /// item is not compiled (so the caller skips it), otherwise the number of
    /// predicates pushed, to be handed back to [`Self::leave_cfg_scope`].
    fn enter_cfg_scope(&mut self, attrs: &[syn::Attribute]) -> Option<usize> {
        match self.cfg_resolution {
            ModuleWalkCfgResolution::AgainstBuildTarget(target) => attrs
                .iter()
                .filter(|attr| attr.path().is_ident("cfg"))
                .all(|attr| eval_cfg_attr(attr, target))
                .then_some(0),
            ModuleWalkCfgResolution::AcrossEveryBuildTarget => {
                let any_predicate_is_unsatisfiable = attrs
                    .iter()
                    .filter(|attr| attr.path().is_ident("cfg"))
                    .any(|attr| {
                        attr.parse_args::<Meta>()
                            .is_ok_and(|meta| cfg_predicate_is_statically_unsatisfiable(&meta))
                    });
                if any_predicate_is_unsatisfiable {
                    return None;
                }
                let predicates = cfg_predicate_sources(attrs);
                let pushed = predicates.len();
                self.active_cfg_predicates.extend(predicates);
                Some(pushed)
            }
        }
    }

    fn leave_cfg_scope(&mut self, pushed_predicate_count: usize) {
        self.active_cfg_predicates
            .truncate(self.active_cfg_predicates.len() - pushed_predicate_count);
    }

    /// Say so when a target-resolved walk pruned a `#[processor(...)]` for want
    /// of a cargo feature.
    ///
    /// [`ModuleReachabilityTarget::for_host`] derives `target_os` / `target_arch`
    /// / `target_family` plus the family flag and infers no features — it cannot
    /// know which features a downstream build enables. A feature-gated
    /// `#[processor(...)]` therefore evaluates false and leaves the derived set,
    /// and the publish-time drift gate then reports the committed entry as
    /// "listed in `processors:` but no longer declared in code" — a confusing
    /// error rather than a quiet one. Naming the file, the predicate and the
    /// missing feature turns it into an actionable one.
    ///
    /// `pruned_scope_declares_a_processor` is what keeps the message true: a
    /// feature-gated helper module under `processors/` declares no processor,
    /// so nothing left the derived set and there is nothing to explain.
    fn warn_if_an_undefined_cargo_feature_pruned_a_processor(
        &self,
        attrs: &[syn::Attribute],
        declaring_file: &Path,
        pruned_scope_declares_a_processor: impl FnOnce() -> bool,
    ) {
        let ModuleWalkCfgResolution::AgainstBuildTarget(target) = self.cfg_resolution else {
            return;
        };
        let mut pruning_predicates: Vec<String> = Vec::new();
        let mut undefined_features: BTreeSet<String> = BTreeSet::new();
        for attr in attrs.iter().filter(|attr| attr.path().is_ident("cfg")) {
            let Ok(meta) = attr.parse_args::<Meta>() else {
                continue;
            };
            if eval_cfg_meta(&meta, target) {
                continue;
            }
            let mut features_this_predicate_names = BTreeSet::new();
            collect_undefined_feature_atoms(&meta, target, &mut features_this_predicate_names);
            if features_this_predicate_names.is_empty() {
                continue;
            }
            pruning_predicates.push(render_cfg_predicate(&meta));
            undefined_features.extend(features_this_predicate_names);
        }
        if undefined_features.is_empty() || !pruned_scope_declares_a_processor() {
            return;
        }
        tracing::warn!(
            file = %declaring_file.display(),
            predicate = %pruning_predicates.join(", "),
            undefined_features = %undefined_features.into_iter().collect::<Vec<_>>().join(", "),
            "pruned a `#[processor(...)]` behind a cfg gated on cargo features the scan target \
             does not define — it is absent from the derived `processors:` set and will read as \
             removed from code; declare the feature on the target with \
             `ModuleReachabilityTarget::with_feature` if the build enables it"
        );
    }

    /// Whether the module file a cfg predicate pruned would have declared a
    /// `#[processor(...)]`.
    fn pruned_module_file_declares_a_processor(&self, file: &Path, mod_dir: &Path) -> bool {
        let mut across_every_build_target = self.walker_across_every_build_target();
        across_every_build_target.walk_file(file, mod_dir).is_ok()
            && !across_every_build_target.out.is_empty()
    }

    /// Whether the `mod` a cfg predicate pruned would have declared a
    /// `#[processor(...)]`, in its own body or in any file below it.
    fn pruned_module_item_declares_a_processor(
        &self,
        item: &syn::Item,
        declaring_file: &Path,
        mod_dir: &Path,
        path_attribute_base_dir: &Path,
        rel_path: &Path,
    ) -> bool {
        let mut across_every_build_target = self.walker_across_every_build_target();
        across_every_build_target
            .walk_item(
                item,
                declaring_file,
                mod_dir,
                path_attribute_base_dir,
                rel_path,
            )
            .is_ok()
            && !across_every_build_target.out.is_empty()
    }

    /// A second walker over the same package with cfg pruning off — how a
    /// pruned scope is asked what it would have contributed. Its own walk
    /// raises no diagnostics (the warning is target-resolved only), so this
    /// cannot recurse.
    fn walker_across_every_build_target(&self) -> ReachableModuleWalker<'_> {
        ReachableModuleWalker {
            package_dir: self.package_dir,
            cfg_resolution: ModuleWalkCfgResolution::AcrossEveryBuildTarget,
            top_level_arm_module_files: self.top_level_arm_module_files.clone(),
            visited: BTreeSet::new(),
            active_cfg_predicates: Vec::new(),
            module_path_segments: Vec::new(),
            enclosing_private_module_name: None,
            out: Vec::new(),
        }
    }
}

/// Every `feature = "<name>"` atom in a predicate that `target` does not
/// define.
fn collect_undefined_feature_atoms(
    meta: &Meta,
    target: &ModuleReachabilityTarget,
    out: &mut BTreeSet<String>,
) {
    match meta {
        Meta::Path(_) => {}
        Meta::NameValue(name_value) => {
            if !name_value.path.is_ident("feature") {
                return;
            }
            if let Some(name) = literal_str(&name_value.value)
                && !target.has_key_value("feature", &name)
            {
                out.insert(name);
            }
        }
        Meta::List(list) => {
            let Some((combinator, inner)) = parse_cfg_combinator(list) else {
                return;
            };
            // `not(feature = "x")` is satisfied BY the feature's absence, so a
            // scope it prunes was pruned by the feature being present, not
            // missing — the confusion this warns about cannot arise there.
            if !matches!(combinator.as_str(), "all" | "any") {
                return;
            }
            for operand in &inner {
                collect_undefined_feature_atoms(operand, target, out);
            }
        }
    }
}

/// A child `mod` resolved to its file, plus how it was resolved — `rustc` gives
/// a `#[path]` module the directory holding its file, with no extra component,
/// which is a different rule from the plain `foo.rs` / `foo/mod.rs` search.
struct ResolvedChildModule {
    module_file: PathBuf,
    resolved_via_path_attribute: bool,
}

/// Evaluate a single `#[cfg(<predicate>)]`. A malformed predicate the parser
/// can't read is treated as unreachable (conservative: never over-collect a
/// processor from a cfg we could not prove true).
fn eval_cfg_attr(attr: &syn::Attribute, target: &ModuleReachabilityTarget) -> bool {
    match attr.parse_args::<Meta>() {
        Ok(meta) => eval_cfg_meta(&meta, target),
        Err(_) => false,
    }
}

/// Evaluate a cfg predicate meta: `all(..)` / `any(..)` / `not(..)`
/// combinators, `key = "value"` atoms, and bare flag atoms.
fn eval_cfg_meta(meta: &Meta, target: &ModuleReachabilityTarget) -> bool {
    match meta {
        Meta::Path(path) => path
            .get_ident()
            .is_some_and(|ident| target.has_flag(&ident.to_string())),
        Meta::NameValue(name_value) => {
            let Some(key) = name_value.path.get_ident().map(|i| i.to_string()) else {
                return false;
            };
            match literal_str(&name_value.value) {
                Some(value) => target.has_key_value(&key, &value),
                None => false,
            }
        }
        Meta::List(list) => {
            let Some((combinator, inner)) = parse_cfg_combinator(list) else {
                return false;
            };
            match combinator.as_str() {
                // `all()` is vacuously true; `any()` is vacuously false —
                // which is exactly why `#[cfg(any())]` parks a module.
                "all" => inner.iter().all(|m| eval_cfg_meta(m, target)),
                "any" => inner.iter().any(|m| eval_cfg_meta(m, target)),
                "not" => inner.len() == 1 && !eval_cfg_meta(&inner[0], target),
                _ => false,
            }
        }
    }
}

/// Whether a cfg predicate is false on EVERY target, structurally — the parked
/// convention `#[cfg(any())]` and anything that folds down to it. An atom is
/// satisfiable by definition (some target defines it), so only the vacuous
/// `any()` and combinations built from it fold to unsatisfiable.
fn cfg_predicate_is_statically_unsatisfiable(meta: &Meta) -> bool {
    let Meta::List(list) = meta else {
        return false;
    };
    let Some((combinator, inner)) = parse_cfg_combinator(list) else {
        return false;
    };
    match combinator.as_str() {
        "any" => inner.iter().all(cfg_predicate_is_statically_unsatisfiable),
        "all" => inner.iter().any(cfg_predicate_is_statically_unsatisfiable),
        _ => false,
    }
}

/// Fold the `#[cfg(...)]` predicates in force at one declaration into a single
/// predicate. Nested predicates are ANDed the way `rustc` applies them; no
/// predicate at all is unconditional.
///
/// The one owner of the conjunction rule: the crate-root generator renders it
/// onto an `export_plugin!` entry and the availability resolution folds it into
/// a processor's availability predicate, off the same function.
pub(crate) fn conjoin_cfg_predicates(predicates: &[String]) -> Option<String> {
    match predicates {
        [] => None,
        [single] => Some(single.clone()),
        many => Some(format!("all({})", many.join(", "))),
    }
}

/// Fold alternative `#[cfg(...)]` predicates into their disjunction, `None`
/// when the alternatives are unconditional — either because one of them is
/// (its disjunction with anything holds everywhere, and an `any(...)` naming
/// the others would understate where it holds) or because there are none to
/// gate on.
///
/// The one owner of the disjunction rule: a processor's availability across its
/// declaring arms and the crate root's `export_plugin!` gate across the
/// package's processors are the same fold.
pub(crate) fn disjoin_cfg_predicates_when_every_alternative_is_conditional<'predicate>(
    alternatives: impl IntoIterator<Item = Option<&'predicate str>>,
) -> Option<String> {
    let conditional_alternatives: Option<Vec<&str>> = alternatives.into_iter().collect();
    let mut distinct: Vec<&str> = Vec::new();
    for predicate in conditional_alternatives? {
        if !distinct.contains(&predicate) {
            distinct.push(predicate);
        }
    }
    match distinct.as_slice() {
        [] => None,
        [single] => Some((*single).to_string()),
        many => Some(format!("any({})", many.join(", "))),
    }
}

/// cfg keys the model lets a build target define more than one value for.
///
/// Every other key — `target_os`, `target_arch`, `target_family`, `target_env`,
/// `target_vendor`, `target_endian`, `target_pointer_width`, `target_abi`,
/// `panic`, and any custom key — is modelled as admitting at most one value,
/// which is what makes `target_os = "linux"` and `any(target_os = "macos",
/// target_os = "ios")` provably disjoint rather than reading as satisfiable.
/// Single-valued is the deliberate default: modelling a genuinely set-valued
/// key as single-valued can only ever MISS an overlap (the concrete-target net
/// still catches it), while the reverse would invent one and refuse a correct
/// package.
///
/// `target_family` is the known exception the model deliberately keeps
/// single-valued: `rustc` sets it twice for the wasm-with-libc targets (`wasi`
/// and `emscripten` are both `wasm` AND `unix`). Those OSes are therefore
/// absent from [`TARGET_FAMILIES_BY_KNOWN_TARGET_OS`], which makes a candidate
/// assigning one unusable as a witness rather than a wrong one.
const SET_VALUED_CFG_KEYS: [&str; 3] = ["feature", "target_feature", "target_has_atomic"];

/// The `target_family` values `rustc` defines for a `target_os`, for the single-
/// family OSes the model knows.
///
/// The implication the atom search cannot derive on its own: nothing in a
/// predicate's own atoms says that `target_os = "ios"` forces `unix`, so
/// without this table a candidate is free to assign the pair `target_os =
/// "ios"` + no `unix` — a target that does not exist — and offer it as a proof
/// that a `#[cfg(target_os = "ios")]` arm and a `#[cfg(not(unix))]` arm
/// overlap.
///
/// Deliberately partial. A `target_os` this table does not name determines
/// nothing about the target's families, so a candidate assigning one cannot
/// witness a predicate pair that mentions a family atom at all (see
/// [`CfgPredicateAtomUniverse::candidate_build_target_is_coherent`]) — an
/// unknown OS costs a missed overlap, never a false one, which is why the
/// multi-family wasm targets are left out rather than approximated.
const TARGET_FAMILIES_BY_KNOWN_TARGET_OS: &[(&str, &[&str])] = &[
    ("android", &["unix"]),
    ("dragonfly", &["unix"]),
    ("freebsd", &["unix"]),
    ("fuchsia", &["unix"]),
    ("haiku", &["unix"]),
    ("illumos", &["unix"]),
    ("ios", &["unix"]),
    ("linux", &["unix"]),
    ("macos", &["unix"]),
    ("netbsd", &["unix"]),
    ("none", &[]),
    ("openbsd", &["unix"]),
    ("redox", &["unix"]),
    ("solaris", &["unix"]),
    ("tvos", &["unix"]),
    ("visionos", &["unix"]),
    ("watchos", &["unix"]),
    ("windows", &["windows"]),
];

/// The families [`TARGET_FAMILIES_BY_KNOWN_TARGET_OS`] pins for a `target_os`,
/// or `None` for one it does not name.
fn target_families_of_known_target_os(target_os: &str) -> Option<&'static [&'static str]> {
    TARGET_FAMILIES_BY_KNOWN_TARGET_OS
        .iter()
        .find(|(known_target_os, _)| *known_target_os == target_os)
        .map(|(_, families)| *families)
}

/// The `target_*` cfg keys the coherence rules can weigh against each other:
/// `target_os` fixes a target's families, and `target_family` — spelled as a
/// value or as the bare `unix` / `windows` flag — is that same fact.
const TARGET_CFG_KEYS_THE_COHERENCE_RULES_RELATE: [&str; 2] = ["target_os", "target_family"];

/// Ceiling on the assignment search. A processor group's predicates mention a
/// handful of atoms in practice; a pathological one is left unproven (no
/// refusal) rather than searched to a stall.
const MAX_CANDIDATE_BUILD_TARGET_ASSIGNMENTS: usize = 4096;

/// A build target on which every predicate in BOTH sets holds, or `None` when
/// none was found.
///
/// Exhaustive over the atoms the two sets themselves mention — a predicate's
/// truth depends only on the atoms it names — so a `Some` carries its own
/// witness. That polarity is the whole point: the caller refuses a build on it,
/// so the witness must be an assignment some real target could have, which is
/// what [`CfgPredicateAtomUniverse::candidate_build_target_is_coherent`] is
/// for. Every rule there PRUNES candidates, and an assignment pinning target
/// atoms the rules relate to nothing is pruned rather than guessed at — so the
/// model's error direction is toward `None`, on the one assumption it cannot
/// check: that a value a predicate names is a value some target defines — two
/// arms both gated on a misspelled `target_os = "linxu"` still witness each
/// other, which is the answer an author wants anyway.
///
/// A `None` is therefore "not proven", NOT "proven disjoint": an unparseable
/// predicate, a search past the ceiling, a `target_os` outside
/// [`TARGET_FAMILIES_BY_KNOWN_TARGET_OS`] weighed against a family atom, a pair
/// resting on two target keys the rules cannot relate (`target_env` against
/// `target_os`), and a genuinely disjoint pair all land there. The
/// concrete-target duplicate check stays the backstop for every one of them.
fn find_build_target_satisfying_both_cfg_predicate_sets(
    first_predicate_sources: &[String],
    second_predicate_sources: &[String],
) -> Option<ModuleReachabilityTarget> {
    let first_predicates = parse_cfg_predicate_sources(first_predicate_sources)?;
    let second_predicates = parse_cfg_predicate_sources(second_predicate_sources)?;

    let mut atoms = CfgPredicateAtomUniverse::default();
    for predicate in first_predicates.iter().chain(&second_predicates) {
        atoms.collect_from(predicate);
    }

    atoms.candidate_build_targets()?.find(|candidate| {
        first_predicates
            .iter()
            .chain(&second_predicates)
            .all(|predicate| eval_cfg_meta(predicate, candidate))
    })
}

/// Parse every recorded predicate source back into a `Meta`, or `None` if any
/// one of them is unreadable — a predicate the evaluator cannot read cannot
/// take part in a proof.
fn parse_cfg_predicate_sources(predicate_sources: &[String]) -> Option<Vec<Meta>> {
    predicate_sources
        .iter()
        .map(|source| syn::parse_str::<Meta>(source).ok())
        .collect()
}

/// Every cfg atom a set of predicates mentions — the search space an overlap
/// proof enumerates. Atoms outside it are irrelevant: a predicate's truth
/// depends only on the atoms it names.
#[derive(Debug, Default)]
struct CfgPredicateAtomUniverse {
    /// Values mentioned per single-valued cfg key (`target_os = "linux"`).
    single_valued_key_values: BTreeMap<String, BTreeSet<String>>,
    /// Values mentioned per set-valued cfg key (`feature = "cuda"`).
    set_valued_key_values: BTreeMap<String, BTreeSet<String>>,
    /// Bare flag atoms mentioned (`unix`).
    flags: BTreeSet<String>,
}

impl CfgPredicateAtomUniverse {
    fn collect_from(&mut self, meta: &Meta) {
        match meta {
            Meta::Path(path) => {
                if let Some(ident) = path.get_ident() {
                    self.flags.insert(ident.to_string());
                }
            }
            Meta::NameValue(name_value) => {
                let (Some(key), Some(value)) = (
                    name_value.path.get_ident().map(|ident| ident.to_string()),
                    literal_str(&name_value.value),
                ) else {
                    return;
                };
                let by_key = if SET_VALUED_CFG_KEYS.contains(&key.as_str()) {
                    &mut self.set_valued_key_values
                } else {
                    &mut self.single_valued_key_values
                };
                by_key.entry(key).or_default().insert(value);
            }
            Meta::List(list) => {
                let Some((combinator, inner)) = parse_cfg_combinator(list) else {
                    return;
                };
                if !matches!(combinator.as_str(), "all" | "any" | "not") {
                    return;
                }
                for operand in &inner {
                    self.collect_from(operand);
                }
            }
        }
    }

    /// Every coherent build target that assigns these atoms, or `None` when the
    /// search space exceeds [`MAX_CANDIDATE_BUILD_TARGET_ASSIGNMENTS`].
    ///
    /// Lazy: the caller stops at the first assignment that satisfies both
    /// predicate sets, and the ceiling is what bounds the walk, so materializing
    /// the space would only build targets nobody looks at.
    fn candidate_build_targets(
        &self,
    ) -> Option<impl Iterator<Item = ModuleReachabilityTarget> + '_> {
        let choices = self.candidate_atom_choices();
        let mut assignment_count: usize = 1;
        for choice in &choices {
            assignment_count = assignment_count.checked_mul(choice.arity())?;
            if assignment_count > MAX_CANDIDATE_BUILD_TARGET_ASSIGNMENTS {
                tracing::debug!(
                    atoms = choices.len(),
                    "cfg overlap search space exceeds the ceiling — leaving the pair unproven"
                );
                return None;
            }
        }

        Some((0..assignment_count).filter_map(move |encoded| {
            let mut remaining = encoded;
            let mut candidate = ModuleReachabilityTarget::new();
            for choice in &choices {
                let arity = choice.arity();
                choice.apply(remaining % arity, &mut candidate);
                remaining /= arity;
            }
            self.candidate_build_target_is_coherent(&candidate)
                .then_some(candidate)
        }))
    }

    fn candidate_atom_choices(&self) -> Vec<CandidateCfgAtomChoice<'_>> {
        let mut choices = Vec::new();
        for (key, values) in &self.single_valued_key_values {
            choices.push(CandidateCfgAtomChoice::SingleValuedKey {
                key,
                values: values.iter().map(String::as_str).collect(),
            });
        }
        for (key, values) in &self.set_valued_key_values {
            for value in values {
                choices.push(CandidateCfgAtomChoice::SetValuedKeyValue { key, value });
            }
        }
        for flag in &self.flags {
            choices.push(CandidateCfgAtomChoice::Flag(flag));
        }
        choices
    }

    /// Whether the predicates mention a bare flag atom.
    fn mentions_flag(&self, flag: &str) -> bool {
        self.flags.contains(flag)
    }

    /// Whether the predicates mention `key = "value"` for a single-valued key.
    fn mentions_single_valued_key_value(&self, key: &str, value: &str) -> bool {
        self.single_valued_key_values
            .get(key)
            .is_some_and(|mentioned| mentioned.contains(value))
    }

    /// Whether the predicates mention any atom whose truth is decided by the
    /// target's families — the atoms a `target_os` outside
    /// [`TARGET_FAMILIES_BY_KNOWN_TARGET_OS`] cannot decide.
    fn mentions_a_target_family_atom(&self) -> bool {
        self.mentions_flag("unix")
            || self.mentions_flag("windows")
            || self.single_valued_key_values.contains_key("target_family")
    }

    /// Whether a candidate pins a `target_*` key from outside the cluster
    /// [`TARGET_CFG_KEYS_THE_COHERENCE_RULES_RELATE`] names, while a second
    /// target atom is decided.
    ///
    /// `rustc` fixes the target keys against one another — no target is both
    /// `target_env = "msvc"` and `target_os = "linux"`, none is both
    /// `target_arch = "wasm32"` and `target_env = "msvc"`, no `target_vendor =
    /// "apple"` target is `not(unix)` — and the model holds none of those
    /// facts. Weighing two of them is therefore a guess, and this search's
    /// answers are read as proofs, so the candidate is dropped. A mentioned
    /// bare family flag counts as that second atom whether the candidate
    /// defines it or not — an assignment decides a flag both ways, unlike a
    /// single-valued key it can leave at "some value the predicates never
    /// name".
    fn candidate_pins_target_cfg_keys_the_rules_cannot_relate(
        &self,
        candidate: &ModuleReachabilityTarget,
    ) -> bool {
        let mut pinned_relatable_keys: BTreeSet<&str> = BTreeSet::new();
        let mut pinned_unrelatable_keys: BTreeSet<&str> = BTreeSet::new();
        for (key, _) in &candidate.key_values {
            if !key.starts_with("target_") {
                continue;
            }
            if TARGET_CFG_KEYS_THE_COHERENCE_RULES_RELATE.contains(&key.as_str()) {
                pinned_relatable_keys.insert(key.as_str());
            } else {
                pinned_unrelatable_keys.insert(key.as_str());
            }
        }
        if pinned_unrelatable_keys.is_empty() {
            return false;
        }
        pinned_relatable_keys.len() + pinned_unrelatable_keys.len() > 1
            || self.mentions_flag("unix")
            || self.mentions_flag("windows")
    }

    /// Whether a candidate assignment describes a build target that could
    /// actually exist. Every rule is a fact about how `rustc` sets these atoms,
    /// so filtering by them only ever removes a target that would have proven a
    /// false overlap — it can never hide a real one:
    ///
    /// - `unix` and `windows` are the two target families a `cfg` names, and no
    ///   target is in both;
    /// - the family flags a target defines are exactly the one its
    ///   `target_family` names — the same fact spelled two ways, so a
    ///   `target_family` outside the pair (`wasm`) defines neither flag. That
    ///   is where modelling `target_family` single-valued costs a witness:
    ///   `wasi` really is `wasm` AND `unix`, and this drops it;
    /// - the windows family holds exactly one `target_os`, `windows`;
    /// - a `target_os` [`TARGET_FAMILIES_BY_KNOWN_TARGET_OS`] names fixes the
    ///   families the target defines, both ways — `target_os = "ios"` forces
    ///   `unix`, and forbids `windows`.
    ///
    /// A `target_os` that table does NOT name fixes nothing, so such a
    /// candidate is rejected outright once a family atom is in play: assigning
    /// it either way would be a guess, and this function's answers are read as
    /// proofs. A candidate pinning target keys the rules relate to nothing is
    /// dropped for the same reason (see
    /// [`CfgPredicateAtomUniverse::candidate_pins_target_cfg_keys_the_rules_cannot_relate`]).
    fn candidate_build_target_is_coherent(&self, candidate: &ModuleReachabilityTarget) -> bool {
        if candidate.has_flag("unix") && candidate.has_flag("windows") {
            return false;
        }
        if self.candidate_pins_target_cfg_keys_the_rules_cannot_relate(candidate) {
            return false;
        }
        let candidate_target_os = candidate.single_defined_value_of_cfg_key("target_os");
        let candidate_target_family = candidate.single_defined_value_of_cfg_key("target_family");

        for family in ["unix", "windows"] {
            if let Some(candidate_target_family) = candidate_target_family
                && self.mentions_flag(family)
                && candidate.has_flag(family) != (candidate_target_family == family)
            {
                return false;
            }
            if candidate.has_flag(family)
                && self.mentions_single_valued_key_value("target_family", family)
                && candidate_target_family != Some(family)
            {
                return false;
            }
        }

        if (candidate.has_flag("windows") || candidate_target_family == Some("windows"))
            && self.mentions_single_valued_key_value("target_os", "windows")
            && candidate_target_os != Some("windows")
        {
            return false;
        }

        let Some(candidate_target_os) = candidate_target_os else {
            return true;
        };
        let Some(families) = target_families_of_known_target_os(candidate_target_os) else {
            return !self.mentions_a_target_family_atom();
        };
        for family in ["unix", "windows"] {
            if self.mentions_flag(family)
                && candidate.has_flag(family) != families.contains(&family)
            {
                return false;
            }
        }
        match candidate_target_family {
            Some(family) => families.contains(&family),
            None => !families
                .iter()
                .any(|family| self.mentions_single_valued_key_value("target_family", family)),
        }
    }
}

/// One slot in the assignment search: a cfg atom (or single-valued key) the
/// candidate build target either defines, or defines a particular value for.
enum CandidateCfgAtomChoice<'atom> {
    /// A key admitting at most one value: each mentioned value, plus "some
    /// other value the predicates never name", which is the arity's `+ 1`.
    SingleValuedKey {
        key: &'atom str,
        values: Vec<&'atom str>,
    },
    /// One `key = "value"` a target either defines or does not, independently
    /// of the key's other values.
    SetValuedKeyValue { key: &'atom str, value: &'atom str },
    /// One bare flag a target either defines or does not.
    Flag(&'atom str),
}

impl CandidateCfgAtomChoice<'_> {
    fn arity(&self) -> usize {
        match self {
            CandidateCfgAtomChoice::SingleValuedKey { values, .. } => values.len() + 1,
            CandidateCfgAtomChoice::SetValuedKeyValue { .. } | CandidateCfgAtomChoice::Flag(_) => 2,
        }
    }

    fn apply(&self, selection: usize, target: &mut ModuleReachabilityTarget) {
        match self {
            CandidateCfgAtomChoice::SingleValuedKey { key, values } => {
                if let Some(value) = values.get(selection) {
                    target
                        .key_values
                        .insert((key.to_string(), value.to_string()));
                }
            }
            CandidateCfgAtomChoice::SetValuedKeyValue { key, value } => {
                if selection == 1 {
                    target
                        .key_values
                        .insert((key.to_string(), value.to_string()));
                }
            }
            CandidateCfgAtomChoice::Flag(flag) => {
                if selection == 1 {
                    target.flags.insert(flag.to_string());
                }
            }
        }
    }
}

/// The `#[path = "..."]` override on a `mod`, if present.
fn path_override(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("path") {
            return None;
        }
        match &attr.meta {
            Meta::NameValue(name_value) => literal_str(&name_value.value),
            _ => None,
        }
    })
}

/// The string value of an expression literal (`"linux"`), if it is one.
fn literal_str(expr: &syn::Expr) -> Option<String> {
    if let syn::Expr::Lit(expr_lit) = expr
        && let syn::Lit::Str(lit_str) = &expr_lit.lit
    {
        return Some(lit_str.value());
    }
    None
}

/// The directory a child module file's own `mod <name>;` children resolve
/// against, matching `rustc`:
///
/// - resolved through `#[path]` — the directory holding the file, with no
///   component introduced, so a `#[path = ".../blur.rs"]` module's children are
///   that file's SIBLINGS while a `#[path = ".../linux/mod.rs"]` module keeps
///   its own directory;
/// - resolved by the standard search — `.../foo/mod.rs` keeps `.../foo`, and
///   `.../foo.rs` introduces the `.../foo` directory component.
fn resolved_child_module_search_dir(resolved: &ResolvedChildModule, mod_name: &str) -> PathBuf {
    let parent = resolved
        .module_file
        .parent()
        .unwrap_or_else(|| Path::new(""));
    if resolved.resolved_via_path_attribute {
        return parent.to_path_buf();
    }
    match resolved.module_file.file_name().and_then(|n| n.to_str()) {
        Some("mod.rs") => parent.to_path_buf(),
        _ => parent.join(mod_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan_fixture_tempdir::{
        ScanFixtureTempDir, scan_fixture_tempdir_named, write_scan_fixture_file as write,
    };

    fn tempdir() -> ScanFixtureTempDir {
        scan_fixture_tempdir_named("slreach")
    }

    fn linux() -> ModuleReachabilityTarget {
        ModuleReachabilityTarget::new()
            .with_key_value("target_os", "linux")
            .with_key_value("target_family", "unix")
            .with_flag("unix")
    }

    fn macos() -> ModuleReachabilityTarget {
        ModuleReachabilityTarget::new()
            .with_key_value("target_os", "macos")
            .with_key_value("target_family", "unix")
            .with_flag("unix")
    }

    fn build_target_for_os(os: &str) -> ModuleReachabilityTarget {
        let family = if os == "windows" { "windows" } else { "unix" };
        ModuleReachabilityTarget::new()
            .with_key_value("target_os", os)
            .with_key_value("target_family", family)
            .with_flag(family)
    }

    fn predicates(sources: &[&str]) -> Vec<String> {
        sources.iter().map(|s| (*s).to_string()).collect()
    }

    fn names(mut procs: Vec<ExtractedProcessor>) -> Vec<String> {
        procs.sort_by(|a, b| a.schema.name.cmp(&b.schema.name));
        procs.into_iter().map(|p| p.schema.name).collect()
    }

    /// A `tracing` writer that keeps what a scan emitted, so a diagnostic can be
    /// asserted on rather than only read in a log.
    #[derive(Clone, Default)]
    struct CapturedScanDiagnosticBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedScanDiagnosticBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedScanDiagnosticBuffer {
        type Writer = Self;
        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }

    /// The `WARN`-level diagnostics a scan emitted, rendered.
    fn warnings_emitted_by(scan: impl FnOnce()) -> String {
        let captured = CapturedScanDiagnosticBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, scan);
        let rendered = captured.0.lock().unwrap().clone();
        String::from_utf8(rendered).unwrap()
    }

    /// The parked-directory convention (`#![cfg(any())]` in
    /// `processors/_apple_impl_pending_/mod.rs`) falls out of cfg evaluation for
    /// free: `any()` is vacuously false, so the parked subtree is never walked
    /// and its `#[processor]` never collected. Mentally revert `eval_cfg_meta`'s
    /// `any` arm to `true` and this fails.
    #[test]
    fn parked_cfg_any_arm_is_excluded() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/linux_impl.rs",
            r#"#![cfg(target_os = "linux")]

            #[processor("@tatolab/demo/Camera", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct Camera;
            "#,
        );
        write(
            root,
            "processors/_apple_impl_pending_/mod.rs",
            r#"#![cfg(any())]

            #[processor("@tatolab/demo/AppleCamera", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct AppleCamera;
            "#,
        );

        let procs = extract_reachable_rust_processors(root, &linux()).unwrap();
        assert_eq!(names(procs), vec!["Camera"]);
    }

    /// A parked arm is invisible to the across-every-target scan too: it
    /// compiles nowhere, so the generated crate root must not name it in an
    /// `export_plugin!` entry that would only ever be stripped.
    #[test]
    fn parked_cfg_any_arm_is_excluded_across_every_build_target() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/_apple_impl_pending_/mod.rs",
            r#"#![cfg(any())]

            #[processor("@tatolab/demo/AppleCamera", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct AppleCamera;
            "#,
        );
        write(
            root,
            "processors/live.rs",
            r#"#[processor("@tatolab/demo/Live", execution = reactive)]
            pub struct Live;"#,
        );

        let set = extract_processors_across_every_build_target(root).unwrap();
        assert_eq!(names(set.processor_declarations), vec!["Live"]);
    }

    /// Two platform arms declaring the same processor: only the arm the target
    /// compiles surfaces (the raw scan would surface both).
    #[test]
    fn cross_platform_arms_resolve_to_the_target_arm() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/linux.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/LinuxCam", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct LinuxCam;"#,
        );
        write(
            root,
            "processors/apple.rs",
            r#"#![cfg(target_os = "macos")]
            #[processor("@tatolab/demo/AppleCam", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct AppleCam;"#,
        );

        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["LinuxCam"]
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &macos()).unwrap()),
            vec!["AppleCam"]
        );
    }

    /// The across-every-target scan keeps both platform arms and records the
    /// author's predicate on each, verbatim.
    #[test]
    fn across_every_build_target_keeps_both_arms_with_their_predicates() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/linux.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/LinuxCam", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct LinuxCam;"#,
        );
        write(
            root,
            "processors/apple.rs",
            r#"#![cfg(any(target_os = "macos", target_os = "ios"))]
            #[processor("@tatolab/demo/AppleCam", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct AppleCam;"#,
        );

        let set = extract_processors_across_every_build_target(root).unwrap();
        let mut by_name: Vec<(String, Vec<String>, Vec<String>)> = set
            .processor_declarations
            .into_iter()
            .map(|p| (p.schema.name, p.cfg_predicates, p.module_path_segments))
            .collect();
        by_name.sort();
        assert_eq!(
            by_name,
            vec![
                (
                    "AppleCam".to_string(),
                    vec![r#"any(target_os = "macos", target_os = "ios")"#.to_string()],
                    vec!["apple".to_string()],
                ),
                (
                    "LinuxCam".to_string(),
                    vec![r#"target_os = "linux""#.to_string()],
                    vec!["linux".to_string()],
                ),
            ]
        );
    }

    /// Predicates nest: a struct-level `#[cfg]` under a file-level one records
    /// both, outermost first, so the generator can `all(...)` them.
    #[test]
    fn nested_cfg_predicates_accumulate_outermost_first() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/gated.rs",
            r#"#![cfg(unix)]

            #[cfg(feature = "cuda")]
            #[processor("@tatolab/demo/CudaOnly", execution = reactive)]
            pub struct CudaOnly;"#,
        );
        let set = extract_processors_across_every_build_target(root).unwrap();
        assert_eq!(set.processor_declarations.len(), 1);
        assert_eq!(
            set.processor_declarations[0].cfg_predicates,
            vec!["unix".to_string(), r#"feature = "cuda""#.to_string()]
        );
        // The same conjunction is what the availability predicate folds to.
        assert_eq!(
            set.availability_of_processor_type_name("CudaOnly")
                .unwrap()
                .availability_cfg_predicate
                .as_deref(),
            Some(r#"all(unix, feature = "cuda")"#)
        );
    }

    /// A `#[processor]` directly on a cfg-gated struct (no module boundary) is
    /// gated too.
    #[test]
    fn cfg_on_the_struct_itself_is_honored() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/arms.rs",
            r#"
            #[cfg(target_os = "linux")]
            #[processor("@tatolab/demo/OnlyLinux", execution = reactive)]
            pub struct OnlyLinux;

            #[cfg(target_os = "windows")]
            #[processor("@tatolab/demo/OnlyWindows", execution = reactive)]
            pub struct OnlyWindows;
            "#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["OnlyLinux"]
        );
    }

    /// A file-level inner `#![cfg(...)]` on a nested module file gates the whole
    /// file, pruning the subtree it declares.
    #[test]
    fn file_level_inner_cfg_prunes_the_child_module_subtree() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/outer/mod.rs", "pub mod gated;\n");
        write(
            root,
            "processors/outer/gated.rs",
            r#"#![cfg(target_os = "linux")]

            pub mod child;"#,
        );
        write(
            root,
            "processors/outer/gated/child.rs",
            r#"#[processor("@tatolab/demo/DeepChild", execution = reactive)]
            pub struct DeepChild;"#,
        );

        assert!(
            extract_reachable_rust_processors(root, &macos())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["DeepChild"]
        );
    }

    /// File-level gates run through the same combinator evaluator as item-level
    /// ones: `all(...)`, `not(...)`, and `any(...)` resolve against the target.
    #[test]
    fn file_level_inner_cfg_evaluates_combinators() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/unix_not_linux.rs",
            r#"#![cfg(all(unix, not(target_os = "linux")))]

            #[processor("@tatolab/demo/UnixNotLinux", execution = reactive)]
            pub struct UnixNotLinux;"#,
        );
        write(
            root,
            "processors/unix_and_linux.rs",
            r#"#![cfg(all(unix, target_os = "linux"))]

            #[processor("@tatolab/demo/UnixAndLinux", execution = reactive)]
            pub struct UnixAndLinux;"#,
        );
        write(
            root,
            "processors/any_exotic.rs",
            r#"#![cfg(any(target_os = "windows", target_os = "redox"))]

            #[processor("@tatolab/demo/AnyExotic", execution = reactive)]
            pub struct AnyExotic;"#,
        );

        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["UnixAndLinux"]
        );
    }

    /// An inner `#![cfg]` inside an *inline* `mod foo { ... }` is folded into
    /// `ItemMod::attrs` by `syn`, so the `mod` arm of the walk already gates it.
    #[test]
    fn inline_mod_inner_cfg_is_honored() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/arm.rs",
            r#"
            pub mod inline_linux {
                #![cfg(target_os = "linux")]

                #[processor("@tatolab/demo/InlineLinux", execution = reactive)]
                pub struct InlineLinux;
            }
            "#,
        );

        assert!(
            extract_reachable_rust_processors(root, &macos())
                .unwrap()
                .is_empty()
        );
        let procs = extract_reachable_rust_processors(root, &linux()).unwrap();
        assert_eq!(names(procs.clone()), vec!["InlineLinux"]);
        assert_eq!(
            procs[0].module_path_segments,
            vec!["arm".to_string(), "inline_linux".to_string()]
        );
    }

    /// The evaluator fails closed: a predicate whose tokens are not a `Meta` at
    /// all means "not reachable", never "reachable".
    #[test]
    fn file_level_inner_cfg_fails_closed_on_an_unparseable_predicate() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/unparseable.rs",
            r#"#![cfg(target_os =)]

            #[processor("@tatolab/demo/Unparseable", execution = reactive)]
            pub struct Unparseable;"#,
        );

        assert!(
            extract_reachable_rust_processors(root, &linux())
                .unwrap()
                .is_empty()
        );
    }

    /// The other fail-closed path: `target_os = 42` *is* a well-formed `Meta`,
    /// so `parse_args` succeeds and the atom lookup — not the parser — is what
    /// must refuse it, because a cfg value is always a string literal.
    #[test]
    fn file_level_inner_cfg_fails_closed_on_a_non_string_predicate_value() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/non_string_value.rs",
            r#"#![cfg(target_os = 42)]

            #[processor("@tatolab/demo/NonStringValue", execution = reactive)]
            pub struct NonStringValue;"#,
        );

        assert!(
            extract_reachable_rust_processors(root, &linux())
                .unwrap()
                .is_empty()
        );
    }

    /// A `.rs` file that is not `mod`-declared from an arm is unreachable — a
    /// scratch file nested under an arm's directory is not compiled and must not
    /// contribute a processor. The raw whole-tree scan would collect it.
    #[test]
    fn undeclared_file_under_an_arm_is_unreachable() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/wired/mod.rs", "pub mod declared;\n");
        write(
            root,
            "processors/wired/declared.rs",
            r#"#[processor("@tatolab/demo/Wired", execution = reactive)]
            pub struct Wired;"#,
        );
        write(
            root,
            "processors/wired/scratch.rs",
            r#"#[processor("@tatolab/demo/Scratch", execution = reactive)]
            pub struct Scratch;"#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["Wired"]
        );
    }

    /// A `#[processor]` behind a plain `mod` cannot be named from the crate
    /// root, so the generated `export_plugin!` entry would be a privacy error
    /// inside generated source. Refused at the scan, pointing at the author's
    /// file.
    #[test]
    fn a_processor_behind_a_private_module_is_refused() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/arm/mod.rs", "mod helper;\n");
        write(
            root,
            "processors/arm/helper.rs",
            r#"#[processor("@tatolab/demo/Hidden", execution = reactive)]
            pub struct Hidden;"#,
        );
        let error = extract_reachable_rust_processors(root, &linux()).unwrap_err();
        assert!(
            matches!(
                &error,
                ExtractError::ProcessorBehindPrivateModule { private_module, struct_name, .. }
                    if private_module == "helper" && struct_name == "Hidden"
            ),
            "{error}"
        );

        // `pub mod helper;` is the fix, and it resolves.
        write(root, "processors/arm/mod.rs", "pub mod helper;\n");
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["Hidden"]
        );
    }

    /// A flat arm declaring an out-of-line `mod` resolves it to a SIBLING under
    /// `processors/`, which the generated crate root already declares as its own
    /// arm — the file would be two modules and only one path reaches the
    /// register list, decided by arm sort order. Refused rather than resolved.
    #[test]
    fn an_arm_reached_as_another_arms_child_module_is_refused() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/a.rs", "pub mod b;\n");
        write(
            root,
            "processors/b.rs",
            r#"#[processor("@tatolab/demo/Shared", execution = reactive)]
            pub struct Shared;"#,
        );
        let error = extract_reachable_rust_processors(root, &linux()).unwrap_err();
        assert!(
            matches!(
                &error,
                ExtractError::ProcessorSourceArmDeclaredAsChildModule { module, .. }
                    if module == "b"
            ),
            "{error}"
        );
    }

    /// `not(...)`, `all(...)`, and `any(...)` combinators evaluate against the
    /// target.
    #[test]
    fn cfg_combinators_evaluate() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/arms.rs",
            r#"
            #[cfg(all(unix, target_arch = "x86_64"))]
            #[processor("@tatolab/demo/UnixX86", execution = reactive)]
            pub struct UnixX86;

            #[cfg(not(target_os = "windows"))]
            #[processor("@tatolab/demo/NotWindows", execution = reactive)]
            pub struct NotWindows;

            #[cfg(any(target_os = "windows", target_os = "redox"))]
            #[processor("@tatolab/demo/Exotic", execution = reactive)]
            pub struct Exotic;
            "#,
        );
        let target = linux().with_key_value("target_arch", "x86_64");
        assert_eq!(
            names(extract_reachable_rust_processors(root, &target).unwrap()),
            vec!["NotWindows", "UnixX86"]
        );
    }

    /// A `#[cfg(feature = "...")]` module is reachable only when the feature is
    /// declared on the target.
    #[test]
    fn feature_gated_module_needs_the_feature() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/cuda.rs",
            r#"#![cfg(feature = "cuda")]
            #[processor("@tatolab/demo/Cuda", execution = reactive)]
            pub struct Cuda;"#,
        );

        assert!(
            extract_reachable_rust_processors(root, &linux())
                .unwrap()
                .is_empty()
        );
        let with_cuda = linux().with_feature("cuda");
        assert_eq!(
            names(extract_reachable_rust_processors(root, &with_cuda).unwrap()),
            vec!["Cuda"]
        );
    }

    /// A `mod.rs`-backed arm keeps its own directory for its children, and a
    /// nested `foo.rs` introduces the `foo/` directory component for its own.
    #[test]
    fn nested_module_resolution_follows_rustc() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/devices/mod.rs", "pub mod webcam;\n");
        write(root, "processors/devices/webcam.rs", "pub mod sensor;\n");
        write(
            root,
            "processors/devices/webcam/sensor.rs",
            r#"#[processor("@tatolab/demo/Sensor", execution = reactive)]
            pub struct Sensor;"#,
        );
        let procs = extract_reachable_rust_processors(root, &linux()).unwrap();
        assert_eq!(names(procs.clone()), vec!["Sensor"]);
        assert_eq!(
            procs[0].module_path_segments,
            vec![
                "devices".to_string(),
                "webcam".to_string(),
                "sensor".to_string()
            ]
        );
    }

    /// `#[path]` on a `mod.rs`-shaped target keeps that file's own directory for
    /// the child's children — `rustc` gives a `#[path]` module the directory
    /// holding its file, and for a `mod.rs` that is the module's own directory.
    #[test]
    fn path_attribute_to_a_mod_rs_keeps_its_directory() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/arm/mod.rs",
            "#[path = \"platform/linux/mod.rs\"]\npub mod linux;\n",
        );
        write(
            root,
            "processors/arm/platform/linux/mod.rs",
            "pub mod helper;\n",
        );
        write(
            root,
            "processors/arm/platform/linux/helper.rs",
            r#"#[processor("@tatolab/demo/PathModRs", execution = reactive)]
            pub struct PathModRs;"#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["PathModRs"]
        );
    }

    /// `#[path]` to a NON-`mod.rs` file makes that module's children SIBLINGS of
    /// the file — `rustc` introduces no directory component for a `#[path]`
    /// module. Mentally revert `resolved_child_module_search_dir` to always appending the
    /// module name and this looks for `blur/helper.rs`, which does not exist.
    #[test]
    fn path_attribute_to_a_flat_file_makes_children_siblings() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/arm/mod.rs",
            "#[path = \"effects/blur.rs\"]\npub mod blur;\n",
        );
        write(root, "processors/arm/effects/blur.rs", "pub mod helper;\n");
        write(
            root,
            "processors/arm/effects/helper.rs",
            r#"#[processor("@tatolab/demo/PathSibling", execution = reactive)]
            pub struct PathSibling;"#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["PathSibling"]
        );
    }

    /// A `#[path]` on a non-inline `mod` resolves against the directory holding
    /// the DECLARING file, not the directory that file's plain `mod` children
    /// resolve against. Declaring file `processors/arm/inner.rs` resolves
    /// `#[path = "aliased.rs"]` to `processors/arm/aliased.rs`, never
    /// `processors/arm/inner/aliased.rs`.
    #[test]
    fn path_attribute_resolves_against_the_declaring_files_directory() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/arm/mod.rs", "pub mod inner;\n");
        write(
            root,
            "processors/arm/inner.rs",
            "#[path = \"aliased.rs\"]\npub mod aliased;\n",
        );
        write(
            root,
            "processors/arm/aliased.rs",
            r#"#[processor("@tatolab/demo/PathBase", execution = reactive)]
            pub struct PathBase;"#,
        );
        // The pre-fix resolution would look here and find nothing.
        write(
            root,
            "processors/arm/inner/decoy.rs",
            "// not a module of anything\n",
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["PathBase"]
        );
    }

    /// A `#[path]` inside an INLINE `mod` block still resolves against the
    /// inline module's directory chain, which for a non-`mod.rs` declaring file
    /// starts with a directory named after that file.
    #[test]
    fn path_attribute_inside_an_inline_module_keeps_the_inline_directory_chain() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/arm/mod.rs", "pub mod inner;\n");
        write(
            root,
            "processors/arm/inner.rs",
            "pub mod nested { #[path = \"aliased.rs\"] pub mod aliased; }\n",
        );
        write(
            root,
            "processors/arm/inner/nested/aliased.rs",
            r#"#[processor("@tatolab/demo/InlinePathBase", execution = reactive)]
            pub struct InlinePathBase;"#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["InlinePathBase"]
        );
    }

    /// A reachable `mod x;` with no backing file is a typed error (a compilable
    /// crate never hits it, but the walk surfaces it rather than dropping a
    /// subtree the target would compile).
    #[test]
    fn missing_reachable_module_file_is_typed_error() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/arm/mod.rs", "pub mod ghost;\n");
        let err = extract_reachable_rust_processors(root, &linux()).unwrap_err();
        match err {
            ExtractError::UnresolvedModule { module, .. } => assert_eq!(module, "ghost"),
            other => panic!("expected UnresolvedModule, got {other:?}"),
        }
    }

    /// An unconditional arm is reachable regardless of target.
    #[test]
    fn unconditional_arm_always_reachable() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/always.rs",
            r#"#[processor("@tatolab/demo/Always", execution = reactive)]
            pub struct Always;"#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &macos()).unwrap()),
            vec!["Always"]
        );
    }

    /// `for_host()` defines the running host's os/arch/family so the crate's own
    /// host arm is reachable.
    #[test]
    fn for_host_defines_host_atoms() {
        let host = ModuleReachabilityTarget::for_host();
        assert!(host.has_key_value("target_os", std::env::consts::OS));
        assert!(host.has_key_value("target_arch", std::env::consts::ARCH));
        assert!(host.has_flag(std::env::consts::FAMILY));
    }

    /// A package with no `processors/` directory derives the empty set — a
    /// schema-only Rust package is first-class, not an error.
    #[test]
    fn missing_processors_dir_yields_no_processors() {
        let tmp = tempdir();
        assert!(
            extract_reachable_rust_processors(tmp.path(), &linux())
                .unwrap()
                .is_empty()
        );
        assert!(
            enumerate_processor_source_module_arms(tmp.path())
                .unwrap()
                .is_empty()
        );
    }

    /// Arm enumeration: flat files and `mod.rs` directories become arms; a
    /// directory without a `mod.rs` and a non-`.rs` file are skipped, because
    /// `processors/` is the shared discovery root for every language.
    #[test]
    fn arm_enumeration_covers_flat_and_directory_arms_and_skips_foreign_entries() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/blur.rs", "");
        write(
            root,
            "processors/capture_backends/mod.rs",
            "#![cfg(target_os = \"linux\")]\n",
        );
        write(root, "processors/vision/detector.py", "");
        write(root, "processors/effect.ts", "");

        let arms = enumerate_processor_source_module_arms(root).unwrap();
        let described: Vec<(String, String, Vec<String>)> = arms
            .into_iter()
            .map(|a| {
                (
                    a.module_name,
                    a.crate_root_relative_module_path,
                    a.file_level_cfg_predicates,
                )
            })
            .collect();
        assert_eq!(
            described,
            vec![
                (
                    "blur".to_string(),
                    "../processors/blur.rs".to_string(),
                    Vec::new()
                ),
                (
                    "capture_backends".to_string(),
                    "../processors/capture_backends/mod.rs".to_string(),
                    vec![r#"target_os = "linux""#.to_string()]
                ),
            ]
        );
    }

    /// The two arms of a real platform-split package (`@tatolab/audio`) —
    /// `target_os = "linux"` against `any(target_os = "macos", target_os =
    /// "ios")`. Modelling `target_os` as SINGLE-VALUED is what makes them
    /// provably disjoint; with the atoms independent this reads satisfiable and
    /// the overlap check would refuse a correct package on every build.
    #[test]
    fn platform_split_arms_have_no_satisfying_build_target() {
        assert!(
            find_build_target_satisfying_both_cfg_predicate_sets(
                &predicates(&[r#"target_os = "linux""#]),
                &predicates(&[r#"any(target_os = "macos", target_os = "ios")"#]),
            )
            .is_none()
        );
    }

    /// A family flag against a `target_os` value inside that family is
    /// satisfiable, and the witness names the assignment that proves it.
    #[test]
    fn a_family_flag_and_an_os_inside_it_have_a_satisfying_build_target() {
        let witness = find_build_target_satisfying_both_cfg_predicate_sets(
            &predicates(&["unix"]),
            &predicates(&[r#"target_os = "linux""#]),
        )
        .expect("unix and target_os = \"linux\" are satisfied together");
        let atoms = witness.describe_defined_cfg_atoms();
        assert!(atoms.contains(r#"target_os = "linux""#), "{atoms}");
        assert!(atoms.contains("unix"), "{atoms}");
    }

    /// The coherence rules are facts about how `rustc` sets these atoms, so
    /// they only ever remove a candidate that would have proven a FALSE overlap:
    /// `unix` and `windows` are never both set, and the windows family holds
    /// exactly one `target_os`. Mentally drop
    /// `candidate_build_target_is_coherent` and each of these reads satisfiable.
    #[test]
    fn incoherent_build_targets_are_not_offered_as_witnesses() {
        assert!(
            find_build_target_satisfying_both_cfg_predicate_sets(
                &predicates(&["unix"]),
                &predicates(&["windows"]),
            )
            .is_none(),
            "no target is in both families"
        );
        assert!(
            find_build_target_satisfying_both_cfg_predicate_sets(
                &predicates(&["windows"]),
                &predicates(&[r#"target_os = "linux""#]),
            )
            .is_none(),
            "the windows family holds only `target_os = \"windows\"`"
        );
        assert!(
            find_build_target_satisfying_both_cfg_predicate_sets(
                &predicates(&["unix"]),
                &predicates(&[r#"target_family = "windows""#]),
            )
            .is_none(),
            "`target_family` and the family flag are the same fact"
        );
    }

    /// A `target_os` the model knows carries its family with it: no target is
    /// both `target_os = "ios"` and `not(unix)`. Without
    /// [`TARGET_FAMILIES_BY_KNOWN_TARGET_OS`] the search assigns that pair
    /// freely, calls it a proof, and refuses the three-arm platform split
    /// below on every build.
    #[test]
    fn a_known_os_and_a_negated_family_flag_have_no_satisfying_build_target() {
        for os_predicate in [
            r#"target_os = "linux""#,
            r#"any(target_os = "macos", target_os = "ios")"#,
            r#"target_os = "android""#,
        ] {
            assert!(
                find_build_target_satisfying_both_cfg_predicate_sets(
                    &predicates(&[os_predicate]),
                    &predicates(&["not(unix)"]),
                )
                .is_none(),
                "`{os_predicate}` names a unix-family OS, so no target satisfies it and `not(unix)`"
            );
        }
        assert!(
            find_build_target_satisfying_both_cfg_predicate_sets(
                &predicates(&[r#"target_os = "windows""#]),
                &predicates(&["not(windows)"]),
            )
            .is_none()
        );
        // The other direction still proves: a unix OS and the family flag it
        // implies really do overlap.
        assert!(
            find_build_target_satisfying_both_cfg_predicate_sets(
                &predicates(&[r#"target_os = "macos""#]),
                &predicates(&["unix"]),
            )
            .is_some()
        );
    }

    /// A `target_os` outside the family table decides nothing, so a pair that
    /// weighs one against a family atom is left UNPROVEN rather than guessed in
    /// either direction — `wasi` is both `wasm` and `unix`, which is exactly the
    /// guess a partial table must not make.
    #[test]
    fn an_unmodelled_os_weighed_against_a_family_atom_is_left_unproven() {
        assert!(
            find_build_target_satisfying_both_cfg_predicate_sets(
                &predicates(&[r#"target_os = "wasi""#]),
                &predicates(&["not(unix)"]),
            )
            .is_none()
        );
        assert!(
            find_build_target_satisfying_both_cfg_predicate_sets(
                &predicates(&[r#"target_os = "wasi""#]),
                &predicates(&["unix"]),
            )
            .is_none()
        );
        // With no family atom in play the same OS proves normally: a cargo
        // feature is free of the target, so the pair really can both compile.
        assert!(
            find_build_target_satisfying_both_cfg_predicate_sets(
                &predicates(&[r#"target_os = "wasi""#]),
                &predicates(&[r#"feature = "cuda""#]),
            )
            .is_some()
        );
    }

    /// The model reads a single-valued `target_family` as THE family, so a
    /// value outside `unix` / `windows` defines neither flag. Without that,
    /// `#![cfg(target_family = "wasm")]` and `#![cfg(windows)]` read satisfiable
    /// — no rustc target is in both families — and a correct package is refused
    /// on every build.
    #[test]
    fn a_target_family_outside_the_family_flags_defines_neither_flag() {
        for family_atom in ["windows", "unix"] {
            assert!(
                find_build_target_satisfying_both_cfg_predicate_sets(
                    &predicates(&[r#"target_family = "wasm""#]),
                    &predicates(&[family_atom]),
                )
                .is_none(),
                "a `wasm` target_family cannot also define `{family_atom}` in this model"
            );
        }
        // The flag a `target_family` DOES name still proves.
        assert!(
            find_build_target_satisfying_both_cfg_predicate_sets(
                &predicates(&[r#"target_family = "unix""#]),
                &predicates(&["unix"]),
            )
            .is_some()
        );
    }

    /// `rustc` fixes the target keys against one another and the model holds
    /// none of those facts, so a pair resting on two of them is left UNPROVEN.
    /// Without that, `#![cfg(target_env = "msvc")]` and `#![cfg(target_os =
    /// "linux")]` read satisfiable on a build target no rustc triple has.
    #[test]
    fn a_pair_resting_on_unrelatable_target_keys_is_left_unproven() {
        for (first, second) in [
            (r#"target_env = "msvc""#, r#"target_os = "linux""#),
            (r#"target_arch = "wasm32""#, r#"target_os = "linux""#),
            (r#"target_vendor = "apple""#, "not(unix)"),
            (r#"target_arch = "wasm32""#, r#"target_env = "msvc""#),
        ] {
            assert!(
                find_build_target_satisfying_both_cfg_predicate_sets(
                    &predicates(&[first]),
                    &predicates(&[second]),
                )
                .is_none(),
                "`{first}` and `{second}` name target keys the model cannot relate"
            );
        }
        // One such key on its own pins nothing against anything.
        assert!(
            find_build_target_satisfying_both_cfg_predicate_sets(
                &predicates(&[r#"target_env = "msvc""#]),
                &predicates(&[r#"not(target_env = "gnu")"#]),
            )
            .is_some()
        );
    }

    /// The end-to-end shape the two coherence gaps refused: arms gated on
    /// families no target holds at once are disjoint, so the scan ACCEPTS them.
    #[test]
    fn arms_split_across_unrelated_target_keys_are_accepted() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/wasm_arm.rs",
            r#"#![cfg(target_family = "wasm")]
            #[processor("@tatolab/demo/Capture", execution = manual)]
            pub struct WasmCapture;"#,
        );
        write(
            root,
            "processors/windows_arm.rs",
            r#"#![cfg(windows)]
            #[processor("@tatolab/demo/Capture", execution = manual)]
            pub struct WindowsCapture;"#,
        );

        let set = extract_processors_across_every_build_target(root)
            .expect("no target is in both the wasm and windows families");
        let availability = set
            .availability_of_processor_type_name("Capture")
            .expect("the two arms fold to one availability entry");
        assert!(availability.is_available_on_build_target(&build_target_for_os("windows")));
    }

    /// A three-arm platform split whose third arm is the `not(unix)` fallback.
    /// All three are pairwise disjoint on every real target, so the scan must
    /// ACCEPT them and fold them into one availability entry.
    #[test]
    fn a_platform_split_with_a_non_unix_fallback_arm_is_accepted() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/linux_arm.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/Capture", execution = manual)]
            pub struct LinuxCapture;"#,
        );
        write(
            root,
            "processors/apple_arm.rs",
            r#"#![cfg(any(target_os = "macos", target_os = "ios"))]
            #[processor("@tatolab/demo/Capture", execution = manual)]
            pub struct AppleCapture;"#,
        );
        write(
            root,
            "processors/other_arm.rs",
            r#"#![cfg(not(unix))]
            #[processor("@tatolab/demo/Capture", execution = manual)]
            pub struct OtherCapture;"#,
        );

        let set = extract_processors_across_every_build_target(root)
            .expect("three pairwise-disjoint arms are not an overlap");
        assert_eq!(set.processor_declarations.len(), 3);
        let availability = set
            .availability_of_processor_type_name("Capture")
            .expect("the three arms fold to one availability entry");
        for os in ["linux", "macos", "ios", "windows"] {
            assert!(
                availability.is_available_on_build_target(&build_target_for_os(os)),
                "some arm covers {os}: {:?}",
                availability.availability_cfg_predicate
            );
        }
    }

    /// Cargo features are a SET, not a single value: a target enables many at
    /// once, so two feature-gated arms really can both compile.
    #[test]
    fn two_feature_gated_arms_are_satisfiable_together() {
        assert!(
            find_build_target_satisfying_both_cfg_predicate_sets(
                &predicates(&[r#"feature = "cuda""#]),
                &predicates(&[r#"feature = "vulkan""#]),
            )
            .is_some()
        );
    }

    /// Two arms with no `#[cfg]` at all are satisfied by every target, which is
    /// the degenerate overlap: the empty predicate set holds vacuously.
    #[test]
    fn two_unconditional_arms_are_satisfiable_together() {
        assert!(
            find_build_target_satisfying_both_cfg_predicate_sets(&[], &[]).is_some(),
            "an unconditional arm compiles everywhere, so two of them always collide"
        );
    }

    /// Two arms one build target compiles both of are refused, naming both
    /// source files and a target that exhibits the overlap. Mentally revert the
    /// refusal and the package builds with two `export_plugin!` entries under
    /// one name, of which the registry keeps whichever it sees first.
    #[test]
    fn overlapping_arms_are_refused_with_a_witness_build_target() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/unix_arm.rs",
            r#"#![cfg(unix)]
            #[processor("@tatolab/demo/Capture", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct UnixCapture;"#,
        );
        write(
            root,
            "processors/linux_arm.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/Capture", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct LinuxCapture;"#,
        );

        let error = extract_processors_across_every_build_target(root).unwrap_err();
        let ExtractError::OverlappingProcessorDeclarations {
            processor_type_name,
            first_declared_in,
            second_declared_in,
            witness_build_target_atoms,
        } = &error
        else {
            panic!("expected OverlappingProcessorDeclarations, got {error:?}");
        };
        assert_eq!(processor_type_name, "Capture");
        let declared_in = [
            first_declared_in.to_string_lossy().to_string(),
            second_declared_in.to_string_lossy().to_string(),
        ];
        assert!(
            declared_in.contains(&"processors/unix_arm.rs".to_string())
                && declared_in.contains(&"processors/linux_arm.rs".to_string()),
            "{declared_in:?}"
        );
        assert!(
            witness_build_target_atoms.contains(r#"target_os = "linux""#),
            "{witness_build_target_atoms}"
        );
        assert!(error.to_string().contains("declared twice"), "{error}");
    }

    /// The assertion that protects `@tatolab/audio`: two arms gated on disjoint
    /// platform predicates are NOT an overlap, so the check refuses only on a
    /// proven collision rather than on any two arms sharing a name.
    #[test]
    fn disjoint_platform_arms_declaring_one_processor_are_accepted() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/linux/mod.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/AudioCapture", execution = manual, output("audio", "@tatolab/core/AudioFrame"))]
            pub struct LinuxAudioCapture;"#,
        );
        write(
            root,
            "processors/apple/mod.rs",
            r#"#![cfg(any(target_os = "macos", target_os = "ios"))]
            #[processor("@tatolab/demo/AudioCapture", execution = manual, output("audio", "@tatolab/core/AudioFrame"))]
            pub struct AppleAudioCapture;"#,
        );

        let set = extract_processors_across_every_build_target(root).unwrap();
        assert_eq!(set.processor_declarations.len(), 2);
        assert_eq!(set.processor_availability.len(), 1);
    }

    /// Availability is the disjunction over the declaring arms, answered
    /// through the one cfg evaluator — the datum that replaces "which targets
    /// is this on?" being prose in a description string.
    #[test]
    fn a_two_arm_processors_availability_covers_both_arms_targets() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/linux/mod.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/AudioCapture", execution = manual, output("audio", "@tatolab/core/AudioFrame"))]
            pub struct LinuxAudioCapture;"#,
        );
        write(
            root,
            "processors/apple/mod.rs",
            r#"#![cfg(any(target_os = "macos", target_os = "ios"))]
            #[processor("@tatolab/demo/AudioCapture", execution = manual, output("audio", "@tatolab/core/AudioFrame"))]
            pub struct AppleAudioCapture;"#,
        );

        let set = extract_processors_across_every_build_target(root).unwrap();
        let availability = set
            .availability_of_processor_type_name("AudioCapture")
            .expect("the two arms fold to one availability entry");
        assert_eq!(
            availability.declaring_arm_source_files,
            vec![
                PathBuf::from("processors/apple/mod.rs"),
                PathBuf::from("processors/linux/mod.rs"),
            ]
        );
        for os in ["linux", "macos", "ios"] {
            assert!(
                availability.is_available_on_build_target(&build_target_for_os(os)),
                "AudioCapture must be available on {os}: {:?}",
                availability.availability_cfg_predicate
            );
        }
        assert!(
            !availability.is_available_on_build_target(&build_target_for_os("windows")),
            "neither arm compiles on windows"
        );
    }

    /// An unconditional arm makes the processor unconditional: no predicate at
    /// all, available everywhere. An `any(...)` naming the other arms would
    /// understate where it exists.
    #[test]
    fn an_unconditional_arm_yields_unconditional_availability() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/always.rs",
            r#"#[processor("@tatolab/demo/Always", execution = reactive)]
            pub struct Always;"#,
        );
        let set = extract_processors_across_every_build_target(root).unwrap();
        let availability = set
            .availability_of_processor_type_name("Always")
            .expect("the unconditional arm has an availability entry");
        assert!(availability.availability_cfg_predicate.is_none());
        for os in ["linux", "macos", "ios", "windows"] {
            assert!(availability.is_available_on_build_target(&build_target_for_os(os)));
        }
    }

    /// A GAP is not an error: a package that declares a processor on some
    /// targets and none on others is legitimate (`@tatolab/clap` is Apple-only,
    /// `@tatolab/frame-tap` Linux-only). Availability reports the gap; nothing
    /// refuses it.
    #[test]
    fn a_processor_available_on_no_common_target_is_not_an_error() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/apple_only.rs",
            r#"#![cfg(any(target_os = "macos", target_os = "ios"))]
            #[processor("@tatolab/demo/AppleOnly", execution = reactive)]
            pub struct AppleOnly;"#,
        );
        let set = extract_processors_across_every_build_target(root).unwrap();
        let availability = set
            .availability_of_processor_type_name("AppleOnly")
            .unwrap();
        assert!(!availability.is_available_on_build_target(&build_target_for_os("linux")));
        assert!(availability.is_available_on_build_target(&build_target_for_os("macos")));
    }

    /// Two disjoint arms declaring one processor with different PORTS are
    /// refused: the derived `processors:` section would carry whichever port
    /// set the publishing host happened to compile. The diagnostic names the
    /// port and both schema types, not just "they differ".
    #[test]
    fn arms_declaring_one_processor_with_different_ports_are_refused() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/linux/mod.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/AudioCapture", execution = manual, output("audio", "@tatolab/core/AudioFrame"))]
            pub struct LinuxAudioCapture;"#,
        );
        write(
            root,
            "processors/apple/mod.rs",
            r#"#![cfg(any(target_os = "macos", target_os = "ios"))]
            #[processor("@tatolab/demo/AudioCapture", execution = manual, output("audio", "@tatolab/core/VideoFrame"))]
            pub struct AppleAudioCapture;"#,
        );

        let error = extract_processors_across_every_build_target(root).unwrap_err();
        let ExtractError::DivergentProcessorDeclarations {
            processor_type_name,
            difference,
            ..
        } = &error
        else {
            panic!("expected DivergentProcessorDeclarations, got {error:?}");
        };
        assert_eq!(processor_type_name, "AudioCapture");
        assert!(difference.contains("output port `audio`"), "{difference}");
        assert!(difference.contains("AudioFrame"), "{difference}");
        assert!(difference.contains("VideoFrame"), "{difference}");
        assert!(
            difference.contains("processors/apple/mod.rs")
                && difference.contains("processors/linux/mod.rs"),
            "{difference}"
        );
    }

    /// The same refusal for a differing execution mode.
    #[test]
    fn arms_declaring_one_processor_with_different_execution_are_refused() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/linux/mod.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/AudioCapture", execution = manual)]
            pub struct LinuxAudioCapture;"#,
        );
        write(
            root,
            "processors/apple/mod.rs",
            r#"#![cfg(any(target_os = "macos", target_os = "ios"))]
            #[processor("@tatolab/demo/AudioCapture", execution = reactive)]
            pub struct AppleAudioCapture;"#,
        );

        let error = extract_processors_across_every_build_target(root).unwrap_err();
        let ExtractError::DivergentProcessorDeclarations { difference, .. } = &error else {
            panic!("expected DivergentProcessorDeclarations, got {error:?}");
        };
        assert!(difference.contains("execution differs"), "{difference}");
    }

    /// Two arms sharing a `Type` under DIFFERENT `@org/package` are a
    /// divergence, not two unrelated processors: the derived `processors:`
    /// entry drops the `@org/package` the attribute named, and the runtime
    /// composes the ident from the package's own — so the two fold into one
    /// entry either way. Mentally drop the `SchemaIdent` off
    /// `ExtractedProcessor` and this passes silently.
    #[test]
    fn arms_sharing_a_type_under_different_packages_are_refused() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/linux/mod.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/AudioCapture", execution = manual)]
            pub struct LinuxAudioCapture;"#,
        );
        write(
            root,
            "processors/apple/mod.rs",
            r#"#![cfg(any(target_os = "macos", target_os = "ios"))]
            #[processor("@tatolab/other/AudioCapture", execution = manual)]
            pub struct AppleAudioCapture;"#,
        );

        let error = extract_processors_across_every_build_target(root).unwrap_err();
        let ExtractError::DivergentProcessorDeclarations { difference, .. } = &error else {
            panic!("expected DivergentProcessorDeclarations, got {error:?}");
        };
        assert!(difference.contains("identity differs"), "{difference}");
        assert!(
            difference.contains("@tatolab/demo/AudioCapture")
                && difference.contains("@tatolab/other/AudioCapture"),
            "{difference}"
        );
    }

    /// The drift surface excludes `description`, but the derived manifest entry
    /// carries it — so two arms whose descriptions disagree still ship a
    /// host-dependent `processors:`. The whole-projection comparison is what
    /// catches it; the lossy identity surface alone would not.
    #[test]
    fn arms_declaring_one_processor_with_different_descriptions_are_refused() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/linux/mod.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/AudioCapture", description = "ALSA capture", execution = manual)]
            pub struct LinuxAudioCapture;"#,
        );
        write(
            root,
            "processors/apple/mod.rs",
            r#"#![cfg(any(target_os = "macos", target_os = "ios"))]
            #[processor("@tatolab/demo/AudioCapture", description = "CoreAudio capture", execution = manual)]
            pub struct AppleAudioCapture;"#,
        );

        let error = extract_processors_across_every_build_target(root).unwrap_err();
        let ExtractError::DivergentProcessorDeclarations { difference, .. } = &error else {
            panic!("expected DivergentProcessorDeclarations, got {error:?}");
        };
        assert!(difference.contains("`description`"), "{difference}");
    }

    /// The reasoning-free net: a target-resolved scan that collects one `Type`
    /// twice has PROVEN the overlap by compiling both, with no satisfiability
    /// argument at all. This is what still fires when a predicate pair the atom
    /// model cannot decide is compiled by a real host.
    #[test]
    fn one_build_target_collecting_a_processor_twice_is_refused() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/unix_arm.rs",
            r#"#![cfg(unix)]
            #[processor("@tatolab/demo/Capture", execution = manual)]
            pub struct UnixCapture;"#,
        );
        write(
            root,
            "processors/linux_arm.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/Capture", execution = manual)]
            pub struct LinuxCapture;"#,
        );

        let error = extract_reachable_rust_processors(root, &linux()).unwrap_err();
        let ExtractError::OverlappingProcessorDeclarations {
            processor_type_name,
            witness_build_target_atoms,
            ..
        } = &error
        else {
            panic!("expected OverlappingProcessorDeclarations, got {error:?}");
        };
        assert_eq!(processor_type_name, "Capture");
        assert!(
            witness_build_target_atoms.contains(r#"target_os = "linux""#),
            "{witness_build_target_atoms}"
        );

        // The same package on macOS compiles only the `unix` arm — one
        // declaration, no refusal. The overlap is a property of the target, and
        // the message names the target it was proven on.
        assert_eq!(
            names(extract_reachable_rust_processors(root, &macos()).unwrap()),
            vec!["Capture"]
        );
    }

    /// The diagnostic itself, observed: a feature-gated processor the scan
    /// target cannot satisfy is named at the prune site, both when a file-level
    /// `#![cfg]` prunes it and when the `mod` that would have reached it is
    /// pruned a file above. Delete either call site and this fails.
    #[test]
    fn a_processor_pruned_by_an_undefined_feature_is_warned_about() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/cuda_arm.rs",
            r#"#![cfg(feature = "cuda")]
            #[processor("@tatolab/demo/CudaCopy", execution = manual)]
            pub struct CudaCopy;"#,
        );
        write(
            root,
            "processors/vulkan_arm/mod.rs",
            r#"#[cfg(feature = "vulkan")]
            pub mod blit;"#,
        );
        write(
            root,
            "processors/vulkan_arm/blit.rs",
            r#"#[processor("@tatolab/demo/VulkanBlit", execution = manual)]
            pub struct VulkanBlit;"#,
        );

        let warnings = warnings_emitted_by(|| {
            let procs = extract_reachable_rust_processors(root, &linux()).unwrap();
            assert!(names(procs).is_empty(), "neither feature is defined");
        });
        assert!(warnings.contains("processors/cuda_arm.rs"), "{warnings}");
        assert!(warnings.contains("undefined_features=cuda"), "{warnings}");
        assert!(
            warnings.contains("processors/vulkan_arm/mod.rs"),
            "{warnings}"
        );
        assert!(warnings.contains("undefined_features=vulkan"), "{warnings}");

        // With the feature declared on the scan target nothing is pruned, so
        // there is nothing to explain.
        let quiet = warnings_emitted_by(|| {
            let target = linux().with_feature("cuda").with_feature("vulkan");
            let procs = extract_reachable_rust_processors(root, &target).unwrap();
            assert_eq!(names(procs), vec!["CudaCopy", "VulkanBlit"]);
        });
        assert!(quiet.is_empty(), "{quiet}");
    }

    /// A feature-gated scope under `processors/` that declares NO processor is
    /// pruned silently: nothing left the derived set, so the warning's whole
    /// subject is absent and emitting it would be noise on every scan.
    #[test]
    fn a_feature_gated_scope_declaring_no_processor_is_pruned_silently() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/arm/mod.rs",
            r#"#[cfg(feature = "cuda")]
            pub mod cuda_helpers;

            #[processor("@tatolab/demo/Copy", execution = manual)]
            pub struct Copy;"#,
        );
        write(
            root,
            "processors/arm/cuda_helpers.rs",
            "pub fn align_pitch(width: usize) -> usize { width }",
        );
        write(
            root,
            "processors/cuda_only_helpers.rs",
            r#"#![cfg(feature = "cuda")]
            pub fn device_count() -> usize { 0 }"#,
        );

        let warnings = warnings_emitted_by(|| {
            let procs = extract_reachable_rust_processors(root, &linux()).unwrap();
            assert_eq!(names(procs), vec!["Copy"]);
        });
        assert!(warnings.is_empty(), "{warnings}");
    }

    /// A feature-gated scope the scan target cannot satisfy is named rather
    /// than silently dropped: `for_host()` infers no cargo features, so the
    /// processor leaves the derived set and the publish-time drift gate reports
    /// the committed entry as "no longer declared in code". The atoms the
    /// warning names come from here.
    #[test]
    fn undefined_feature_atoms_in_a_pruning_predicate_are_named() {
        let collect = |source: &str, target: &ModuleReachabilityTarget| {
            let meta = syn::parse_str::<Meta>(source).unwrap();
            let mut out = BTreeSet::new();
            collect_undefined_feature_atoms(&meta, target, &mut out);
            out.into_iter().collect::<Vec<_>>()
        };

        assert_eq!(
            collect(r#"all(unix, feature = "cuda")"#, &linux()),
            vec!["cuda".to_string()]
        );
        assert_eq!(
            collect(r#"any(feature = "cuda", feature = "vulkan")"#, &linux()),
            vec!["cuda".to_string(), "vulkan".to_string()]
        );
        // A feature the target DOES define is not the reason the scope pruned.
        assert!(collect(r#"feature = "cuda""#, &linux().with_feature("cuda")).is_empty());
        // Nothing feature-shaped pruned a platform-only predicate.
        assert!(collect(r#"target_os = "windows""#, &linux()).is_empty());
        // `not(feature = "x")` is satisfied BY absence, so a scope it prunes
        // was pruned by the feature being PRESENT — not the confusing case.
        assert!(collect(r#"not(feature = "cuda")"#, &linux()).is_empty());
    }
}
