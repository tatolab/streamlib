// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::CString;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};

use super::add_the_node_scope_to;

/// Run `source` with the node scope's names in its globals, the way a helper's node body calls
/// them, and fail the test with the Python traceback when it raises.
fn run_against_the_node_scope(source: &str) {
    Python::initialize();
    Python::attach(|python| {
        let module = PyModule::new(python, "node_scope_under_test").unwrap();
        add_the_node_scope_to(&module).unwrap();
        let globals = PyDict::new(python);
        globals.update(module.dict().as_mapping()).unwrap();
        let source = CString::new(source).unwrap();
        if let Err(raised) = python.run(&source, Some(&globals), None) {
            raised.print(python);
            panic!("the Python under test raised: {raised}");
        }
    });
}

const FRAME_LOOP: &str = r#"
import threading
import time

FRAME_THREAD = threading.get_ident()

def run_frames_until(scope, done, timeout_seconds=5.0):
    """Run frames 2 ms apart until `done()` answers true in one; the longest frame, in seconds."""
    deadline = time.monotonic() + timeout_seconds
    longest = 0.0
    answered = []
    while True:
        started = time.monotonic()
        scope.run_frame(lambda: answered.append(done()))
        longest = max(longest, time.monotonic() - started)
        if answered and answered[-1]:
            return longest
        assert time.monotonic() < deadline, "the frame loop waited too long"
        time.sleep(0.002)
"#;

fn run_with_the_frame_loop(source: &str) {
    run_against_the_node_scope(&format!("{FRAME_LOOP}\n{source}"));
}

#[test]
fn a_computed_value_that_sleeps_a_second_while_building_never_delays_a_frame() {
    run_with_the_frame_loop(
        r#"
declared = NodeScopeBeingDeclared()
seconds_to_build = declared.input(0.0)

def build_the_kernel():
    seconds = seconds_to_build.get()
    time.sleep(seconds)
    return ("kernel", seconds)

kernel = declared.declare(lambda: computed(build_the_kernel))
scope = declared.start()
run_frames_until(scope, lambda: kernel.get() == ("kernel", 0.0))

scope.write(seconds_to_build, 1.0)
frames_on_the_old_kernel = []
def taken_the_new_kernel():
    if kernel.get() == ("kernel", 0.0):
        frames_on_the_old_kernel.append(1)
        return False
    return kernel.get() == ("kernel", 1.0)

building_started = time.monotonic()
longest_frame = run_frames_until(scope, taken_the_new_kernel)
assert time.monotonic() - building_started >= 1.0
assert longest_frame < 0.25, f"a frame took {longest_frame:.3f} s while the kernel built"
assert len(frames_on_the_old_kernel) > 50, len(frames_on_the_old_kernel)
scope.close()
"#,
    );
}

#[test]
fn a_replaced_value_is_closed_once_off_the_frame_thread() {
    run_with_the_frame_loop(
        r#"
closes = []

class StubDevice:
    def __init__(self, device_id):
        self.device_id = device_id
    def close(self):
        closes.append((self.device_id, threading.get_ident()))

declared = NodeScopeBeingDeclared()
device_id = declared.input(0)
device = declared.declare(lambda: computed(lambda: StubDevice(device_id.get())))
scope = declared.start()
run_frames_until(scope, lambda: device.get().device_id == 0)

scope.write(device_id, 1)
run_frames_until(scope, lambda: device.get().device_id == 1 and len(closes) == 1)
scope.write(device_id, 2)
run_frames_until(scope, lambda: device.get().device_id == 2 and len(closes) == 2)
time.sleep(0.1)
run_frames_until(scope, lambda: True)

assert [closed for closed, _ in closes] == [0, 1], closes
assert all(thread != FRAME_THREAD for _, thread in closes), closes

scope.close()
assert [closed for closed, _ in closes] == [0, 1, 2], closes
"#,
    );
}

