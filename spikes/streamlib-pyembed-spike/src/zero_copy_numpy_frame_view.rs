// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Hands a Rust-owned pixel buffer to a Python callable as a numpy `uint8`
//! array that aliases the buffer, with no copy and no ownership transfer.
//!
//! rust-numpy has no safe public way to do this: `PyArray::from_slice` copies
//! element by element, `from_vec` / `into_pyarray` move the allocation into a
//! `PySliceContainer` base object, and `new_with_data` / `from_raw_parts` are
//! private. The only reachable route is the C API re-export — `PY_ARRAY_API`
//! plus `npyffi` — calling `PyArray_New` with a caller-supplied `data` pointer
//! and no `NPY_ARRAY_OWNDATA` flag.
//!
//! The view is valid only for the duration of the call. Nothing in numpy or
//! CPython enforces that. This module *detects* a violation after the fact by
//! reading the array's reference count; it cannot *prevent* one.

use std::ffi::{c_int, c_void};
use std::ptr;

use numpy::PY_ARRAY_API;
use numpy::npyffi::{self, NPY_ARRAY_C_CONTIGUOUS, NPY_ARRAY_WRITEABLE, NPY_TYPES, npy_intp};
use pyo3::exceptions::PyValueError;
use pyo3::types::PyAnyMethods;
use pyo3::{Bound, Py, PyAny, PyResult, Python};

/// Reference count the frame view is expected to carry once the callback has
/// returned and its return value has been released.
///
/// Exactly one strong reference exists at that point: the `Bound` this module
/// holds, created from the new reference `PyArray_New` returned. `call1` may
/// pass the argument through CPython's vectorcall protocol (borrowed, no
/// container) or through a temporary argument tuple depending on the build, but
/// either way that temporary is released before `call1` returns, and a Python
/// callee's frame drops its argument references when the frame exits. So a
/// well-behaved callback leaves the count at 1, and anything above 1 is a
/// reference the callback parked somewhere that outlives the call.
const FRAME_VIEW_REFCOUNT_WITH_NO_PYTHON_REFERENCE_HELD: isize = 1;

/// Whether the Python callback retained a reference to the frame view past its call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumpyFrameViewEscapeOutcome {
    NoEscape,
    RetainedByPythonWithRefcount(isize),
}

