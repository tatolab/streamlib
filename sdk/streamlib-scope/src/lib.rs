// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node's scope: its inputs, written by a set or a wire alike, and the values computed from
//! them, built on a worker thread off the frame thread. A build is a function of no arguments
//! whose `.get()` reads are its dependencies; a frame reads the values handed over at its last
//! frame boundary and never waits for a build.
//!
//! Inputs written together, as a transaction writes them, land as one write: no build and no
//! frame sees some of them written and not the others. The frame thread takes new values
//! without blocking or allocating, and the values it replaces are dropped on another thread.

use std::any::Any;
use std::cell::RefCell;
use std::fmt;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, TryLockError};

/// What the scope's threads meet through: std's locks and threads, or loom's in a loom model
/// build.
mod sync {
    #[cfg(loom)]
    pub(crate) use loom::sync::{Mutex, MutexGuard};
    #[cfg(loom)]
    pub(crate) use loom::thread::{Builder, JoinHandle, Thread, park};
    #[cfg(not(loom))]
    pub(crate) use std::sync::{Mutex, MutexGuard};
    #[cfg(not(loom))]
    pub(crate) use std::thread::{Builder, JoinHandle, Thread, park};
}

use sync::{Mutex, MutexGuard, Thread};

type AnyValue = Arc<dyn Any + Send + Sync>;
type BuildOfAnyValue = Arc<dyn Fn() -> Result<AnyValue, String> + Send + Sync>;
type EqualityOfAnyValues = fn(&dyn Any, &dyn Any) -> bool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScopeId(u64);

impl ScopeId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// The slot a handle names, in the scope it was declared in.
#[derive(Clone, Copy)]
struct SlotOf {
    scope_id: ScopeId,
    slot: usize,
}

impl SlotOf {
    fn in_scope(self, scope_id: ScopeId) -> usize {
        if self.scope_id != scope_id {
            refuse(Refusal::HandleOfAnotherScope);
        }
        self.slot
    }
}

/// An input of a scope holding a `T`, read with [`Input::get`] in a build or a frame.
pub struct Input<T> {
    slot: SlotOf,
    of: PhantomData<fn() -> T>,
}

/// A value computed from a scope's inputs and other computed values, holding a `T`, read with
/// [`Computed::get`] in a build or a frame.
pub struct Computed<T> {
    slot: SlotOf,
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
        scope_id: ScopeId,
        values: Vec<Option<AnyValue>>,
        read: Vec<usize>,
    },
    Frame {
        scope_id: ScopeId,
        values: Vec<Option<AnyValue>>,
    },
}

struct Declaring {
    scope_id: ScopeId,
    slots: Vec<Slot>,
}

#[cfg(not(loom))]
std::thread_local! {
    static READING: RefCell<Option<Reading>> = const { RefCell::new(None) };
    static DECLARING: RefCell<Option<Declaring>> = const { RefCell::new(None) };
}

#[cfg(loom)]
loom::thread_local! {
    static READING: RefCell<Option<Reading>> = RefCell::new(None);
    static DECLARING: RefCell<Option<Declaring>> = RefCell::new(None);
}

fn replace_reading(reading: Option<Reading>) -> Option<Reading> {
    READING.with(|cell| cell.replace(reading))
}

fn replace_declaring(declaring: Option<Declaring>) -> Option<Declaring> {
    DECLARING.with(|cell| cell.replace(declaring))
}

fn read_slot(slot: SlotOf) -> Option<AnyValue> {
    READING.with(|reading| match &mut *reading.borrow_mut() {
        None => refuse(Refusal::GetOutsideABuildOrAFrame),
        Some(Reading::Build {
            scope_id,
            values,
            read,
        }) => {
            let slot = slot.in_scope(*scope_id);
            read.push(slot);
            values[slot].clone()
        }
        Some(Reading::Frame { scope_id, values }) => values[slot.in_scope(*scope_id)].clone(),
    })
}

impl<T: Send + Sync + 'static> Input<T> {
    /// The input's value: in a build, as it was when the build started, recorded as read; in a
    /// frame, as of the frame's boundary. Refused anywhere else.
    #[must_use]
    pub fn get(self) -> Arc<T> {
        read_slot(self.slot)
            .expect("an input always holds a value")
            .downcast::<T>()
            .expect("an input holds its own type")
    }
}