#[test]
fn a_build_that_returns_the_object_it_already_holds_does_not_close_it() {
    run_with_the_frame_loop(
        r#"
closes = []

class StubDevice:
    def close(self):
        closes.append(self)

the_one_device = StubDevice()
declared = NodeScopeBeingDeclared()
unread_by_the_device = declared.input(0)
def build_the_device():
    unread_by_the_device.get()
    return the_one_device
device = declared.declare(lambda: computed(build_the_device))
scope = declared.start()
run_frames_until(scope, lambda: device.get() is the_one_device)

scope.write(unread_by_the_device, 1)
assert scope.wait_until_settled(5.0)
assert scope.last_build_of(device).builds == 2
run_frames_until(scope, lambda: True)
time.sleep(0.1)
assert closes == []

scope.close()
assert closes == [the_one_device]
"#,
    );
}

#[test]
fn a_failed_build_keeps_the_previous_value_and_reports_the_error() {
    run_with_the_frame_loop(
        r#"
declared = NodeScopeBeingDeclared()
layer_count = declared.input(2)

def build_the_layout():
    count = layer_count.get()
    if count < 1:
        raise ValueError(f"a compositor needs a layer, not {count}")
    return ("layout", count)

layout = declared.declare(lambda: computed(build_the_layout))
scope = declared.start()
run_frames_until(scope, lambda: layout.get() == ("layout", 2))

scope.write(layer_count, 0)
assert scope.wait_until_settled(5.0)
last_build = scope.last_build_of(layout)
assert (last_build.state, last_build.builds, last_build.failure) == ("failed", 2, "raised"), last_build.state
assert last_build.why == "ValueError: a compositor needs a layer, not 0", last_build.why
frames = []
run_frames_until(scope, lambda: frames.append(layout.get()) or len(frames) == 3)
assert frames == [("layout", 2)] * 3, frames

scope.write(layer_count, 3)
run_frames_until(scope, lambda: layout.get() == ("layout", 3))
last_build = scope.last_build_of(layout)
assert (last_build.state, last_build.builds, last_build.failure, last_build.why) == ("built", 3, None, None)
scope.close()
"#,
    );
}

#[test]
fn a_build_reading_a_computed_value_not_yet_built_fails_and_builds_again_once_it_is() {
    run_with_the_frame_loop(
        r#"
declared = NodeScopeBeingDeclared()
taps = declared.input(4)

def build_the_odd_taps():
    count = taps.get()
    if count % 2 == 0:
        raise ValueError(f"{count} taps is even")
    return count

def declare():
    odd_taps = computed(build_the_odd_taps)
    return odd_taps, computed(lambda: ("kernel", odd_taps.get()))

odd_taps, kernel = declared.declare(declare)
scope = declared.start()
assert scope.wait_until_settled(5.0)
last_build = scope.last_build_of(kernel)
assert (last_build.state, last_build.failure) == ("failed", "read_a_computed_value_not_yet_built"), last_build.failure
assert not scope.run_frame(lambda: None)

scope.write(taps, 5)
run_frames_until(scope, lambda: kernel.get() == ("kernel", 5))
scope.close()
"#,
    );
}

#[test]
fn a_value_equal_by_eq_builds_nothing_downstream() {
    run_with_the_frame_loop(
        r#"
declared = NodeScopeBeingDeclared()
layers = declared.input(("background", "title"))

def declare():
    layer_count = computed(lambda: len(layers.get()))
    return layer_count, computed(lambda: ("kernels", layer_count.get()))

layer_count, kernels = declared.declare(declare)
scope = declared.start()
run_frames_until(scope, lambda: kernels.get() == ("kernels", 2))

scope.write(layers, ("background", "subtitle"))
assert scope.wait_until_settled(5.0)
assert scope.last_build_of(layer_count).builds == 2
assert scope.last_build_of(kernels).builds == 1
scope.close()
"#,
    );
}

