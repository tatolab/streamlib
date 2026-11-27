// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a node written as one function runs on once its body has run: the scope its body
//! declared, the view its frames read and the per-frame function its body returned.

use streamlib_scope::{FrameView, Scope, ScopeBeingDeclared, Writes};

use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::error::{Error, Result};

/// How long a frame waits when a computed value has no first value yet.
const UNBUILT_FRAME_BACKOFF: std::time::Duration = std::time::Duration::from_millis(1);

/// The per-frame function a node's body returns.
pub type PerFrameFunction =
    Box<dyn for<'ctx> FnMut(&RuntimeContextLimitedAccess<'ctx>) -> Result<()> + Send>;

/// A node written as one function, its body run.
///
/// Its fields drop in order: the per-frame function and what it captured, then the view's
/// values, then the scope, whose drop waits out a build in flight and starts no other.
pub struct NodeFunctionRunning {
    per_frame: PerFrameFunction,
    view: FrameView,
    scope: Scope,
}

impl NodeFunctionRunning {
    /// Run `body`, the node `node`'s body, once, with every computed value it declares declared
    /// into `declaring`, then start the scope, its computed values' builds shown in the health
    /// `ctx`'s node reports: refused with the body's own error, or when the thread its computed
    /// values build on does not start.
    pub fn run_the_body_once(
        node: &str,
        ctx: &RuntimeContextFullAccess<'_>,
        mut declaring: ScopeBeingDeclared,
        body: impl FnOnce() -> Result<PerFrameFunction>,
    ) -> Result<Self> {
        let per_frame = declaring.declare(body)?;
        let (scope, view) =
            declaring
                .start()
                .map_err(|why| Error::ComputedValueBuildThreadNotStarted {
                    node: node.to_owned(),
                    why,
                })?;
        ctx.host_base()
            .report_the_builds_of_the_computed_values_from(scope.builds());
        Ok(Self {
            per_frame,
            view,
            scope,
        })
    }

    /// Take the values built or written since the last frame, then run the per-frame function
    /// once every computed value has its first value. Until then, run nothing and wait
    /// `UNBUILT_FRAME_BACKOFF`: a reactive node with a message waiting is called again at once,
    /// and would otherwise spin its thread for as long as its first build runs.
    pub fn run_a_frame(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let Self {
            per_frame, view, ..
        } = self;
        view.run_frame(|| per_frame(ctx)).unwrap_or_else(|| {
            std::thread::sleep(UNBUILT_FRAME_BACKOFF);
            Ok(())
        })
    }

    /// Write the inputs `writes` names to the scope, all as one write.
    pub fn write(&self, writes: impl FnOnce(&mut Writes)) {
        self.scope.write(writes);
    }
}

/// A value a value input's `default = …` may state for an input holding a `T`: a `T` itself, or
/// a `&str` for an input holding a `String`.
pub trait TheDefaultOfAValueInput<T> {
    fn into_the_default(self) -> T;
}

impl<T> TheDefaultOfAValueInput<T> for T {
    fn into_the_default(self) -> T {
        self
    }
}

impl TheDefaultOfAValueInput<String> for &str {
    fn into_the_default(self) -> String {
        self.to_owned()
    }
}
