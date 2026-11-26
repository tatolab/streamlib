// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node's scope: its inputs, written by a set or a wire alike, and the values computed from
//! them, built on a worker thread off the frame thread. A build is a function of no arguments
//! whose `.get()` reads are its dependencies; a frame reads the values handed over at its last
//! frame boundary and never waits for a build.

use std::any::Any;
use std::cell::RefCell;
use std::fmt;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

type AnyValue = Arc<dyn Any + Send + Sync>;
type BuildOfAnyValue = Arc<dyn Fn() -> Result<AnyValue, String> + Send + Sync>;
type EqualityOfAnyValues = fn(&dyn Any, &dyn Any) -> bool;

static NEXT_SCOPE_ID: AtomicU64 = AtomicU64::new(0);

/// An input of a scope holding a `T`, read with [`Input::get`] in a build or a frame.
pub struct Input<T> {
    scope: u64,
    slot: usize,
    of: PhantomData<fn() -> T>,
}

/// A value computed from a scope's inputs and other computed values, holding a `T`, read with
/// [`Computed::get`] in a build or a frame.
pub struct Computed<T> {
    scope: u64,
    slot: usize,
    of: PhantomData<fn() -> T>,
}

impl<T> Clone for Input<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Input<T> {}
impl<T> Clone for Computed<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Computed<T> {}

/// A use of the scope's API that can never be right, refused with a panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    GetOutsideABuildOrAFrame,
    ComputedOutsideADeclaration,
    HandleOfAnotherScope,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refusal::GetOutsideABuildOrAFrame => {
                "`.get()` reads only in a build or a frame; read the value in a `computed` build \
                 or in the per-frame function"
            }
            Refusal::ComputedOutsideADeclaration => {
                "`computed` declares only while a scope is being declared, in the node's body"
            }
            Refusal::HandleOfAnotherScope => "the handle belongs to another scope",
        })
    }
}

fn refuse(refusal: Refusal) -> ! {
    panic!("{refusal}")
}

/// The unwind payload of a build's `.get()` of a computed value that has no value yet.
struct ReadOfAComputedValueNotYetBuilt;

/// What a build or a frame on this thread reads from.
enum Reading {
    Build {
        scope: u64,
        values: Vec<Option<AnyValue>>,
        read: Vec<usize>,
    },
    Frame {
        scope: u64,
        values: Vec<Option<AnyValue>>,
    },
}

struct Declaring {
    scope: u64,
    slots: Vec<Slot>,
}

thread_local! {
    static READING: RefCell<Option<Reading>> = const { RefCell::new(None) };
    static DECLARING: RefCell<Option<Declaring>> = const { RefCell::new(None) };
}

fn read_slot(scope: u64, slot: usize) -> Option<AnyValue> {
    READING.with_borrow_mut(|reading| {
        let (reading_scope, values) = match reading {
            None => refuse(Refusal::GetOutsideABuildOrAFrame),
            Some(Reading::Build {
                scope,
                values,
                read,
            }) => {
                read.push(slot);
                (*scope, values)
            }
            Some(Reading::Frame { scope, values }) => (*scope, values),
        };
        if reading_scope != scope {
            refuse(Refusal::HandleOfAnotherScope);
        }
        values[slot].clone()
    })
}

impl<T: Send + Sync + 'static> Input<T> {
    /// The input's value: in a build, as it was when the build started, recorded as read; in a
    /// frame, as of the frame's boundary. Refused anywhere else.
    pub fn get(self) -> Arc<T> {
        read_slot(self.scope, self.slot)
            .expect("an input always holds a value")
            .downcast::<T>()
            .expect("an input holds its own type")
    }
}

impl<T: Send + Sync + 'static> Computed<T> {
    /// The computed value: in a build, as it was when the build started, recorded as read, and a
    /// value not yet built fails the build until it is built; in a frame, always a value, since
    /// frames run only once every computed value has one. Refused anywhere else.
    pub fn get(self) -> Arc<T> {
        match read_slot(self.scope, self.slot) {
            Some(value) => value
                .downcast::<T>()
                .expect("a computed value holds its own type"),
            None => resume_unwind(Box::new(ReadOfAComputedValueNotYetBuilt)),
        }
    }
}

