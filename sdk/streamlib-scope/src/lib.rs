// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Spike, not for merge. A node's scope: its inputs, written by a set or a wire alike, and the
//! values derived from them, built lazily on a worker thread off the frame thread. The frame
//! thread reads both through a [`FrameView`] that changes only when [`FrameView::flush`] runs at a
//! frame boundary, never blocks, and keeps the previous derived value while a newer one builds.
//!
//! Two roles only: an input and a derived value. A device or a session is a derived value; its
//! release is the `Drop` of the value it replaced, run on the worker thread.
//!
//! Propagation follows Salsa's revisions with backdating: each slot records the revision it last
//! changed at, a derived value records the slots its build read and the revisions it saw, and it
//! is built again only when one of those changed. A build whose result equals the value it
//! replaces keeps the old revision, so nothing downstream of it is built again. The newest inputs
//! win, as in Solid 2: a build whose inputs changed while it ran is thrown away and built again
//! from the newest values, so every write in between is coalesced.

use std::any::Any;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

type AnyValue = Arc<dyn Any + Send + Sync>;
type BuildOfAnyValue = Arc<dyn Fn(&Reads) -> Result<AnyValue, String> + Send + Sync>;
type EqualityOfAnyValues = fn(&dyn Any, &dyn Any) -> bool;

/// An input of a scope holding a `T`.
pub struct Input<T> {
    slot: usize,
    of: PhantomData<fn() -> T>,
}

/// A value derived from a scope's inputs and earlier derived values, holding a `T`.
pub struct Derived<T> {
    slot: usize,
    of: PhantomData<fn() -> T>,
}

impl<T> Clone for Input<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Input<T> {}
impl<T> Clone for Derived<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Derived<T> {}

/// What one build of a derived value read, recorded as it reads.
pub struct Reads {
    values: Vec<Option<AnyValue>>,
    changed_at: Vec<u64>,
    read: RefCell<Vec<usize>>,
}

impl Reads {
    /// The value input `input` held when the build started, recorded as read.
    pub fn input<T: Send + Sync + 'static>(&self, input: Input<T>) -> Arc<T> {
        self.read.borrow_mut().push(input.slot);
        let value = self.values[input.slot]
            .clone()
            .expect("an input always holds a value");
        value.downcast::<T>().expect("the input's own type")
    }

    /// The value `derived` held when the build started, recorded as read; none before its first
    /// build.
    pub fn derived<T: Send + Sync + 'static>(&self, derived: Derived<T>) -> Option<Arc<T>> {
        self.read.borrow_mut().push(derived.slot);
        self.values[derived.slot]
            .clone()
            .map(|value| value.downcast::<T>().expect("the derived value's own type"))
    }
}

/// How the last build of a derived value went, for a node's diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LastBuild {
    NotYetBuilt,
    Built { builds: u64 },
    Failed { builds: u64, why: String },
}

enum SlotRole {
    Input,
    Derived {
        build: BuildOfAnyValue,
        read_at: Vec<(usize, u64)>,
        last_build: LastBuild,
    },
}

struct Slot {
    role: SlotRole,
    value: Option<AnyValue>,
    changed_at: u64,
    equal: EqualityOfAnyValues,
}

struct ScopeSlots {
    revision: u64,
    slots: Vec<Slot>,
}

struct Shared {
    slots: Mutex<ScopeSlots>,
    something_to_build: Condvar,
    handed_over: Mutex<Vec<(usize, AnyValue)>>,
    replaced: Mutex<Vec<AnyValue>>,
    closing: AtomicBool,
}

fn equal_as<T: PartialEq + 'static>(left: &dyn Any, right: &dyn Any) -> bool {
    match (left.downcast_ref::<T>(), right.downcast_ref::<T>()) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// A node's scope while it is being declared: inputs and derived values, in that order.
#[derive(Default)]
pub struct ScopeBeingDeclared {
    slots: Vec<Slot>,
}

impl ScopeBeingDeclared {
    pub fn new() -> Self {
        Self::default()
    }

