// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a node written as one function runs on once its body has run: the scope its body
//! declared, the view its frames read and the per-frame function its body returned, which a
//! node that paces itself (`execution = manual`) has none of: its computed values publish on
//! their own, and a value set on it reaches its scope at once.

use std::cell::RefCell;
use std::fmt;

use streamlib_scope::{Computed, FrameView, Scope, ScopeBeingDeclared, Writes};

use crate::core::context::{RuntimeContext, RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::error::{Error, Result};
use crate::core::graph::BuildsOfItsComputedValues;

/// How long a frame waits when a computed value has no first value yet.
const UNBUILT_FRAME_BACKOFF: std::time::Duration = std::time::Duration::from_millis(1);

thread_local! {
    /// The context of the node whose body runs on this thread, which each value the body
    /// declares builds with.
    static CONTEXT_OF_THE_BODY_RUNNING: RefCell<Option<RuntimeContext>> =
        const { RefCell::new(None) };
}

/// Declare a value `build` computes from what it reads with `.get()`, in the body of a node
/// written as one function. `build` runs off the frame thread with the node's limited context,
/// whose GPU escalates for the work a frame may not do: creating a kernel, opening a device.
/// Outside such a body: refused with the scope's own refusal where no scope is being declared,
/// and in a scope no node's body declares, a value whose every build fails for want of a node's
/// context.
#[track_caller]
pub fn computed<T, E, B>(build: B) -> Computed<T>
where
    T: PartialEq + Send + Sync + 'static,
    E: fmt::Display,
    B: for<'ctx> Fn(&RuntimeContextLimitedAccess<'ctx>) -> std::result::Result<T, E>
        + Send
        + Sync
        + 'static,
{
    let Some(context_of_the_node) =
        CONTEXT_OF_THE_BODY_RUNNING.with(|running| running.borrow().clone())
    else {
        return streamlib_scope::computed(|| Err(BUILT_WITH_NO_NODE_CONTEXT));
    };
    streamlib_scope::computed(move || {
        build(&RuntimeContextLimitedAccess::new(&context_of_the_node))
    })
}

/// Why a value declared in a scope no node's body declares never builds: it has no node's
/// context to build with.
const BUILT_WITH_NO_NODE_CONTEXT: &str =
    "this value was declared outside a node's body, so it has no node's context to build with";

/// The context of the node whose body runs on this thread, for as long as the body runs.
struct BodyRunningWithTheContextOf;

impl BodyRunningWithTheContextOf {
    fn the_node(ctx: &RuntimeContextFullAccess<'_>) -> Self {
        let context_of_the_node = ctx.host_base().clone();
        CONTEXT_OF_THE_BODY_RUNNING
            .with(|running| *running.borrow_mut() = Some(context_of_the_node));
        Self
    }
}

impl Drop for BodyRunningWithTheContextOf {
    fn drop(&mut self) {
        CONTEXT_OF_THE_BODY_RUNNING.with(|running| *running.borrow_mut() = None);
    }
}

/// The per-frame function a node's body returns.
pub type PerFrameFunction =
    Box<dyn for<'ctx> FnMut(&RuntimeContextLimitedAccess<'ctx>) -> Result<()> + Send>;

/// A node written as one function, its body run.
///
/// Its fields drop in order: the per-frame function and what it captured, then the view's
/// values, then the scope, whose drop waits out a build in flight and starts no other.
pub struct NodeFunctionRunning {
    /// `None` for a node that paces itself, whose computed values publish on their own.
    per_frame: Option<PerFrameFunction>,
    view: FrameView,
    scope: Scope,
}

impl NodeFunctionRunning {
    /// Run `body`, the node `node`'s body, once, with every computed value it declares declared
    /// into `declaring`, then start the scope, its computed values' builds shown in the health
    /// `ctx`'s node reports: refused with the body's own error, or when the thread its computed
    /// values build on does not start. The body returns the per-frame function, or `None` for a
    /// node that paces itself. Each value the body declares with [`computed`] builds with `ctx`'s
    /// limited access.
    pub fn run_the_body_once(
        node: &str,
        ctx: &RuntimeContextFullAccess<'_>,
        mut declaring: ScopeBeingDeclared,
        body: impl FnOnce() -> Result<Option<PerFrameFunction>>,
    ) -> Result<Self> {
        let per_frame = {
            let _body_running = BodyRunningWithTheContextOf::the_node(ctx);
            declaring.declare(body)?
        };
        let (scope, view) =
            declaring
                .start()
                .map_err(|why| Error::ComputedValueBuildThreadNotStarted {
                    node: node.to_owned(),
                    why,
                })?;
        ctx.host_base()
            .report_the_builds_of_the_computed_values_from(BuildsOfItsComputedValues::OfItsScope(
                scope.builds(),
            ));
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
        // A node that paces itself has no per-frame function, and its runner sends it no frame.
        let Some(per_frame) = per_frame else {
            return Ok(());
        };
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