/// Declare a value `build` computes from what it reads with `.get()`, into the scope being
/// declared on this thread. Refused outside [`ScopeBeingDeclared::declare`].
pub fn computed<T, E, B>(build: B) -> Computed<T>
where
    T: PartialEq + Send + Sync + 'static,
    E: fmt::Display,
    B: Fn() -> Result<T, E> + Send + Sync + 'static,
{
    let build: BuildOfAnyValue = Arc::new(move || {
        build()
            .map(|value| Arc::new(value) as AnyValue)
            .map_err(|error| error.to_string())
    });
    DECLARING.with_borrow_mut(|declaring| {
        let Some(declaring) = declaring else {
            refuse(Refusal::ComputedOutsideADeclaration)
        };
        declaring.slots.push(Slot {
            role: SlotRole::Computed {
                build,
                read_at: Vec::new(),
                last_build: LastBuild::NotYetBuilt,
            },
            value: None,
            changed_at: 0,
            equal: equal_as::<T>,
        });
        Computed {
            scope: declaring.scope,
            slot: declaring.slots.len() - 1,
            of: PhantomData,
        }
    })
}

/// Why the last build of a computed value failed. The previous value keeps serving.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildFailure {
    /// The build returned this error.
    Returned(String),
    /// The build read a computed value that has no value yet; it builds again once that value
    /// is built.
    ReadAComputedValueNotYetBuilt,
    /// The build panicked with this message.
    Panicked(String),
}

impl fmt::Display for BuildFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildFailure::Returned(why) => f.write_str(why),
            BuildFailure::ReadAComputedValueNotYetBuilt => {
                f.write_str("it reads a computed value that is not built yet")
            }
            BuildFailure::Panicked(message) => write!(f, "it panicked: {message}"),
        }
    }
}

/// How the last build of a computed value went, for a node's diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LastBuild {
    NotYetBuilt,
    Built { builds: u64 },
    Failed { builds: u64, why: BuildFailure },
}

enum SlotRole {
    Input,
    Computed {
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
    scope: u64,
    slots: Mutex<ScopeSlots>,
    something_to_build: Condvar,
    handed_over: Mutex<Vec<(usize, AnyValue)>>,
    replaced: Mutex<Vec<AnyValue>>,
    closing: AtomicBool,
}

/// Builds run under `catch_unwind` with no lock held, so only a value's own `PartialEq` panicking
/// mid-comparison can poison a lock.
const NO_LOCK_HELD_ACROSS_A_PANIC: &str = "a value's PartialEq panicked while the scope was locked";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect(NO_LOCK_HELD_ACROSS_A_PANIC)
}

fn equal_as<T: PartialEq + 'static>(left: &dyn Any, right: &dyn Any) -> bool {
    match (left.downcast_ref::<T>(), right.downcast_ref::<T>()) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// A node's scope while it is being declared: its inputs, then its computed values.
pub struct ScopeBeingDeclared {
    scope: u64,
    slots: Vec<Slot>,
}

impl Default for ScopeBeingDeclared {
    fn default() -> Self {
        Self {
            scope: NEXT_SCOPE_ID.fetch_add(1, Ordering::Relaxed),
            slots: Vec::new(),
        }
    }
}

/// Gives the slots `computed` declared on this thread back to the scope, also when the body
/// unwinds.
struct DeclaringOnThisThread<'scope> {
    slots: &'scope mut Vec<Slot>,
    previous: Option<Declaring>,
}

