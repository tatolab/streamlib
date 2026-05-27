// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI gate banning the escalate-from-lifecycle anti-pattern.
//!
//! Inside a processor's `setup` / `teardown` body (or any helper
//! method called from one — `setup_inner`, `teardown_inner`, etc.,
//! detected via the `&RuntimeContextFullAccess<'_>` parameter), a
//! call to `.escalate(...)` on `gpu_limited_access()` (or any
//! equivalent re-entry into the escalate gate) deadlocks pre-#912
//! and post-#1072 panics with a same-thread re-entry message
//! (see `EscalateGate::enter`). The historical sandbox contract
//! gives setup/teardown direct privileged access — call
//! `ctx.gpu_full_access()` directly instead.
//!
//! This check is defense-in-depth on top of the runtime panic:
//! the runtime panic catches the violation when the code runs, the
//! xtask catches it at PR-review time before the gate-detection
//! panic ever fires.
//!
//! Targets:
//! - `packages/**/*.rs` (every workspace package)
//! - `examples/**/*.rs` (in-tree example consumers)
//!
//! Method bodies scanned:
//! - Any `fn` whose **name** is `setup`, `teardown`, `setup_inner`,
//!   or `teardown_inner` AND takes `&RuntimeContextFullAccess` in
//!   its parameter list. The name-match scopes the lint to the
//!   actual engine-side gate-wrap surface — `ProcessorInstance`
//!   wraps `setup` / `teardown` dispatch, not `start` / `stop`
//!   (Manual-mode processors take `RuntimeContextFullAccess` in
//!   `start` but the engine does not hold the gate around it, so
//!   `.escalate(...)` there is the legitimate Pattern 4 use case,
//!   not the anti-pattern).
//! - The `_inner` suffix variants cover delegation helpers (see the
//!   `BlendingCompositor` / `CrtFilmGrain` / `CameraToCudaCopy`
//!   shape) — a `fn setup` that immediately calls `self.setup_inner(ctx)`
//!   leaks the dispatched body into the helper, which inherits the
//!   gate-held semantic.
//!
//! Calls flagged inside those bodies:
//! - `<expr>.escalate(...)` — any method call named `escalate` on
//!   any receiver. The receiver is almost always a
//!   `GpuContextLimitedAccess`, but the check is name-based so a
//!   future renamed escalate variant still trips it.
//!
//! See `docs/architecture/cdylib-reachability.md` (anti-pattern #1)
//! for the rationale and the right pattern.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;
use walkdir::WalkDir;

/// Directories the check walks. Every `.rs` file under these trees
/// is scanned.
const TARGET_DIRS: &[&str] = &["packages", "examples"];

/// Substring the function-parameter type-path must contain for the
/// function body to be scanned. Matches both
/// `&RuntimeContextFullAccess<'_>` and the fully-qualified
/// `&streamlib::sdk::context::RuntimeContextFullAccess<'_>` form.
const LIFECYCLE_PARAM_MARKER: &str = "RuntimeContextFullAccess";

/// Function names whose bodies are subject to the escalate ban
/// when they also take `&RuntimeContextFullAccess`. Scoped to the
/// engine-side gate-wrap surface (see
/// `ProcessorInstance::setup` / `teardown`'s dispatch shape).
const LIFECYCLE_FN_NAMES: &[&str] =
    &["setup", "teardown", "setup_inner", "teardown_inner"];

/// Method name flagged inside lifecycle bodies.
const BANNED_METHOD: &str = "escalate";