/// Invoke `callable` with a zero-copy numpy uint8 view of shape
/// (height, width, channel_count) aliasing `frame_pixel_bytes`.
///
/// The view aliases the caller's buffer for the duration of the call only. If
/// the callback keeps it, the returned outcome says so — dereferencing that
/// retained array after `frame_pixel_bytes` goes away is a use-after-free that
/// this function reports but does not prevent.
pub fn invoke_python_callback_over_zero_copy_frame_view(
    python: Python<'_>,
    callable: &Py<PyAny>,
    frame_pixel_bytes: &mut [u8],
    frame_height_pixels: usize,
    frame_width_pixels: usize,
    channel_count: usize,
) -> PyResult<NumpyFrameViewEscapeOutcome> {
    let required_byte_count = frame_height_pixels
        .checked_mul(frame_width_pixels)
        .and_then(|pixel_count| pixel_count.checked_mul(channel_count))
        .ok_or_else(|| {
            PyValueError::new_err(format!(
                "frame shape ({frame_height_pixels}, {frame_width_pixels}, {channel_count}) \
                 overflows a byte count"
            ))
        })?;
    if frame_pixel_bytes.len() != required_byte_count {
        return Err(PyValueError::new_err(format!(
            "frame buffer holds {} bytes but shape \
             ({frame_height_pixels}, {frame_width_pixels}, {channel_count}) needs \
             {required_byte_count}",
            frame_pixel_bytes.len()
        )));
    }

    let mut frame_view_dimensions: [npy_intp; 3] = [
        frame_height_pixels as npy_intp,
        frame_width_pixels as npy_intp,
        channel_count as npy_intp,
    ];
    let frame_pixel_bytes_pointer = frame_pixel_bytes.as_mut_ptr();

    // SAFETY: `PyArray_New` builds an array header around `data` without
    // reading or copying it. Soundness rests on four things:
    //   - The interpreter is attached (`python` proves it) and numpy's C API
    //     capsule is loaded, so the API slot and `PyArray_Type` are valid.
    //   - `frame_view_dimensions` lives until `PyArray_New` returns; numpy
    //     copies the dims into the array header, it does not alias them. Null
    //     `strides` asks numpy to compute C-contiguous strides; itemsize 0 is
    //     ignored for a fixed-width type like `NPY_UBYTE`.
    //   - The flags deliberately omit `NPY_ARRAY_OWNDATA`, and numpy clears it
    //     itself on the caller-supplied-data path, so numpy will never free the
    //     Rust allocation. Null `obj` means no subclass `__array_finalize__`.
    //   - `frame_pixel_bytes_pointer` is valid for `required_byte_count` bytes
    //     and, because `frame_pixel_bytes` is a `&mut` borrow held across this
    //     whole function, nothing else in Rust reads or writes it while Python
    //     can. That borrow is what makes the alias sound, and it is upheld by
    //     the caller through the borrow checker.
    // The obligation the borrow checker CANNOT uphold: the array must not
    // outlive this call. Python is free to stash it; if it does and later
    // touches it, that is a read or write of freed (or reused) Rust memory.
    // `read_numpy_frame_view_escape_outcome` below reports that it happened —
    // it does not stop it.
    let frame_view_array = unsafe {
        let raw_frame_view_array_pointer = PY_ARRAY_API.PyArray_New(
            python,
            npyffi::get_type_object(python, npyffi::NpyTypes::PyArray_Type),
            frame_view_dimensions.len() as c_int,
            frame_view_dimensions.as_mut_ptr(),
            NPY_TYPES::NPY_UBYTE as c_int,
            ptr::null_mut(),
            frame_pixel_bytes_pointer.cast::<c_void>(),
            0,
            NPY_ARRAY_C_CONTIGUOUS | NPY_ARRAY_WRITEABLE,
            ptr::null_mut(),
        );
        Bound::from_owned_ptr_or_err(python, raw_frame_view_array_pointer)?
    };

    let callback_call_result = callable.bind(python).call1((&frame_view_array,));

    // A callback that returns the view unchanged is well-behaved, but the
    // returned handle is a strong reference and would read as a retention.
    // Release it before counting.
    let callback_raised_error = match callback_call_result {
        Ok(callback_return_value) => {
            drop(callback_return_value);
            None
        }
        Err(callback_error) => Some(callback_error),
    };

    let escape_outcome = read_numpy_frame_view_escape_outcome(&frame_view_array);

    if let Some(callback_error) = callback_raised_error {
        // A raising callback is itself an escape route: the exception carries a
        // traceback holding the callback's frame, and that frame holds the view
        // in its locals. The error is what the caller asked about, so the
        // retention goes to the log rather than the return value.
        if let NumpyFrameViewEscapeOutcome::RetainedByPythonWithRefcount(observed_refcount) =
            escape_outcome
        {
            tracing::warn!(
                observed_refcount,
                "python callback raised; its traceback still references the frame view"
            );
        }
        return Err(callback_error);
    }

    Ok(escape_outcome)
}

/// Import numpy and load its C array API so the first frame does not pay for it.
///
/// Without this, the first `invoke_python_callback_over_zero_copy_frame_view`
/// on a process pays a module import inside the measured region and lands in
/// the p99.9 bucket.
pub fn preload_numpy_c_array_api_before_first_frame_view(python: Python<'_>) -> PyResult<()> {
    pyo3::types::PyModule::import(python, "numpy")?;
    // SAFETY: numpy imported cleanly above, so the capsule lookup this forces
    // has a module to read from. The returned type-object pointer is borrowed
    // from numpy's static API table and is only discarded here.
    let _ = unsafe { npyffi::get_type_object(python, npyffi::NpyTypes::PyArray_Type) };
    Ok(())
}