impl Drop for DeclaringOnThisThread<'_> {
    fn drop(&mut self) {
        if let Some(declaring) = DECLARING.replace(self.previous.take()) {
            *self.slots = declaring.slots;
        }
    }
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
            scope: self.scope,
            slot: self.slots.len() - 1,
            of: PhantomData,
        }
    }

    /// Run a node's body once, with every [`computed`] it calls declared into this scope.
    pub fn declare<R>(&mut self, body: impl FnOnce() -> R) -> R {
        let previous = DECLARING.replace(Some(Declaring {
            scope: self.scope,
            slots: std::mem::take(&mut self.slots),
        }));
        let _declaring = DeclaringOnThisThread {
            slots: &mut self.slots,
            previous,
        };
        body()
    }

    /// Start the scope's worker, and return the scope and the one view its frame thread reads.
    pub fn start(self) -> (Scope, FrameView) {
        let initial: Vec<Option<AnyValue>> =
            self.slots.iter().map(|slot| slot.value.clone()).collect();
        let shared = Arc::new(Shared {
            scope: self.scope,
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
            ready: initial.iter().all(Option::is_some),
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
        if input.scope != self.shared.scope {
            refuse(Refusal::HandleOfAnotherScope);
        }
        let value: AnyValue = Arc::new(value);
        let mut scope = lock(&self.shared.slots);
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
            .expect(NO_LOCK_HELD_ACROSS_A_PANIC)
            .push((input.slot, value));
        drop(scope);
        self.shared.something_to_build.notify_one();
    }

    /// How the last build of `computed` went.
    pub fn last_build_of<T>(&self, computed: Computed<T>) -> LastBuild {
        if computed.scope != self.shared.scope {
            refuse(Refusal::HandleOfAnotherScope);
        }
        match &lock(&self.shared.slots).slots[computed.slot].role {
            SlotRole::Computed { last_build, .. } => last_build.clone(),
            SlotRole::Input => unreachable!("a computed handle names a computed slot"),
        }
    }

    /// Wait until no computed value is left to build, for tests.
    pub fn wait_until_settled(&self) {
        loop {
            {
                let scope = lock(&self.shared.slots);
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
        SlotRole::Computed {
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

fn message_of_a_panic(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "a panic with no message".to_string()
    }
}

/// Run `build` with its `.get()` reads served from `values` and recorded, and return its result
/// and the slots it read.
fn run_a_build(
    scope: u64,
    build: &BuildOfAnyValue,
    values: Vec<Option<AnyValue>>,
) -> (Result<AnyValue, BuildFailure>, Vec<usize>) {
    READING.set(Some(Reading::Build {
        scope,
        values,
        read: Vec::new(),
    }));
    let built = catch_unwind(AssertUnwindSafe(|| build()));
    let Some(Reading::Build { read, .. }) = READING.take() else {
        unreachable!("the worker's reading is the build's own")
    };
    let built = match built {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(why)) => Err(BuildFailure::Returned(why)),
        Err(payload) if payload.is::<ReadOfAComputedValueNotYetBuilt>() => {
            Err(BuildFailure::ReadAComputedValueNotYetBuilt)
        }
        Err(payload) => Err(BuildFailure::Panicked(message_of_a_panic(payload.as_ref()))),
    };
    (built, read)
}

fn build_until_closed(shared: &Shared) {
    loop {
        // Values the frame thread replaced are released here, off the frame thread.
        let replaced = std::mem::take(&mut *lock(&shared.replaced));
        drop(replaced);
        let scope = lock(&shared.slots);
        let next = (0..scope.slots.len()).find(|index| is_left_to_build(&scope, *index));
        let Some(index) = next else {
            if shared.closing.load(Ordering::SeqCst) {
                return;
            }
            let _unused = shared
                .something_to_build
                .wait_timeout(scope, std::time::Duration::from_millis(20))
                .expect(NO_LOCK_HELD_ACROSS_A_PANIC);
            continue;
        };
        let SlotRole::Computed { build, .. } = &scope.slots[index].role else {
            unreachable!("only a computed slot is left to build")
        };
        let build = Arc::clone(build);
        let values: Vec<Option<AnyValue>> =
            scope.slots.iter().map(|slot| slot.value.clone()).collect();
        let changed_at: Vec<u64> = scope.slots.iter().map(|slot| slot.changed_at).collect();
        drop(scope);
        let (built, read) = run_a_build(shared.scope, &build, values);
        let mut scope = lock(&shared.slots);
        let revision = scope.revision;
        let read_at: Vec<(usize, u64)> =
            read.iter().map(|read| (*read, changed_at[*read])).collect();
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
            SlotRole::Computed {
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
                    lock(&shared.handed_over).push((index, value));
                }
                LastBuild::Built { builds }
            }
            // The previous value keeps serving; it is built again only when what it read changes.
            Err(why) => LastBuild::Failed { builds, why },
        };
        if let SlotRole::Computed {
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
    ready: bool,
    replaced_here: Vec<AnyValue>,
    shared: Arc<Shared>,
}

/// Gives the view its values back from this thread's frame reading, also when the frame
/// unwinds.
struct FrameReadingOnThisThread<'view> {
    values: &'view mut Vec<Option<AnyValue>>,
    previous: Option<Reading>,
}

impl Drop for FrameReadingOnThisThread<'_> {
    fn drop(&mut self) {
        if let Some(Reading::Frame { values, .. }) = READING.replace(self.previous.take()) {
            *self.values = values;
        }
    }
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
        if !self.ready {
            self.ready = self.values.iter().all(Option::is_some);
        }
    }

    /// Flush, then run `frame` with `.get()` reading this view, once every computed value has
    /// its first value; none until then.
    pub fn run_frame<R>(&mut self, frame: impl FnOnce() -> R) -> Option<R> {
        self.flush();
        if !self.ready {
            return None;
        }
        let previous = READING.replace(Some(Reading::Frame {
            scope: self.shared.scope,
            values: std::mem::take(&mut self.values),
        }));
        let _reading = FrameReadingOnThisThread {
            values: &mut self.values,
            previous,
        };
        Some(frame())
    }
}