    /// An input holding `value` until it is written.
    pub fn input<T: PartialEq + Send + Sync + 'static>(&mut self, value: T) -> Input<T> {
        self.slots.push(Slot {
            role: SlotRole::Input,
            value: Some(Arc::new(value)),
            changed_at: 0,
            equal: equal_as::<T>,
        });
        Input {
            slot: self.slots.len() - 1,
            of: PhantomData,
        }
    }

    /// A value `build` derives from what it reads. It may read only inputs and derived values
    /// declared before it, so the declaration order is an order to build in.
    pub fn derived<T, B>(&mut self, build: B) -> Derived<T>
    where
        T: PartialEq + Send + Sync + 'static,
        B: Fn(&Reads) -> Result<T, String> + Send + Sync + 'static,
    {
        let build: BuildOfAnyValue =
            Arc::new(move |reads| build(reads).map(|value| Arc::new(value) as AnyValue));
        self.slots.push(Slot {
            role: SlotRole::Derived {
                build,
                read_at: Vec::new(),
                last_build: LastBuild::NotYetBuilt,
            },
            value: None,
            changed_at: 0,
            equal: equal_as::<T>,
        });
        Derived {
            slot: self.slots.len() - 1,
            of: PhantomData,
        }
    }

    /// Start the scope's worker, and return the scope and the one view its frame thread reads.
    pub fn start(self) -> (Scope, FrameView) {
        let initial: Vec<Option<AnyValue>> =
            self.slots.iter().map(|slot| slot.value.clone()).collect();
        let shared = Arc::new(Shared {
            slots: Mutex::new(ScopeSlots {
                revision: 0,
                slots: self.slots,
            }),
            something_to_build: Condvar::new(),
            handed_over: Mutex::new(Vec::new()),
            replaced: Mutex::new(Vec::new()),
            closing: AtomicBool::new(false),
        });
        let worker = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("scope-builds".into())
                .spawn(move || build_until_closed(&shared))
                .expect("a thread for the scope's builds")
        };
        let view = FrameView {
            values: initial,
            replaced_here: Vec::with_capacity(16),
            shared: Arc::clone(&shared),
        };
        (
            Scope {
                shared,
                worker: Some(worker),
            },
            view,
        )
    }
}

/// The writer's side of a running scope: sets and wires write here.
pub struct Scope {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Scope {
    /// Write `value` to `input`. A value equal to the one it holds changes nothing.
    pub fn write<T: Send + Sync + 'static>(&self, input: Input<T>, value: T) {
        let value: AnyValue = Arc::new(value);
        let mut scope = self.shared.slots.lock().unwrap();
        let slot = &scope.slots[input.slot];
        if slot
            .value
            .as_ref()
            .is_some_and(|held| (slot.equal)(held.as_ref(), value.as_ref()))
        {
            return;
        }
        scope.revision += 1;
        let revision = scope.revision;
        let slot = &mut scope.slots[input.slot];
        slot.value = Some(Arc::clone(&value));
        slot.changed_at = revision;
        self.shared
            .handed_over
            .lock()
            .unwrap()
            .push((input.slot, value));
        drop(scope);
        self.shared.something_to_build.notify_one();
    }

    /// How the last build of `derived` went.
    pub fn last_build_of<T>(&self, derived: Derived<T>) -> LastBuild {
        match &self.shared.slots.lock().unwrap().slots[derived.slot].role {
            SlotRole::Derived { last_build, .. } => last_build.clone(),
            SlotRole::Input => unreachable!("a derived handle names a derived slot"),
        }
    }