impl<T: Send + Sync + 'static> Computed<T> {
    /// The computed value: in a build, as it was when the build started, recorded as read, and a
    /// value not yet built fails the build until it is built; in a frame, always a value, since
    /// frames run only once every computed value has one. Refused anywhere else.
    #[must_use]
    pub fn get(self) -> Arc<T> {
        match read_slot(self.slot) {
            Some(value) => value
                .downcast::<T>()
                .expect("a computed value holds its own type"),
            None => resume_unwind(Box::new(ReadOfAComputedValueNotYetBuilt)),
        }
    }
}

fn declare_slot(scope_id: ScopeId, slots: &mut Vec<Slot>, slot: Slot) -> SlotOf {
    slots.push(slot);
    SlotOf {
        scope_id,
        slot: slots.len() - 1,
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
    let slot = Slot {
        role: SlotRole::Computed {
            build,
            read_at: Vec::new(),
            last_build: LastBuild::NotYetBuilt,
        },
        value: None,
        changed_at: 0,
        equal: equal_as::<T>,
    };
    DECLARING.with(|declaring| {
        let Some(declaring) = &mut *declaring.borrow_mut() else {
            refuse(Refusal::ComputedOutsideADeclaration)
        };
        Computed {
            slot: declare_slot(declaring.scope_id, &mut declaring.slots, slot),
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

impl LastBuild {
    fn builds(&self) -> u64 {
        match self {
            LastBuild::NotYetBuilt => 0,
            LastBuild::Built { builds } | LastBuild::Failed { builds, .. } => *builds,
        }
    }
}

/// A slot a build read, and the revision it had changed at when the build started.
struct Read {
    slot: usize,
    seen: u64,
}

enum SlotRole {
    Input,
    Computed {
        build: BuildOfAnyValue,
        read_at: Vec<Read>,
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
    /// Set by the writer as the scope closes; the worker starts no build once it sees it.
    closing: bool,
}

/// A build to run off the lock: the slot it builds and the slots' values and revisions as it
/// starts.
struct NextBuild {
    index: usize,
    build: BuildOfAnyValue,
    values: Vec<Option<AnyValue>>,
    changed_at: Vec<u64>,
}

impl ScopeSlots {
    fn values(&self) -> Vec<Option<AnyValue>> {
        self.slots.iter().map(|slot| slot.value.clone()).collect()
    }

    fn any_changed_since(&self, read_at: &[Read]) -> bool {
        read_at
            .iter()
            .any(|read| self.slots[read.slot].changed_at > read.seen)
    }

    fn is_left_to_build(&self, index: usize) -> bool {
        match &self.slots[index].role {
            SlotRole::Input => false,
            SlotRole::Computed {
                read_at,
                last_build,
                ..
            } => *last_build == LastBuild::NotYetBuilt || self.any_changed_since(read_at),
        }
    }

    fn is_settled(&self) -> bool {
        (0..self.slots.len()).all(|index| !self.is_left_to_build(index))
    }

    /// Store `value` in slot `index` and return it to hand over, unless it equals the value the
    /// slot holds. The slot's previous value is never its last holder: the frame view or the
    /// hand-over holds it too.
    fn store(&mut self, index: usize, value: &AnyValue) -> Option<AnyValue> {
        let slot = &mut self.slots[index];
        if slot
            .value
            .as_ref()
            .is_some_and(|held| (slot.equal)(held.as_ref(), value.as_ref()))
        {
            return None;
        }
        self.revision += 1;
        slot.value = Some(Arc::clone(value));
        slot.changed_at = self.revision;
        Some(Arc::clone(value))
    }

    /// The first slot left to build, in declaration order from `first`, wrapping around.
    fn next_build(&self, first: usize) -> Option<NextBuild> {
        let count = self.slots.len();
        let index = (0..count)
            .map(|offset| (first + offset) % count)
            .find(|index| self.is_left_to_build(*index))?;
        let SlotRole::Computed { build, .. } = &self.slots[index].role else {
            unreachable!("only a computed slot is left to build")
        };
        Some(NextBuild {
            index,
            build: Arc::clone(build),
            values: self.values(),
            changed_at: self.slots.iter().map(|slot| slot.changed_at).collect(),
        })
    }

    /// Record how the build of slot `index` went, and return its value to hand over when it
    /// changed. A failure keeps the previous value, built again only when what it read changes.
    fn record_build(
        &mut self,
        index: usize,
        read_at: Vec<Read>,
        built: &Result<AnyValue, BuildFailure>,
    ) -> Option<AnyValue> {
        let SlotRole::Computed { last_build, .. } = &self.slots[index].role else {
            unreachable!("only a computed slot is built")
        };
        let builds = last_build.builds() + 1;
        let (handed_over, last_build) = match built {
            Ok(value) => (self.store(index, value), LastBuild::Built { builds }),
            Err(why) => (
                None,
                LastBuild::Failed {
                    builds,
                    why: why.clone(),
                },
            ),
        };
        let SlotRole::Computed {
            read_at: recorded_read_at,
            last_build: recorded_last_build,
            ..
        } = &mut self.slots[index].role
        else {
            unreachable!("only a computed slot is built")
        };
        *recorded_read_at = read_at;
        *recorded_last_build = last_build;
        handed_over
    }
}

/// What one slot's hand-over cell holds between the writer or the worker and the frame view.
enum HandOver {
    Empty,
    /// The slot's newest value, not yet taken by the frame view.
    New(AnyValue),
    /// The value the frame view replaced, left for another thread to drop.
    Replaced(AnyValue),
}

impl HandOver {
    fn into_value(self) -> Option<AnyValue> {
        match self {
            HandOver::Empty => None,
            HandOver::New(value) | HandOver::Replaced(value) => Some(value),
        }
    }

    /// The new value the cell holds, leaving it empty; any other cell is left as it is.
    fn take_new(&mut self) -> Option<AnyValue> {
        match std::mem::replace(self, HandOver::Empty) {
            HandOver::New(value) => Some(value),
            other => {
                *self = other;
                None
            }
        }
    }

    /// The replaced value the cell holds, leaving it empty; any other cell is left as it is.
    fn take_replaced(&mut self) -> Option<AnyValue> {
        match std::mem::replace(self, HandOver::Empty) {
            HandOver::Replaced(value) => Some(value),
            other => {
                *self = other;
                None
            }
        }
    }
}

struct ScopeAcrossThreads {
    scope_id: ScopeId,
    slots: Mutex<ScopeSlots>,
    hand_over: Mutex<Vec<HandOver>>,
}

const NO_LOCK_HELD_ACROSS_A_PANIC: &str = "a value's PartialEq panicked while the scope was locked";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect(NO_LOCK_HELD_ACROSS_A_PANIC)
}

/// The lock if no other thread holds it, for the frame thread, which never waits.
fn lock_if_free<T>(mutex: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    match mutex.try_lock() {
        Ok(guard) => Some(guard),
        Err(TryLockError::WouldBlock) => None,
        Err(TryLockError::Poisoned(_)) => panic!("{NO_LOCK_HELD_ACROSS_A_PANIC}"),
    }
}

fn equal_as<T: PartialEq + 'static>(left: &dyn Any, right: &dyn Any) -> bool {
    match (left.downcast_ref::<T>(), right.downcast_ref::<T>()) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// A node's scope while it is being declared: its inputs, then its computed values.
pub struct ScopeBeingDeclared {
    scope_id: ScopeId,
    slots: Vec<Slot>,
}

impl Default for ScopeBeingDeclared {
    fn default() -> Self {
        Self {
            scope_id: ScopeId::next(),
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
        if let Some(declaring) = replace_declaring(self.previous.take()) {
            *self.slots = declaring.slots;
        }
    }
}

impl ScopeBeingDeclared {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An input holding `value` until it is written.
    pub fn input<T: PartialEq + Send + Sync + 'static>(&mut self, value: T) -> Input<T> {
        let slot = Slot {
            role: SlotRole::Input,
            value: Some(Arc::new(value)),
            changed_at: 0,
            equal: equal_as::<T>,
        };
        Input {
            slot: declare_slot(self.scope_id, &mut self.slots, slot),
            of: PhantomData,
        }
    }

    /// Run a node's body once, with every [`computed`] it calls declared into this scope.
    pub fn declare<R>(&mut self, body: impl FnOnce() -> R) -> R {
        let previous = replace_declaring(Some(Declaring {
            scope_id: self.scope_id,
            slots: std::mem::take(&mut self.slots),
        }));
        let _declaring = DeclaringOnThisThread {
            slots: &mut self.slots,
            previous,
        };
        body()
    }

    /// Start the scope, and return it and the one view its frame thread reads. A scope with a
    /// computed value starts a worker thread to build it; a scope of inputs alone starts none.
    pub fn start(self) -> std::io::Result<(Scope, FrameView)> {
        let builds_anything = self
            .slots
            .iter()
            .any(|slot| matches!(slot.role, SlotRole::Computed { .. }));
        let slots = ScopeSlots {
            revision: 0,
            slots: self.slots,
            closing: false,
        };
        let initial = slots.values();
        let slot_count = initial.len();
        let shared = Arc::new(ScopeAcrossThreads {
            scope_id: self.scope_id,
            slots: Mutex::new(slots),
            hand_over: Mutex::new((0..slot_count).map(|_| HandOver::Empty).collect()),
        });
        let worker = if builds_anything {
            let shared = Arc::clone(&shared);
            Some(
                sync::Builder::new()
                    .name("scope-builds".into())
                    .spawn(move || build_until_closed(&shared))?,
            )
        } else {
            None
        };
        let view = FrameView {
            ready: initial.iter().all(Option::is_some),
            values: initial,
            worker: worker.as_ref().map(|worker| worker.thread().clone()),
            shared: Arc::clone(&shared),
        };
        let writer = Scope { shared, worker };
        Ok((writer, view))
    }
}

/// The writer's side of a running scope: sets and wires write here.
#[must_use]
pub struct Scope {
    shared: Arc<ScopeAcrossThreads>,
    worker: Option<sync::JoinHandle<()>>,
}

/// The inputs one [`Scope::write`] writes, applied together once it has them all.
pub struct Writes {
    scope_id: ScopeId,
    values: Vec<(usize, AnyValue)>,
}

impl Writes {
    /// Write `value` to `input` with the others. A later write of the same input wins.
    pub fn write<T: Send + Sync + 'static>(&mut self, input: Input<T>, value: T) {
        let index = input.slot.in_scope(self.scope_id);
        self.values.push((index, Arc::new(value)));
    }
}

impl Scope {
    /// Write the inputs `writes` names, all as one write: a build or a frame sees all of them
    /// or none. A value equal to the one its input holds changes nothing.
    pub fn write(&self, writes: impl FnOnce(&mut Writes)) {
        let mut written = Writes {
            scope_id: self.shared.scope_id,
            values: Vec::new(),
        };
        writes(&mut written);
        let mut slots = lock(&self.shared.slots);
        let changed: Vec<(usize, AnyValue)> = written
            .values
            .iter()
            .filter_map(|(index, value)| slots.store(*index, value).map(|value| (*index, value)))
            .collect();
        if changed.is_empty() {
            drop(slots);
            drop(written);
            return;
        }
        let superseded = self.shared.hand_over(changed);
        drop(slots);
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
        drop(superseded);
        drop(written);
    }

    /// How the last build of `computed` went.
    #[must_use]
    pub fn last_build_of<T>(&self, computed: Computed<T>) -> LastBuild {
        let index = computed.slot.in_scope(self.shared.scope_id);
        match &lock(&self.shared.slots).slots[index].role {
            SlotRole::Computed { last_build, .. } => last_build.clone(),
            SlotRole::Input => unreachable!("a computed handle names a computed slot"),
        }
    }

    /// Wait up to `timeout` until no computed value is left to build, for tests; whether it
    /// settled.
    #[must_use]
    pub fn wait_until_settled(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if lock_if_free(&self.shared.slots).is_some_and(|slots| slots.is_settled()) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

impl Drop for Scope {
    /// Close the scope: a build in flight finishes, and no pending build starts.
    fn drop(&mut self) {
        lock(&self.shared.slots).closing = true;
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
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
    scope_id: ScopeId,
    build: &BuildOfAnyValue,
    values: Vec<Option<AnyValue>>,
) -> (Result<AnyValue, BuildFailure>, Vec<usize>) {
    let previous = replace_reading(Some(Reading::Build {
        scope_id,
        values,
        read: Vec::new(),
    }));
    let built = catch_unwind(AssertUnwindSafe(|| build()));
    let Some(Reading::Build { mut read, .. }) = replace_reading(previous) else {
        unreachable!("the worker's reading is the build's own")
    };
    read.sort_unstable();
    read.dedup();
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

impl ScopeAcrossThreads {
    /// Hand each changed value over to the frame view, all under one lock, in place of what its
    /// slot's cell held: a value the view has not taken yet, or one it replaced. Return those to
    /// drop outside the locks.
    fn hand_over(&self, changed: impl IntoIterator<Item = (usize, AnyValue)>) -> Vec<AnyValue> {
        let mut hand_over = lock(&self.hand_over);
        changed
            .into_iter()
            .filter_map(|(index, value)| {
                std::mem::replace(&mut hand_over[index], HandOver::New(value)).into_value()
            })
            .collect()
    }

    /// Drop the values the frame view replaced, outside the lock.
    fn release_replaced(&self) {
        let mut hand_over = lock(&self.hand_over);
        let released: Vec<AnyValue> = hand_over
            .iter_mut()
            .filter_map(HandOver::take_replaced)
            .collect();
        drop(hand_over);
        drop(released);
    }
}

fn build_until_closed(shared: &ScopeAcrossThreads) {
    // After a build is thrown away, the next search starts past it, so a value whose inputs
    // change faster than it builds does not starve the others.
    let mut first = 0;
    loop {
        shared.release_replaced();
        let slots = lock(&shared.slots);
        if slots.closing {
            return;
        }
        let Some(next) = slots.next_build(first) else {
            drop(slots);
            // A write, a give-back or the close unparks the worker; an unpark that lands before
            // the park makes it return at once.
            sync::park();
            continue;
        };
        drop(slots);
        let (built, read) = run_a_build(shared.scope_id, &next.build, next.values);
        let read_at: Vec<Read> = read
            .into_iter()
            .map(|slot| Read {
                slot,
                seen: next.changed_at[slot],
            })
            .collect();
        let mut slots = lock(&shared.slots);
        // The newest inputs win: the loop builds again from the newest values.
        if slots.any_changed_since(&read_at) {
            drop(slots);
            drop(built);
            first = next.index + 1;
            continue;
        }
        first = 0;
        let superseded = slots
            .record_build(next.index, read_at, &built)
            .map(|value| shared.hand_over(std::iter::once((next.index, value))));
        drop(slots);
        // An equal, superseded or thrown-away value is released only after the lock, in case its
        // Drop is slow.
        drop(superseded);
        drop(built);
    }
}

/// The frame thread's view of a scope. It changes only at [`FrameView::flush`].
#[must_use]
pub struct FrameView {
    values: Vec<Option<AnyValue>>,
    ready: bool,
    worker: Option<Thread>,
    shared: Arc<ScopeAcrossThreads>,
}

/// Gives the view its values back from this thread's frame reading, also when the frame
/// unwinds.
struct FrameReadingOnThisThread<'view> {
    values: &'view mut Vec<Option<AnyValue>>,
    previous: Option<Reading>,
}

impl Drop for FrameReadingOnThisThread<'_> {
    fn drop(&mut self) {
        if let Some(Reading::Frame { values, .. }) = replace_reading(self.previous.take()) {
            *self.values = values;
        }
    }
}

impl FrameView {
    /// Take every value handed over since the last flush, at a frame boundary, all under one
    /// lock, so the view never holds part of one write. Never blocks and never allocates: if
    /// another thread holds the hand-over, the view stays as it is until the next flush. Each
    /// value it replaces is left in its slot's cell, for the worker, or the next write of that
    /// slot, to drop.
    pub fn flush(&mut self) {
        let Some(mut hand_over) = lock_if_free(&self.shared.hand_over) else {
            return;
        };
        let mut replaced_any = false;
        for (slot, cell) in hand_over.iter_mut().enumerate() {
            let Some(value) = cell.take_new() else {
                continue;
            };
            if let Some(replaced) = self.values[slot].replace(value) {
                *cell = HandOver::Replaced(replaced);
                replaced_any = true;
            }
        }
        drop(hand_over);
        if replaced_any && let Some(worker) = &self.worker {
            worker.unpark();
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
        let previous = replace_reading(Some(Reading::Frame {
            scope_id: self.shared.scope_id,
            values: std::mem::take(&mut self.values),
        }));
        let _reading = FrameReadingOnThisThread {
            values: &mut self.values,
            previous,
        };
        Some(frame())
    }
}