#[test]
fn get_outside_a_build_or_a_frame_raises_its_own_kind() {
    run_against_the_node_scope(
        r#"
declared = NodeScopeBeingDeclared()
taps = declared.input(3)
try:
    taps.get()
except NodeScopeReadOutsideABuildOrAFrame as refused:
    assert "reads only in a build or a frame" in str(refused), str(refused)
else:
    raise AssertionError("a get in the body read a value")

others_taps = NodeScopeBeingDeclared().input(3)
scope = declared.start()
read = []
def read_another_scopes_input():
    try:
        others_taps.get()
    except NodeScopeHandleOfAnotherScope:
        read.append("refused")
assert scope.run_frame(read_another_scopes_input)
assert read == ["refused"]
scope.close()
"#,
    );
}

#[test]
fn a_closed_scope_refuses_a_write_and_a_frame() {
    run_against_the_node_scope(
        r#"
declared = NodeScopeBeingDeclared()
taps = declared.input(3)
scope = declared.start()
scope.close()
for refused_call in (lambda: scope.write(taps, 4), lambda: scope.run_frame(lambda: None)):
    try:
        refused_call()
    except RuntimeError as refused:
        assert "closed" in str(refused)
    else:
        raise AssertionError("a closed scope took a call")
"#,
    );
}

#[test]
fn an_object_a_computed_value_hands_on_from_an_input_is_not_closed() {
    run_with_the_frame_loop(
        r#"
closes = []

class StubDevice:
    def close(self):
        closes.append(self)

lent_device = StubDevice()
declared = NodeScopeBeingDeclared()
device = declared.input(lent_device)
gain = declared.input(1)
handed_on = declared.declare(lambda: computed(lambda: device.get() if gain.get() == 1 else None))
scope = declared.start()
run_frames_until(scope, lambda: handed_on.get() is lent_device)

scope.write(gain, 2)
run_frames_until(scope, lambda: handed_on.get() is None)
scope.write(device, StubDevice())
assert scope.wait_until_settled(5.0)
run_frames_until(scope, lambda: True)
time.sleep(0.1)
scope.close()
assert closes == [], closes
"#,
    );
}

#[test]
fn a_close_from_another_thread_waits_for_the_running_frame() {
    run_with_the_frame_loop(
        r#"
declared = NodeScopeBeingDeclared()
taps = declared.input(3)
kernel = declared.declare(lambda: computed(lambda: ("kernel", taps.get())))
scope = declared.start()
run_frames_until(scope, lambda: kernel.get() == ("kernel", 3))

frame_started = threading.Event()
frames_read = []
def a_long_frame():
    frame_started.set()
    for _ in range(50):
        frames_read.append(kernel.get())
        scope.write(taps, len(frames_read))
        time.sleep(0.002)

closer = threading.Thread(target=lambda: (frame_started.wait(5.0), scope.close()))
closer.start()
assert scope.run_frame(a_long_frame)
closer.join(5.0)
assert not closer.is_alive(), "the close never returned"
assert len(frames_read) == 50
try:
    scope.run_frame(lambda: None)
except RuntimeError as refused:
    assert "closed" in str(refused)
else:
    raise AssertionError("a closed scope ran a frame")
"#,
    );
}

#[test]
fn a_frame_closing_its_own_scope_is_refused() {
    run_with_the_frame_loop(
        r#"
declared = NodeScopeBeingDeclared()
taps = declared.input(3)
scope = declared.start()
refusals = []
def a_frame_that_closes():
    try:
        scope.close()
    except RuntimeError as refused:
        refusals.append(str(refused))
assert scope.run_frame(a_frame_that_closes)
assert len(refusals) == 1 and "cannot close its own" in refusals[0], refusals
scope.close()
"#,
    );
}