fn read_numpy_frame_view_escape_outcome(
    frame_view_array: &Bound<'_, PyAny>,
) -> NumpyFrameViewEscapeOutcome {
    // SAFETY: `frame_view_array` is a live strong reference held by this stack
    // frame, so the object header is allocated while `Py_REFCNT` reads it, and
    // the interpreter is attached for as long as the `Bound` exists. The read
    // takes no reference of its own, so it cannot leak.
    let observed_refcount = unsafe { pyo3::ffi::Py_REFCNT(frame_view_array.as_ptr()) };
    if observed_refcount > FRAME_VIEW_REFCOUNT_WITH_NO_PYTHON_REFERENCE_HELD {
        NumpyFrameViewEscapeOutcome::RetainedByPythonWithRefcount(observed_refcount)
    } else {
        NumpyFrameViewEscapeOutcome::NoEscape
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use pyo3::types::{PyDict, PyDictMethods, PyList, PyListMethods};

    use super::*;
    use crate::monotonic_clock::read_monotonic_clock_nanoseconds;

    fn initialize_interpreter_once() {
        static INTERPRETER_INITIALIZED: OnceLock<()> = OnceLock::new();
        INTERPRETER_INITIALIZED.get_or_init(Python::initialize);
    }

    /// Compiles `lambda_source` with `observation_sink` bound in its globals as
    /// a list, so a callback can report what it saw without retaining the view.
    fn compile_callback_with_observation_sink<'py>(
        python: Python<'py>,
        lambda_source: &std::ffi::CStr,
    ) -> (Py<PyAny>, Bound<'py, PyList>) {
        let callback_globals = PyDict::new(python);
        let observation_sink = PyList::empty(python);
        callback_globals
            .set_item("observation_sink", &observation_sink)
            .expect("globals accept the sink");
        let callable = python
            .eval(lambda_source, Some(&callback_globals), None)
            .expect("lambda compiles")
            .unbind();
        (callable, observation_sink)
    }

    /// Guards against a silent regression to a copying constructor: if the
    /// array ever stops aliasing the Rust allocation, every latency number this
    /// spike produces is measuring a memcpy that the real design would not pay.
    #[test]
    fn python_writes_through_the_view_into_the_rust_buffer() {
        initialize_interpreter_once();
        let mut frame_pixel_bytes = vec![0u8; 2 * 3 * 4];
        Python::attach(|python| {
            let callable = python
                .eval(
                    pyo3::ffi::c_str!("lambda frame_view: frame_view.__setitem__((0, 0, 0), 200)"),
                    None,
                    None,
                )
                .expect("lambda compiles")
                .unbind();
            let escape_outcome = invoke_python_callback_over_zero_copy_frame_view(
                python,
                &callable,
                &mut frame_pixel_bytes,
                2,
                3,
                4,
            )
            .expect("callback runs");
            assert_eq!(escape_outcome, NumpyFrameViewEscapeOutcome::NoEscape);
        });
        assert_eq!(frame_pixel_bytes[0], 200);
    }

    /// Guards the array header contract. `OWNDATA` true would mean numpy
    /// believes it may free the Rust allocation; a wrong dtype or a
    /// non-C-contiguous layout would make downstream Python silently reinterpret
    /// the pixels.
    #[test]
    fn the_view_is_a_c_contiguous_non_owning_uint8_array_of_the_requested_shape() {
        initialize_interpreter_once();
        let mut frame_pixel_bytes = vec![0u8; 5 * 7 * 3];
        Python::attach(|python| {
            let (callable, observation_sink) = compile_callback_with_observation_sink(
                python,
                pyo3::ffi::c_str!(
                    "lambda frame_view: observation_sink.append((\
                     bool(frame_view.flags.owndata), \
                     bool(frame_view.flags.c_contiguous), \
                     bool(frame_view.flags.writeable), \
                     str(frame_view.dtype), \
                     frame_view.shape))"
                ),
            );
            let escape_outcome = invoke_python_callback_over_zero_copy_frame_view(
                python,
                &callable,
                &mut frame_pixel_bytes,
                5,
                7,
                3,
            )
            .expect("callback runs");
            assert_eq!(escape_outcome, NumpyFrameViewEscapeOutcome::NoEscape);

            let (owns_data, is_c_contiguous, is_writeable, dtype_name, observed_shape): (
                bool,
                bool,
                bool,
                String,
                (usize, usize, usize),
            ) = observation_sink
                .get_item(0)
                .expect("the callback recorded one observation")
                .extract()
                .expect("observation has the recorded shape");

            assert!(!owns_data, "numpy must not believe it owns the Rust buffer");
            assert!(is_c_contiguous);
            assert!(is_writeable);
            assert_eq!(dtype_name, "uint8");
            assert_eq!(observed_shape, (5, 7, 3));
        });
    }

    /// Guards the escape check's sensitivity. A baseline off by one makes this
    /// check fire on every frame or on none, and either way it stops being
    /// evidence about whether in-process Python is safe to ship.
    #[test]
    fn a_callback_that_stashes_the_view_is_reported_as_retained() {
        initialize_interpreter_once();
        let mut frame_pixel_bytes = vec![0u8; 4 * 4 * 4];
        Python::attach(|python| {
            let (callable, retained_reference_sink) = compile_callback_with_observation_sink(
                python,
                pyo3::ffi::c_str!("lambda frame_view: observation_sink.append(frame_view)"),
            );
            let escape_outcome = invoke_python_callback_over_zero_copy_frame_view(
                python,
                &callable,
                &mut frame_pixel_bytes,
                4,
                4,
                4,
            )
            .expect("callback runs");

            match escape_outcome {
                NumpyFrameViewEscapeOutcome::RetainedByPythonWithRefcount(observed_refcount) => {
                    assert!(observed_refcount >= 2, "got {observed_refcount}");
                }
                NumpyFrameViewEscapeOutcome::NoEscape => {
                    panic!("a stashed view must be reported as retained")
                }
            }

            // Release the dangling view while the Rust buffer is still alive.
            // Letting it outlive `frame_pixel_bytes` is exactly the
            // use-after-free this module can report but not prevent.
            retained_reference_sink
                .call_method0("clear")
                .expect("the sink clears");
        });
    }

    /// Guards against a check that always fires: an ordinary callback must come
    /// back clean, otherwise the harness would flag every measured frame.
    #[test]
    fn a_well_behaved_callback_is_reported_as_no_escape() {
        initialize_interpreter_once();
        let mut frame_pixel_bytes = vec![0u8; 4 * 4 * 4];
        Python::attach(|python| {
            let callable = python
                .eval(pyo3::ffi::c_str!("lambda frame_view: int(frame_view[0, 0, 0])"), None, None)
                .expect("lambda compiles")
                .unbind();
            let escape_outcome = invoke_python_callback_over_zero_copy_frame_view(
                python,
                &callable,
                &mut frame_pixel_bytes,
                4,
                4,
                4,
            )
            .expect("callback runs");
            assert_eq!(escape_outcome, NumpyFrameViewEscapeOutcome::NoEscape);
        });
    }

    /// Guards against a shape that disagrees with the buffer reaching
    /// `PyArray_New`, which would hand Python an array reading past the
    /// allocation. It must surface as a Python exception, not a panic unwinding
    /// through the interpreter.
    #[test]
    fn a_shape_that_does_not_match_the_buffer_returns_a_value_error() {
        initialize_interpreter_once();
        let mut frame_pixel_bytes = vec![0u8; 10];
        Python::attach(|python| {
            let callable = python
                .eval(pyo3::ffi::c_str!("lambda frame_view: None"), None, None)
                .expect("lambda compiles")
                .unbind();
            let invocation_error = invoke_python_callback_over_zero_copy_frame_view(
                python,
                &callable,
                &mut frame_pixel_bytes,
                4,
                4,
                4,
            )
            .expect_err("a mismatched shape is rejected");
            assert!(invocation_error.is_instance_of::<PyValueError>(python));
        });
    }

    /// The load-bearing proof that the view is zero-copy rather than merely
    /// documented as such: a copying constructor costs one memcpy per frame, so
    /// a 6.75x larger frame would cost ~6.75x more. Aliasing costs the same for
    /// both. The threshold is deliberately loose so this asserts the mechanism,
    /// not a machine-specific ratio.
    #[test]
    fn invocation_cost_does_not_scale_with_frame_size() {
        initialize_interpreter_once();

        fn minimum_invocation_nanoseconds(
            python: Python<'_>,
            callable: &Py<PyAny>,
            frame_height_pixels: usize,
            frame_width_pixels: usize,
        ) -> i64 {
            const CHANNEL_COUNT: usize = 4;
            const WARMUP_ITERATIONS: usize = 200;
            const MEASURED_ITERATIONS: usize = 2_000;

            let mut frame_pixel_bytes =
                vec![0u8; frame_height_pixels * frame_width_pixels * CHANNEL_COUNT];
            let mut minimum_nanoseconds = i64::MAX;
            for iteration in 0..(WARMUP_ITERATIONS + MEASURED_ITERATIONS) {
                let started_at_nanoseconds = read_monotonic_clock_nanoseconds();
                invoke_python_callback_over_zero_copy_frame_view(
                    python,
                    callable,
                    &mut frame_pixel_bytes,
                    frame_height_pixels,
                    frame_width_pixels,
                    CHANNEL_COUNT,
                )
                .expect("callback runs");
                let elapsed_nanoseconds =
                    read_monotonic_clock_nanoseconds() - started_at_nanoseconds;
                if iteration >= WARMUP_ITERATIONS {
                    minimum_nanoseconds = minimum_nanoseconds.min(elapsed_nanoseconds);
                }
            }
            minimum_nanoseconds
        }

        Python::attach(|python| {
            preload_numpy_c_array_api_before_first_frame_view(python)
                .expect("numpy is importable");
            let callable = python
                .eval(pyo3::ffi::c_str!("lambda frame_view: None"), None, None)
                .expect("lambda compiles")
                .unbind();

            let small_frame_nanoseconds =
                minimum_invocation_nanoseconds(python, &callable, 480, 640);
            let large_frame_nanoseconds =
                minimum_invocation_nanoseconds(python, &callable, 1080, 1920);

            // 6.75x the bytes; allow 3x the time plus a fixed 2µs of scheduler
            // slack before calling it size-dependent.
            assert!(
                large_frame_nanoseconds <= small_frame_nanoseconds * 3 + 2_000,
                "cost scales with frame size, so the view is not zero-copy: \
                 640x480x4 min {small_frame_nanoseconds}ns vs \
                 1920x1080x4 min {large_frame_nanoseconds}ns"
            );
        });
    }
}
