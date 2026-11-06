// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A message's payload as Python holds it: a read's borrow of the sample it
//! arrived in, and a write's loan of the sample it will be published in. Both
//! are buffers over the sample's own shared memory and DLPack exports of it.

use std::ffi::{c_int, c_void};
use std::sync::Arc;

use pyo3::exceptions::{PyBufferError, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use streamlib::sdk::error::Error;
use streamlib::sdk::iceoryx2::{
    InputPortMessagePayload, MessageGroupBoundary, OutputPortPayloadLoan, OutputWriterInner,
};

use crate::python_gpu_surface_pixel_exchange::{
    HOST_VISIBLE_DLPACK_DEVICE, PixelExchangeTensorLayout, dlpack_capsule_over,
    dlpack_device_as_python_pair, exchange_shape_for_max_version,
};
use crate::python_processor_context::PooledGpuFrameClaimHeldByARead;

/// A read message's payload, held where the sample put it until the borrow and
/// every export of it are gone.
#[pyclass(name = "PortPayloadBorrow", module = "tatolab.stream", frozen)]
pub(crate) struct PythonPortPayloadBorrow {
    /// Locked only to reach the bytes, which stay where they are and are never
    /// written: a sample is `Send` and not `Sync`.
    borrowed_payload: Arc<parking_lot::Mutex<InputPortMessagePayload>>,
    payload_base_address: usize,
    payload_byte_len: usize,
    _pooled_gpu_frame_claim: Option<PooledGpuFrameClaimHeldByARead>,
}

impl PythonPortPayloadBorrow {
    /// A copy of the bytes a read handed back as a borrow.
    #[cfg(test)]
    pub(crate) fn bytes_the_read_borrowed(read: &Bound<'_, PyAny>) -> Vec<u8> {
        read.cast::<Self>()
            .expect("a read without `into` hands back a PortPayloadBorrow")
            .get()
            .payload_bytes()
            .to_vec()
    }

    /// Where the borrowed bytes start, and whether they are a channel sample's.
    #[cfg(test)]
    pub(crate) fn borrowed_payload_address_and_whether_it_is_a_channel_sample(
        &self,
    ) -> (usize, bool) {
        (
            self.payload_base_address,
            matches!(
                *self.borrowed_payload.lock(),
                InputPortMessagePayload::BorrowedChannelSample(_)
            ),
        )
    }

    /// Hold `payload` for Python, with the claim a read took on the pooled
    /// GPU frame it carries.
    pub(crate) fn holding(
        payload: InputPortMessagePayload,
        pooled_gpu_frame_claim: Option<PooledGpuFrameClaimHeldByARead>,
    ) -> Self {
        let payload_bytes = payload.bytes();
        let payload_base_address = payload_bytes.as_ptr() as usize;
        let payload_byte_len = payload_bytes.len();
        Self {
            borrowed_payload: Arc::new(parking_lot::Mutex::new(payload)),
            payload_base_address,
            payload_byte_len,
            _pooled_gpu_frame_claim: pooled_gpu_frame_claim,
        }
    }

    fn payload_bytes(&self) -> &[u8] {
        if self.payload_byte_len == 0 {
            return &[];
        }
        // SAFETY: the payload's bytes neither move nor change while
        // `borrowed_payload` holds it, and `self` holds that.
        unsafe {
            std::slice::from_raw_parts(
                self.payload_base_address as *const u8,
                self.payload_byte_len,
            )
        }
    }
}

#[pymethods]
impl PythonPortPayloadBorrow {
    /// The payload's length in bytes.
    fn __len__(&self) -> usize {
        self.payload_byte_len
    }

    /// The payload's bytes as a copy.
    fn __bytes__<'py>(&self, python: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(python, self.payload_bytes())
    }

    /// The payload as a read-only buffer over the sample, never a copy.
    ///
    /// # Safety
    ///
    /// CPython calls this with a valid `view` to fill.
    unsafe fn __getbuffer__(
        slf: Bound<'_, Self>,
        view: *mut pyo3::ffi::Py_buffer,
        flags: c_int,
    ) -> PyResult<()> {
        let payload_bytes = slf.get().payload_bytes();
        // SAFETY: `view` is CPython's to fill; the bytes live as long as the
        // exporter `view.obj` holds a reference to, and they are never written.
        unsafe {
            fill_buffer_view_over(
                view,
                slf.as_any(),
                payload_bytes.as_ptr().cast_mut(),
                payload_bytes.len(),
                BufferViewAccess::ReadOnly,
                flags,
            )
        }
    }

    /// The payload as a 1-D `uint8` CPU tensor over the sample, read-only.
    #[pyo3(signature = (*, stream = None, max_version = None, dl_device = None, copy = None))]
    fn __dlpack__<'py>(
        &self,
        python: Python<'py>,
        stream: Option<&Bound<'py, PyAny>>,
        max_version: Option<(u32, u32)>,
        dl_device: Option<(i32, i32)>,
        copy: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        refuse_a_dlpack_request_a_cpu_payload_cannot_meet(stream, dl_device, copy)?;
        let payload_bytes = self.payload_bytes();
        dlpack_capsule_over(
            python,
            payload_bytes.as_ptr() as u64,
            PixelExchangeTensorLayout::one_dimensional_bytes(payload_bytes.len())?,
            HOST_VISIBLE_DLPACK_DEVICE,
            exchange_shape_for_max_version(max_version),
            true,
            Box::new(Arc::clone(&self.borrowed_payload)),
        )
    }

    /// `(kDLCPU, 0)`: the sample is host memory.
    fn __dlpack_device__(&self) -> (i32, i32) {
        dlpack_device_as_python_pair(HOST_VISIBLE_DLPACK_DEVICE)
    }
}