#[test]
fn a_replaced_value_with_no_close_is_freed_off_the_frame_thread() {
    run_with_the_frame_loop(
        r#"
freed_on = []

class LargeTensor:
    def __init__(self, size):
        self.size = size
    def __del__(self):
        freed_on.append((self.size, threading.get_ident()))

declared = NodeScopeBeingDeclared()
size = declared.input(1)
tensor = declared.declare(lambda: computed(lambda: LargeTensor(size.get())))
scope = declared.start()
run_frames_until(scope, lambda: tensor.get().size == 1)

scope.write(size, 2)
run_frames_until(scope, lambda: tensor.get().size == 2 and len(freed_on) == 1)
for _ in range(20):
    run_frames_until(scope, lambda: True)
assert [freed for freed, _ in freed_on] == [1], freed_on
assert all(thread != FRAME_THREAD for _, thread in freed_on), freed_on
scope.close()
"#,
    );
}

#[test]
fn a_value_one_scope_built_and_another_took_as_an_input_is_closed_once_neither_holds_it() {
    run_with_the_frame_loop(
        r#"
closes = []

class StubDevice:
    def __init__(self, device_id):
        self.device_id = device_id
    def close(self):
        closes.append(self.device_id)

declared_a = NodeScopeBeingDeclared()
device_id = declared_a.input(0)
device = declared_a.declare(lambda: computed(lambda: StubDevice(device_id.get())))
scope_a = declared_a.start()
run_frames_until(scope_a, lambda: device.get().device_id == 0)

declared_b = NodeScopeBeingDeclared()
wired_device = declared_b.input(None)
scope_b = declared_b.start()
device_0 = []
scope_a.run_frame(lambda: device_0.append(device.get()))
scope_b.write(wired_device, device_0[0])
scope_b.write(wired_device, None)
device_0.clear()

scope_a.write(device_id, 1)
run_frames_until(scope_a, lambda: device.get().device_id == 1 and closes == [0])
scope_a.close()
scope_b.close()
assert closes == [0, 1], closes
"#,
    );
}

#[test]
fn a_handle_of_another_scope_and_computed_outside_a_body_raise_their_own_kinds() {
    run_against_the_node_scope(
        r#"
other = NodeScopeBeingDeclared()
others_input = other.input(1)
others_kernel = other.declare(lambda: computed(lambda: others_input.get()))
scope = NodeScopeBeingDeclared().start()
for refused_call in (
    lambda: scope.write(others_input, 2),
    lambda: scope.last_build_of(others_kernel),
):
    try:
        refused_call()
    except NodeScopeHandleOfAnotherScope:
        pass
    else:
        raise AssertionError("a handle of another scope was taken")
try:
    computed(lambda: 1)
except NodeScopeComputedOutsideADeclaration:
    pass
else:
    raise AssertionError("computed declared outside a body")
scope.close()
other.start().close()
"#,
    );
}

#[test]
fn a_build_closing_or_dropping_its_own_scope_neither_hangs_nor_joins_itself() {
    run_with_the_frame_loop(
        r#"
closes = []
refusals = []
holder = []

class StubDevice:
    def close(self):
        closes.append(self)

declared = NodeScopeBeingDeclared()
generation = declared.input(0)
def build_the_device():
    if generation.get() == 1:
        try:
            holder[0].close()
        except RuntimeError as refused:
            refusals.append(str(refused))
    if generation.get() == 2:
        holder.clear()
    return StubDevice()
device = declared.declare(lambda: computed(build_the_device))
holder.append(declared.start())
run_frames_until(holder[0], lambda: True)

holder[0].write(generation, 1)
assert holder[0].wait_until_settled(5.0)
assert len(refusals) == 1 and "cannot close its own" in refusals[0], refusals

scope = holder[0]
scope.write(generation, 2)
del scope
deadline = time.monotonic() + 5.0
while len(closes) < 3 and time.monotonic() < deadline:
    time.sleep(0.01)
assert len(set(map(id, closes))) == 3, closes
"#,
    );
}