#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    pub file: PathBuf,
    pub function: String,
    pub line: usize,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let violations = scan_workspace(workspace_root)?;
    if violations.is_empty() {
        println!(
            "✓ check-no-escalate-in-lifecycle: no `.escalate(...)` calls inside \
             lifecycle bodies (setup / teardown / setup_inner / teardown_inner). \
             Sandbox contract intact."
        );
        return Ok(());
    }

    eprintln!(
        "✗ check-no-escalate-in-lifecycle: {} violation(s) — \
         `.escalate(...)` called inside a function taking \
         `&RuntimeContextFullAccess` (setup / teardown / helper). \
         The lifecycle dispatch already holds the escalate gate; \
         re-entering panics at runtime.",
        violations.len()
    );
    for v in &violations {
        eprintln!(
            "  {}:{}: fn {} reaches `.escalate(...)` — see \
             docs/architecture/cdylib-reachability.md anti-pattern #1",
            v.file.display(),
            v.line,
            v.function,
        );
    }
    eprintln!(
        "\nFix:\n  \
         Use `ctx.gpu_full_access()` directly. setup() / teardown() bodies\n  \
         are dispatched inside an engine-managed scope that already grants\n  \
         FullAccess (cdylib-resident: ScopeToken via `with_cdylib_scope`;\n  \
         in-process: Boxed via the gpu_limited_access().escalate(|_| ...)\n  \
         wrap inside ProcessorInstance::setup). Calling `.escalate(...)`\n  \
         again from your body re-enters the same gate on the same thread\n  \
         and trips the gate's same-thread re-entry panic in\n  \
         libs/streamlib-engine/src/core/context/escalate_gate.rs.\n  \
         \n  \
         See docs/architecture/cdylib-reachability.md, anti-pattern #1.\n  \
         The historical sandbox contract — pre-#322 setup() got\n  \
         `RuntimeContext` by value (full access); post-#322 it gets\n  \
         `&RuntimeContextFullAccess` whose `gpu_full_access()` is\n  \
         already privileged — never permitted escalate-from-setup."
    );
    anyhow::bail!("check-no-escalate-in-lifecycle failed");
}

pub fn scan_workspace(workspace_root: &Path) -> Result<Vec<Violation>> {
    let mut all = Vec::new();
    for dir in TARGET_DIRS {
        let target = workspace_root.join(dir);
        if !target.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&target).into_iter().filter_map(|e| e.ok()) {
            let abs = entry.path();
            if !entry.file_type().is_file() {
                continue;
            }
            if abs.extension().map(|e| e != "rs").unwrap_or(true) {
                continue;
            }
            let relpath = abs
                .strip_prefix(workspace_root)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| abs.to_path_buf());
            let src = fs::read_to_string(abs)
                .with_context(|| format!("reading {}", abs.display()))?;
            let file = match syn::parse_file(&src) {
                Ok(f) => f,
                // Skip files that don't parse (e.g., generated code
                // fragments that aren't standalone). The xtask is
                // best-effort; the runtime panic is the backstop.
                Err(_) => continue,
            };
            let mut visitor = FileVisitor {
                file_path: relpath,
                violations: &mut all,
            };
            visitor.visit_file(&file);
        }
    }
    Ok(all)
}

/// `true` when any argument's type contains a path segment whose
/// identifier is [`LIFECYCLE_PARAM_MARKER`]
/// (`RuntimeContextFullAccess`). Catches both bare
/// `&RuntimeContextFullAccess<'_>` and fully-qualified forms
/// (`streamlib::sdk::context::RuntimeContextFullAccess`,
/// `crate::core::context::RuntimeContextFullAccess`, etc.).
fn takes_lifecycle_full_access(sig: &syn::Signature) -> bool {
    sig.inputs.iter().any(|arg| match arg {
        syn::FnArg::Typed(pat_type) => type_mentions(&pat_type.ty, LIFECYCLE_PARAM_MARKER),
        syn::FnArg::Receiver(_) => false,
    })
}