/// A sample loaned on an output port for its writer to fill, published when
/// its `with` block exits cleanly.
#[pyclass(name = "PortPayloadLoan", module = "tatolab.stream", frozen)]
pub(crate) struct PythonPortPayloadLoan {
    output_writer: Option<Arc<OutputWriterInner>>,
    output_port: String,
    timestamp_ns: Option<i64>,
    group_boundary: MessageGroupBoundary,
    loaned_payload: Arc<parking_lot::Mutex<LoanedPayloadHeldForPython>>,
    payload_base_address: usize,
    payload_byte_len: usize,
}

/// Where a Python loan's bytes are, and how many exports of them live.
struct LoanedPayloadHeldForPython {
    loaned_bytes: Option<LoanedPayloadBytes>,
    live_export_count: usize,
    given_back_once_unexported: bool,
}

impl LoanedPayloadHeldForPython {
    fn is_closed(&self) -> bool {
        self.loaned_bytes.is_none() || self.given_back_once_unexported
    }

    fn count_one_export_released(&mut self) {
        self.live_export_count -= 1;
        if self.live_export_count == 0 && self.given_back_once_unexported {
            self.loaned_bytes.take();
        }
    }

    fn give_back_once_unexported(&mut self) {
        if self.live_export_count == 0 {
            self.loaned_bytes.take();
        } else {
            self.given_back_once_unexported = true;
        }
    }
}

/// The bytes a Python loan hands out.
enum LoanedPayloadBytes {
    /// The engine's loan: a sample, or scratch for a port no link reached.
    EngineLoan(OutputPortPayloadLoan),
    /// Scratch for a port nothing is wired to or a payload past its ceiling.
    ScratchPublishedNowhere(Vec<u8>),
}

impl LoanedPayloadBytes {
    fn bytes_mut(&mut self) -> &mut [u8] {
        match self {
            Self::EngineLoan(loan) => loan.payload_bytes_mut(),
            Self::ScratchPublishedNowhere(scratch_bytes) => scratch_bytes,
        }
    }
}

/// One live export of a loan; the loan's bytes stay held until the last one
/// drops, even past a block that gave the sample back.
struct LoanExportHeld {
    loaned_payload: Arc<parking_lot::Mutex<LoanedPayloadHeldForPython>>,
}

impl LoanExportHeld {
    fn counting(loaned_payload: &Arc<parking_lot::Mutex<LoanedPayloadHeldForPython>>) -> Self {
        loaned_payload.lock().live_export_count += 1;
        Self {
            loaned_payload: Arc::clone(loaned_payload),
        }
    }
}

impl Drop for LoanExportHeld {
    fn drop(&mut self) {
        self.loaned_payload.lock().count_one_export_released();
    }
}

impl PythonPortPayloadLoan {
    /// Loan `payload_byte_len` bytes on `output_port` through `output_writer`,
    /// refused while the port holds another loan. With no writer, or a payload
    /// past the port's ceiling, which is warned about, the loan is scratch bytes
    /// that publish nowhere, as a write would drop them.
    pub(crate) fn loaning(
        python: Python<'_>,
        output_writer: Option<&Arc<OutputWriterInner>>,
        output_port: &str,
        payload_byte_len: usize,
        timestamp_ns: Option<i64>,
        group_boundary: MessageGroupBoundary,
    ) -> PyResult<Self> {
        let Some(output_writer) = output_writer else {
            return Ok(Self::over(
                None,
                output_port,
                LoanedPayloadBytes::ScratchPublishedNowhere(vec![0u8; payload_byte_len]),
                timestamp_ns,
                group_boundary,
            ));
        };
        let loaned_bytes = match python
            .detach(|| output_writer.loan_output_port_payload(output_port, payload_byte_len))
        {
            Ok(loan) => LoanedPayloadBytes::EngineLoan(loan),
            Err(refusal @ Error::PayloadExceedsChannelCeiling { .. }) => {
                tracing::warn!(
                    "output port {output_port:?}: {refusal}; the loan publishes nowhere"
                );
                LoanedPayloadBytes::ScratchPublishedNowhere(vec![0u8; payload_byte_len])
            }
            Err(refusal) => return Err(PyRuntimeError::new_err(refusal.to_string())),
        };
        Ok(Self::over(
            Some(output_writer),
            output_port,
            loaned_bytes,
            timestamp_ns,
            group_boundary,
        ))
    }