    /// Wait until no derived value is left to build, for tests.
    pub fn wait_until_settled(&self) {
        loop {
            {
                let scope = self.shared.slots.lock().unwrap();
                if (0..scope.slots.len()).all(|index| !is_left_to_build(&scope, index)) {
                    return;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::SeqCst);
        self.shared.something_to_build.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn is_left_to_build(scope: &ScopeSlots, index: usize) -> bool {
    match &scope.slots[index].role {
        SlotRole::Input => false,
        SlotRole::Derived {
            read_at,
            last_build,
            ..
        } => {
            *last_build == LastBuild::NotYetBuilt
                || read_at
                    .iter()
                    .any(|(read, seen)| scope.slots[*read].changed_at > *seen)
        }
    }
}

fn build_until_closed(shared: &Shared) {
    loop {
        // Values the frame thread replaced are released here, off the frame thread.
        drop(std::mem::take(&mut *shared.replaced.lock().unwrap()));
        let scope = shared.slots.lock().unwrap();
        let next = (0..scope.slots.len()).find(|index| is_left_to_build(&scope, *index));
        let Some(index) = next else {
            if shared.closing.load(Ordering::SeqCst) {
                return;
            }
            let _unused = shared
                .something_to_build
                .wait_timeout(scope, std::time::Duration::from_millis(20))
                .unwrap();
            continue;
        };
        let SlotRole::Derived { build, .. } = &scope.slots[index].role else {
            unreachable!("only a derived slot is left to build")
        };
        let build = Arc::clone(build);
        let reads = Reads {
            values: scope.slots.iter().map(|slot| slot.value.clone()).collect(),
            changed_at: scope.slots.iter().map(|slot| slot.changed_at).collect(),
            read: RefCell::new(Vec::new()),
        };
        drop(scope);
        let built = build(&reads);
        let mut scope = shared.slots.lock().unwrap();
        let revision = scope.revision;
        let read_at: Vec<(usize, u64)> = reads
            .read
            .borrow()
            .iter()
            .map(|read| (*read, reads.changed_at[*read]))
            .collect();
        // The newest inputs win: a result built from a value that changed meanwhile is thrown
        // away, and the loop builds again from the newest values.
        if read_at
            .iter()
            .any(|(read, seen)| scope.slots[*read].changed_at > *seen)
        {
            continue;
        }
        let slot = &mut scope.slots[index];
        let builds = match &slot.role {
            SlotRole::Derived {
                last_build: LastBuild::Built { builds } | LastBuild::Failed { builds, .. },
                ..
            } => builds + 1,
            _ => 1,
        };
        let last_build = match built {
            Ok(value) => {
                let unchanged = slot
                    .value
                    .as_ref()
                    .is_some_and(|held| (slot.equal)(held.as_ref(), value.as_ref()));
                if !unchanged {
                    slot.value = Some(Arc::clone(&value));
                    slot.changed_at = revision + 1;
                    shared.handed_over.lock().unwrap().push((index, value));
                }
                LastBuild::Built { builds }
            }
            // The previous value keeps serving; it is built again only when what it read changes.
            Err(why) => LastBuild::Failed { builds, why },
        };
        if let SlotRole::Derived {
            read_at: recorded,
            last_build: recorded_build,
            ..
        } = &mut slot.role
        {
            *recorded = read_at;
            *recorded_build = last_build;
        }
        if scope.slots[index].changed_at == revision + 1 {
            scope.revision = revision + 1;
        }
    }
}

/// The frame thread's view of a scope. It changes only at [`FrameView::flush`].
pub struct FrameView {
    values: Vec<Option<AnyValue>>,
    replaced_here: Vec<AnyValue>,
    shared: Arc<Shared>,
}

impl FrameView {
    /// Take every value handed over since the last flush, at a frame boundary. Never blocks: if
    /// the writer or the worker holds the hand-over, the view stays as it is until the next flush.
    pub fn flush(&mut self) {
        if let Ok(mut handed_over) = self.shared.handed_over.try_lock() {
            for (slot, value) in handed_over.drain(..) {
                if let Some(replaced) = self.values[slot].replace(value) {
                    self.replaced_here.push(replaced);
                }
            }
        }
        if !self.replaced_here.is_empty()
            && let Ok(mut replaced) = self.shared.replaced.try_lock()
        {
            replaced.append(&mut self.replaced_here);
            drop(replaced);
            self.shared.something_to_build.notify_one();
        }
    }

    /// The value of `input` as of the last flush.
    pub fn input<T: Send + Sync + 'static>(&self, input: Input<T>) -> &T {
        self.values[input.slot]
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>())
            .expect("an input always holds a value of its own type")
    }

    /// The value of `derived` as of the last flush; none until its first build is handed over.
    pub fn derived<T: Send + Sync + 'static>(&self, derived: Derived<T>) -> Option<&T> {
        self.values[derived.slot]
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>())
    }
}