/// Recursively walks a `syn::Type` and returns `true` when any path
/// segment ident matches `marker`. Handles `&T`, `&mut T`, generic
/// params, and tuple types.
fn type_mentions(ty: &syn::Type, marker: &str) -> bool {
    match ty {
        syn::Type::Reference(r) => type_mentions(&r.elem, marker),
        syn::Type::Path(tp) => tp.path.segments.iter().any(|seg| {
            if seg.ident == marker {
                return true;
            }
            // Walk generic args (e.g. `RuntimeContextFullAccess<'a>`
            // — the lifetime arg is just `'a`, but a future
            // generics-bearing variant would surface here).
            if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                args.args.iter().any(|arg| match arg {
                    syn::GenericArgument::Type(inner) => type_mentions(inner, marker),
                    _ => false,
                })
            } else {
                false
            }
        }),
        syn::Type::Tuple(tup) => tup.elems.iter().any(|t| type_mentions(t, marker)),
        syn::Type::Group(g) => type_mentions(&g.elem, marker),
        syn::Type::Paren(p) => type_mentions(&p.elem, marker),
        _ => false,
    }
}

struct FileVisitor<'a> {
    file_path: PathBuf,
    violations: &'a mut Vec<Violation>,
}

impl<'ast, 'a> Visit<'ast> for FileVisitor<'a> {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        let fn_name = item.sig.ident.to_string();
        if LIFECYCLE_FN_NAMES.contains(&fn_name.as_str())
            && takes_lifecycle_full_access(&item.sig)
        {
            scan_for_escalate(&item.block, &fn_name, &self.file_path, self.violations);
        }
        syn::visit::visit_item_fn(self, item);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        let fn_name = item.sig.ident.to_string();
        if LIFECYCLE_FN_NAMES.contains(&fn_name.as_str())
            && takes_lifecycle_full_access(&item.sig)
        {
            scan_for_escalate(&item.block, &fn_name, &self.file_path, self.violations);
        }
        syn::visit::visit_impl_item_fn(self, item);
    }
}

fn scan_for_escalate(
    block: &syn::Block,
    fn_name: &str,
    file_path: &Path,
    violations: &mut Vec<Violation>,
) {
    let mut scanner = EscalateScanner {
        file_path: file_path.to_path_buf(),
        function: fn_name.to_string(),
        violations,
    };
    scanner.visit_block(block);
}

struct EscalateScanner<'a> {
    file_path: PathBuf,
    function: String,
    violations: &'a mut Vec<Violation>,
}

impl<'ast, 'a> Visit<'ast> for EscalateScanner<'a> {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if call.method == BANNED_METHOD {
            // Best-effort line — the proc-macro2 `span-locations`
            // feature gives us start-line on the ident's span when
            // the source was parsed from text (which the workspace
            // scan always does).
            let line = call.method.span().start().line;
            self.violations.push(Violation {
                file: self.file_path.clone(),
                function: self.function.clone(),
                line,
            });
        }
        syn::visit::visit_expr_method_call(self, call);
    }
}

#[cfg(test)]
mod tests {
    //! The tests exercise the syn-based AST scanner in-memory — no
    //! filesystem reach, so the workspace's actual lifecycle code
    //! doesn't influence the test outcomes.

    use super::*;

    fn scan_source(src: &str) -> Vec<Violation> {
        let file = syn::parse_file(src).expect("source parses");
        let mut violations = Vec::new();
        let mut visitor = FileVisitor {
            file_path: PathBuf::from("<test>"),
            violations: &mut violations,
        };
        visitor.visit_file(&file);
        violations
    }