    fn over(
        output_writer: Option<&Arc<OutputWriterInner>>,
        output_port: &str,
        mut loaned_bytes: LoanedPayloadBytes,
        timestamp_ns: Option<i64>,
        group_boundary: MessageGroupBoundary,
    ) -> Self {
        let payload_bytes = loaned_bytes.bytes_mut();
        let payload_base_address = payload_bytes.as_mut_ptr() as usize;
        let payload_byte_len = payload_bytes.len();
        Self {
            output_writer: output_writer.map(Arc::clone),
            output_port: output_port.to_string(),
            timestamp_ns,
            group_boundary,
            loaned_payload: Arc::new(parking_lot::Mutex::new(LoanedPayloadHeldForPython {
                loaned_bytes: Some(loaned_bytes),
                live_export_count: 0,
                given_back_once_unexported: false,
            })),
            payload_base_address,
            payload_byte_len,
        }
    }

    fn refuse_once_the_loan_closed(&self) -> PyResult<()> {
        if self.loaned_payload.lock().is_closed() {
            return Err(PyBufferError::new_err(format!(
                "the loan on output port {:?} closed when its block exited",
                self.output_port
            )));
        }
        Ok(())
    }

    /// Publish the loan, or give it back when an export of it still lives.
    fn close_publishing(&self, python: Python<'_>) -> PyResult<()> {
        let mut held = self.loaned_payload.lock();
        if held.is_closed() {
            return Ok(());
        }
        let live_export_count = held.live_export_count;
        if live_export_count > 0 {
            held.give_back_once_unexported();
            drop(held);
            return Err(PyBufferError::new_err(format!(
                "the loan on output port {:?} still has {live_export_count} live export(s) (a \
                 memoryview or DLPack tensor); release them inside the block. Nothing was \
                 published, and the sample goes back once they are gone",
                self.output_port
            )));
        }
        let Some(loaned_bytes) = held.loaned_bytes.take() else {
            return Ok(());
        };
        drop(held);
        let (LoanedPayloadBytes::EngineLoan(loan), Some(output_writer)) =
            (loaned_bytes, &self.output_writer)
        else {
            return Ok(());
        };
        let timestamp_ns = self
            .timestamp_ns
            .unwrap_or_else(crate::python_helper_stream_clock::helper_stream_clock_now_ns);
        python
            .detach(|| {
                output_writer.publish_output_port_payload_loan(
                    loan,
                    timestamp_ns,
                    self.group_boundary,
                )
            })
            .map_err(|refusal| PyRuntimeError::new_err(refusal.to_string()))
    }

    /// Give the loan back unpublished once no export of it lives.
    fn close_giving_back(&self) {
        self.loaned_payload.lock().give_back_once_unexported();
    }
}

#[pymethods]
impl PythonPortPayloadLoan {
    /// The loan, for its block to fill.
    fn __enter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    /// Publish on a clean exit; give the sample back when the block raised.
    #[pyo3(signature = (exception_type = None, exception = None, traceback = None))]
    fn __exit__(
        &self,
        python: Python<'_>,
        exception_type: Option<&Bound<'_, PyAny>>,
        exception: Option<&Bound<'_, PyAny>>,
        traceback: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        let _ = (exception, traceback);
        match exception_type {
            Some(exception_type) if !exception_type.is_none() => {
                self.close_giving_back();
                Ok(false)
            }
            _ => self.close_publishing(python).map(|()| false),
        }
    }

    /// The loaned payload's length in bytes.
    fn __len__(&self) -> usize {
        self.payload_byte_len
    }