    #[test]
    fn flags_escalate_inside_setup_taking_full_access() {
        let src = r#"
            impl Proc {
                fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
                    let lim = ctx.gpu_limited_access().clone();
                    let _ = lim.escalate(|full| Ok::<(), ()>(()));
                    Ok(())
                }
            }
        "#;
        let v = scan_source(src);
        assert_eq!(v.len(), 1, "expected 1 violation, got: {v:?}");
        assert_eq!(v[0].function, "setup");
    }

    #[test]
    fn flags_escalate_inside_setup_inner_helper() {
        let src = r#"
            impl Proc {
                fn setup_inner(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
                    let _ = ctx.gpu_limited_access().clone().escalate(|full| Ok::<(), ()>(()));
                    Ok(())
                }
            }
        "#;
        let v = scan_source(src);
        assert_eq!(v.len(), 1, "expected 1 violation, got: {v:?}");
        assert_eq!(v[0].function, "setup_inner");
    }

    #[test]
    fn allows_escalate_in_non_lifecycle_fn() {
        let src = r#"
            impl Proc {
                fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
                    let _ = ctx.gpu_limited_access().clone().escalate(|full| Ok::<(), ()>(()));
                    Ok(())
                }
            }
        "#;
        // `process` takes LimitedAccess, not FullAccess — escalate is
        // the right pattern (Pattern 4) and must not be flagged.
        assert!(scan_source(src).is_empty());
    }

    #[test]
    fn allows_setup_that_does_not_call_escalate() {
        let src = r#"
            impl Proc {
                fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
                    let _ = ctx.gpu_full_access().host_vulkan_device_arc()?;
                    Ok(())
                }
            }
        "#;
        assert!(scan_source(src).is_empty());
    }

    #[test]
    fn flags_escalate_in_teardown() {
        let src = r#"
            impl Proc {
                fn teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
                    let _ = ctx.gpu_limited_access().clone().escalate(|_| Ok::<(), ()>(()));
                    Ok(())
                }
            }
        "#;
        let v = scan_source(src);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].function, "teardown");
    }

    #[test]
    fn recognizes_fully_qualified_path_in_param_type() {
        let src = r#"
            impl Proc {
                fn setup(
                    &mut self,
                    ctx: &streamlib::sdk::context::RuntimeContextFullAccess<'_>,
                ) -> Result<()> {
                    let _ = ctx.gpu_limited_access().clone().escalate(|_| Ok::<(), ()>(()));
                    Ok(())
                }
            }
        "#;
        let v = scan_source(src);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn allows_helper_named_setup_that_takes_no_lifecycle_ctx() {
        // A free-floating helper named `setup` that takes no
        // RuntimeContextFullAccess parameter must not be flagged —
        // the check requires BOTH name match and parameter match.
        let src = r#"
            fn setup(config: &Config) -> Result<()> {
                let sandbox = make_sandbox();
                let _ = sandbox.escalate(|_| Ok::<(), ()>(()));
                Ok(())
            }
        "#;
        assert!(scan_source(src).is_empty());
    }

    #[test]
    fn allows_escalate_in_start_taking_full_access() {
        // Manual-mode `start()` takes `&RuntimeContextFullAccess`
        // but the engine does NOT hold the escalate gate around
        // it (gate-wrap lives in ProcessorInstance::setup /
        // teardown only). `.escalate(...)` from start() is the
        // legitimate Pattern 4 use case, not the anti-pattern.
        let src = r#"
            impl Proc {
                fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
                    let _ = ctx.gpu_limited_access().clone().escalate(|_| Ok::<(), ()>(()));
                    Ok(())
                }
            }
        "#;
        assert!(scan_source(src).is_empty());
    }

    #[test]
    fn allows_escalate_in_stop_taking_full_access() {
        let src = r#"
            impl Proc {
                fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
                    let _ = ctx.gpu_limited_access().clone().escalate(|_| Ok::<(), ()>(()));
                    Ok(())
                }
            }
        "#;
        assert!(scan_source(src).is_empty());
    }

    #[test]
    fn allows_escalate_in_arbitrarily_named_helper_with_full_access() {
        // A helper called only from start() — not subject to the
        // gate-wrap. Same body inside a fn named setup_inner
        // would fire.
        let src = r#"
            impl Proc {
                fn run_my_smoke(&self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
                    let _ = ctx.gpu_limited_access().clone().escalate(|_| Ok::<(), ()>(()));
                    Ok(())
                }
            }
        "#;
        assert!(scan_source(src).is_empty());
    }
}