    /// The loaned payload as a writable buffer over the sample.
    ///
    /// # Safety
    ///
    /// CPython calls this with a valid `view` to fill.
    unsafe fn __getbuffer__(
        slf: Bound<'_, Self>,
        view: *mut pyo3::ffi::Py_buffer,
        flags: c_int,
    ) -> PyResult<()> {
        let loan = slf.get();
        loan.refuse_once_the_loan_closed()?;
        // SAFETY: `view` is CPython's to fill; the bytes stay held while
        // `__releasebuffer__` has not run, which `live_export_count` counts.
        unsafe {
            fill_buffer_view_over(
                view,
                slf.as_any(),
                loan.payload_base_address as *mut u8,
                loan.payload_byte_len,
                BufferViewAccess::Writable,
                flags,
            )?;
        }
        loan.loaned_payload.lock().live_export_count += 1;
        Ok(())
    }

    /// Count one buffer export of the loan released.
    ///
    /// # Safety
    ///
    /// CPython calls this once per view `__getbuffer__` filled.
    unsafe fn __releasebuffer__(&self, _view: *mut pyo3::ffi::Py_buffer) {
        self.loaned_payload.lock().count_one_export_released();
    }

    /// The loaned payload as a writable 1-D `uint8` CPU tensor over the
    /// sample, counted as a live export until its consumer lets it go.
    #[pyo3(signature = (*, stream = None, max_version = None, dl_device = None, copy = None))]
    fn __dlpack__<'py>(
        &self,
        python: Python<'py>,
        stream: Option<&Bound<'py, PyAny>>,
        max_version: Option<(u32, u32)>,
        dl_device: Option<(i32, i32)>,
        copy: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        refuse_a_dlpack_request_a_cpu_payload_cannot_meet(stream, dl_device, copy)?;
        self.refuse_once_the_loan_closed()?;
        dlpack_capsule_over(
            python,
            self.payload_base_address as u64,
            PixelExchangeTensorLayout::one_dimensional_bytes(self.payload_byte_len)?,
            HOST_VISIBLE_DLPACK_DEVICE,
            exchange_shape_for_max_version(max_version),
            false,
            Box::new(LoanExportHeld::counting(&self.loaned_payload)),
        )
    }

    /// `(kDLCPU, 0)`: the sample is host memory.
    fn __dlpack_device__(&self) -> (i32, i32) {
        dlpack_device_as_python_pair(HOST_VISIBLE_DLPACK_DEVICE)
    }
}

/// Whether a buffer view may be written through.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BufferViewAccess {
    ReadOnly,
    Writable,
}

/// Fill `view` over `len` bytes at `bytes`, exported by `exporter`.
///
/// # Safety
///
/// `view` is a view CPython handed a `__getbuffer__`, and the bytes stay
/// valid while `exporter` lives.
unsafe fn fill_buffer_view_over(
    view: *mut pyo3::ffi::Py_buffer,
    exporter: &Bound<'_, PyAny>,
    bytes: *mut u8,
    len: usize,
    access: BufferViewAccess,
    flags: c_int,
) -> PyResult<()> {
    let len = pyo3::ffi::Py_ssize_t::try_from(len)
        .map_err(|_| PyBufferError::new_err(format!("a {len}-byte payload is past Py_ssize_t")))?;
    let read_only = c_int::from(access == BufferViewAccess::ReadOnly);
    // SAFETY: the caller's contract; `PyBuffer_FillInfo` takes its own
    // reference to `exporter` into `view.obj` and refuses a writable request
    // of a read-only view with `BufferError`.
    let filled = unsafe {
        pyo3::ffi::PyBuffer_FillInfo(
            view,
            exporter.as_ptr(),
            bytes.cast::<c_void>(),
            len,
            read_only,
            flags,
        )
    };
    if filled == 0 {
        Ok(())
    } else {
        Err(PyErr::fetch(exporter.py()))
    }
}

/// Refuse what a CPU payload cannot honour: a copy, a device other than the
/// CPU, or a stream to order against.
pub(crate) fn refuse_a_dlpack_request_a_cpu_payload_cannot_meet(
    stream: Option<&Bound<'_, PyAny>>,
    dl_device: Option<(i32, i32)>,
    copy: Option<bool>,
) -> PyResult<()> {
    if copy == Some(true) {
        return Err(PyBufferError::new_err(
            "a message payload exports in place; copy the tensor on the consumer's side",
        ));
    }
    if let Some(dl_device) = dl_device
        && dl_device != dlpack_device_as_python_pair(HOST_VISIBLE_DLPACK_DEVICE)
    {
        return Err(PyBufferError::new_err(format!(
            "a message payload is host memory and exports to the CPU only, not device {dl_device:?}"
        )));
    }
    if stream.is_some_and(|stream| !stream.is_none()) {
        return Err(PyBufferError::new_err(
            "a message payload is host memory; there is no stream to order it against",
        ));
    }
    Ok(())
}
