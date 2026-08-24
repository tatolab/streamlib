// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The wheel's window object: a window a Python processor owns.
//!
//! Requested in `setup()` where the capability is Full, named frames from
//! `process()`. The window itself lives in the app process, on the engine's
//! own present loop — only ids and coalesced state cross the hop, and the
//! helper never waits on a vsync.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyMapping, PyString};
use serde::Deserialize;
use serde::de::IntoDeserializer;

use streamlib::sdk::color::{ColorTraits, HdrStaticMetadata, PrimariesId, TransferId};
use streamlib_media_builtins::video_frame::{ColorInfo, ContentLight, MasteringDisplay};

use crate::python_bag_conversion::python_type_name_for_error_message;
use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;
use crate::python_processor_context::PythonGpuSurfaceHandle;

/// The attribute a cast object answers its claimed surface id on — the public
/// door `ClaimedSurfacePixelAccess` offers, so `show()` names the id the claim
/// was taken on rather than re-reading a field the object may have re-pointed.
const THE_CAST_OBJECTS_CLAIMED_SURFACE_ID_ATTRIBUTE: &str = "surface_id_the_claim_was_taken_on";

/// The frame one `show()` names, flattened onto the fields the present-class
/// wire carries.
///
/// Built here rather than in the exchange client so every accepted argument
/// shape converges on one document before any of it reaches the wire.
#[derive(Debug)]
pub(crate) struct SurfaceNamedForTheWindowsPresentLoop {
    pub(crate) surface_id: String,
    /// Zero when the caller named a surface it knows no extent for; the host
    /// reads that as "a buffer-backed surface is not acceptable to me".
    pub(crate) source_width_in_pixels: u32,
    pub(crate) source_height_in_pixels: u32,
    pub(crate) producer_published_texture_layout: Option<i32>,
    /// The engine's own colour types, spelled for the wire at `set_item` time.
    /// Holding them rather than two strings makes this document a mirror of
    /// the host struct it is bound for, and leaves one home for the H.273
    /// collapse.
    pub(crate) color_traits_of_frame: Option<ColorTraits>,
    pub(crate) hdr_static_metadata_of_frame: Option<HdrStaticMetadata>,
}

/// The coalesced state one `drain_events()` took off a window.
///
/// A snapshot, not a queue: the pump coalesces, so an owner that drains once
/// a frame sees the same extent and the same one close request it would have
/// seen draining every microsecond.
#[pyclass(name = "ProcessorOwnedWindowEvents", module = "streamlib", frozen)]
pub(crate) struct PythonProcessorOwnedWindowEvents {
    current_width_in_physical_pixels: u32,
    current_height_in_physical_pixels: u32,
    close_requested_by_user: bool,
    window_is_closed: bool,
}

#[pymethods]
impl PythonProcessorOwnedWindowEvents {
    /// The window's current drawable width in physical pixels.
    #[getter]
    fn current_width_in_physical_pixels(&self) -> u32 {
        self.current_width_in_physical_pixels
    }

    /// The window's current drawable height in physical pixels.
    #[getter]
    fn current_height_in_physical_pixels(&self) -> u32 {
        self.current_height_in_physical_pixels
    }

    /// Whether the user asked to close this window since the last drain.
    ///
    /// True exactly once per gesture — this drain reported it and cleared it.
    /// The engine has already closed the window by the time an owner reads
    /// this, so it is reacted to and never vetoed.
    #[getter]
    fn close_requested_by_user(&self) -> bool {
        self.close_requested_by_user
    }

    /// Whether the engine has closed this window. Sticky once true.
    #[getter]
    fn window_is_closed(&self) -> bool {
        self.window_is_closed
    }

    fn __repr__(&self) -> String {
        format!(
            "ProcessorOwnedWindowEvents({}x{}, close_requested_by_user={}, window_is_closed={})",
            self.current_width_in_physical_pixels,
            self.current_height_in_physical_pixels,
            self.close_requested_by_user,
            self.window_is_closed,
        )
    }
}

/// A window this processor owns, presented by the engine at vsync.
///
/// Constructed in `setup()` through `ctx.gpu_full_access.create_window(...)`;
/// named frames per frame in `process()`. No window handle, swapchain or
/// present thread reaches Python — the object is the handle.
///
/// Defined on every platform so the stub's surface is honest everywhere; off
/// Linux it is unconstructible, because `create_window` refuses before
/// reaching it.
#[pyclass(name = "ProcessorOwnedWindow", module = "streamlib", frozen)]
pub(crate) struct PythonProcessorOwnedWindow {
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    window_id: String,
    window_title: String,
    /// The engine's closed state as the last answered op reported it. Sticky,
    /// like the engine's own: once a window is closed nothing reopens it, so a
    /// cached true is never stale, and it is what lets `show()` stop paying a
    /// round trip for a window that will never draw again.
    ///
    /// `Relaxed` throughout: the flag orders no other memory, and a stronger
    /// ordering would imply a happens-before a reader would look for and not
    /// find.
    window_is_closed: AtomicBool,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    helper_process_exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

#[pymethods]
impl PythonProcessorOwnedWindow {
    /// The title this window was requested with.
    #[getter]
    fn title(&self) -> String {
        self.window_title.clone()
    }

    /// Whether the window has closed — by the user's gesture or this owner's
    /// own `close()`.
    ///
    /// Reflects what the last answered op reported, so it needs no round trip
    /// of its own; `drain_events()` and `show()` are what keep it current.
    #[getter]
    fn is_closed(&self) -> bool {
        self.window_is_closed.load(Ordering::Relaxed)
    }

    /// Name the frame this window shows next.
    ///
    /// Takes anything that names a published surface: a cast object — whose
    /// claim is what guarantees the id un-recycled — a surface handle a kernel
    /// wrote, or a bare surface id. Returns without waiting for the frame to
    /// be shown: the window presents at vsync, latest-wins, and naming nothing
    /// leaves the last frame up.
    ///
    /// A bare id names a **texture-backed** surface only, and so does a cast
    /// type that declares no `width`/`height`: naming no extent is how a
    /// caller says it knows nothing else about the surface, and the engine
    /// reads that as refusing a buffer-backed one. Such a frame does not
    /// draw — the window keeps what it last had, and the engine logs it once
    /// per pool slot rather than raising here. A camera or a test pattern
    /// publishes buffer-backed frames; name those with the cast object.
    ///
    /// A no-op once the window has closed, never an error — a user gesture
    /// does not take a pipeline down. The argument is still read, so a call
    /// that names no surface at all is refused whether the window is open or
    /// shut.
    fn show(
        &self,
        python: Python<'_>,
        frame_or_surface_to_show: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        #[cfg(target_os = "linux")]
        {
            // Built before the closed window is answered, and deliberately:
            // the no-op belongs to the user's gesture, not to an argument that
            // names no surface. Collapsing the two would let a real mistake
            // stop being reported the moment somebody shut the window.
            let named_surface = surface_named_by_the_caller_of_show(frame_or_surface_to_show)?;
            if self.window_is_closed.load(Ordering::Relaxed) {
                return Ok(());
            }
            let window_is_closed = self
                .helper_process_exchange_client
                .show_surface_on_processor_owned_window(python, &self.window_id, &named_surface)?;
            self.window_is_closed
                .store(window_is_closed, Ordering::Relaxed);
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (python, frame_or_surface_to_show);
            Err(window_unreachable_from_a_helper_process_error())
        }
    }

    /// Take this window's coalesced state: its current extent, whether the
    /// user asked to close it since the last drain, and whether it has closed.
    ///
    /// Polling is optional — an owner that never drains still presents; it
    /// only learns of a resize or a close from the next `show()`.
    fn drain_events(&self, python: Python<'_>) -> PyResult<PythonProcessorOwnedWindowEvents> {
        #[cfg(target_os = "linux")]
        {
            let drained = self
                .helper_process_exchange_client
                .drain_processor_owned_window_events(python, &self.window_id)?;
            self.window_is_closed
                .store(drained.window_is_closed, Ordering::Relaxed);
            Ok(drained)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = python;
            Err(window_unreachable_from_a_helper_process_error())
        }
    }

    /// Close this window and release its present thread.
    ///
    /// Never an error for a window already closed, and never required: the
    /// engine closes what a processor still owns at teardown.
    fn close(&self, python: Python<'_>) -> PyResult<()> {
        #[cfg(target_os = "linux")]
        {
            let window_is_closed = self
                .helper_process_exchange_client
                .close_processor_owned_window(python, &self.window_id)?;
            self.window_is_closed
                .store(window_is_closed, Ordering::Relaxed);
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = python;
            Err(window_unreachable_from_a_helper_process_error())
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "ProcessorOwnedWindow(title={:?}, is_closed={})",
            self.window_title,
            self.window_is_closed.load(Ordering::Relaxed),
        )
    }
}

impl PythonProcessorOwnedWindow {
    /// The object handed back to a `setup()` hook whose request was granted.
    #[cfg(target_os = "linux")]
    pub(crate) fn over_the_minted_window(
        window_id: String,
        window_title: String,
        helper_process_exchange_client: Arc<HelperProcessGpuExchangeClient>,
    ) -> Self {
        Self {
            window_id,
            window_title,
            window_is_closed: AtomicBool::new(false),
            helper_process_exchange_client,
        }
    }
}

impl PythonProcessorOwnedWindowEvents {
    /// The drained state, as the exchange client read it off the response.
    pub(crate) fn drained(
        current_width_in_physical_pixels: u32,
        current_height_in_physical_pixels: u32,
        close_requested_by_user: bool,
        window_is_closed: bool,
    ) -> Self {
        Self {
            current_width_in_physical_pixels,
            current_height_in_physical_pixels,
            close_requested_by_user,
            window_is_closed,
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn window_unreachable_from_a_helper_process_error() -> PyErr {
    pyo3::exceptions::PyRuntimeError::new_err(
        "a processor-owned window is not reachable from this platform: the engine's window \
         event pump and its present loop are Linux-only.",
    )
}

/// The frame the caller of `show()` named, whichever of the three shapes they
/// named it in.
pub(crate) fn surface_named_by_the_caller_of_show(
    frame_or_surface_to_show: &Bound<'_, PyAny>,
) -> PyResult<SurfaceNamedForTheWindowsPresentLoop> {
    if let Ok(bare_surface_id) = frame_or_surface_to_show.extract::<String>() {
        return Ok(surface_named_with_nothing_else_known_about_it(
            bare_surface_id,
        ));
    }
    if let Ok(surface_handle) = frame_or_surface_to_show.cast::<PythonGpuSurfaceHandle>() {
        let (surface_id, width, height) = surface_handle
            .borrow()
            .surface_id_and_extent_a_window_can_name()?;
        return Ok(SurfaceNamedForTheWindowsPresentLoop {
            source_width_in_pixels: width,
            source_height_in_pixels: height,
            // A handle names an allocation this helper acquired, never a
            // frame another producer published, so there is no producer
            // layout to override the pool's own default with.
            ..surface_named_with_nothing_else_known_about_it(surface_id)
        });
    }
    surface_named_by_a_cast_object(frame_or_surface_to_show)
}

/// The document a bare surface id amounts to: the id, and nothing else the
/// window could have learned from it.
fn surface_named_with_nothing_else_known_about_it(
    surface_id: String,
) -> SurfaceNamedForTheWindowsPresentLoop {
    SurfaceNamedForTheWindowsPresentLoop {
        surface_id,
        source_width_in_pixels: 0,
        source_height_in_pixels: 0,
        producer_published_texture_layout: None,
        color_traits_of_frame: None,
        hdr_static_metadata_of_frame: None,
    }
}

/// The frame a cast object names: its claimed id, plus whatever of the frame's
/// own description the type carries.
///
/// Read by attribute rather than by type, so a user-authored cast type reaches
/// the window on exactly the terms the shipped `VideoFrame` does. A type that
/// declares no extent names its surface the way a bare id does.
fn surface_named_by_a_cast_object(
    cast_object: &Bound<'_, PyAny>,
) -> PyResult<SurfaceNamedForTheWindowsPresentLoop> {
    let python = cast_object.py();
    // Only a missing attribute means "this is not one of the three shapes". A
    // cast type that has the attribute and refuses — one over several
    // surfaces, one no typed read built — is answering with a refusal of its
    // own, which says more than this one could, and `getattr_opt` propagates
    // it rather than folding it into absence.
    let Some(claimed_surface_id) = cast_object.getattr_opt(intern!(
        python,
        THE_CAST_OBJECTS_CLAIMED_SURFACE_ID_ATTRIBUTE
    ))?
    else {
        return Err(nothing_about_this_names_a_published_surface_error(
            cast_object,
        ));
    };
    let surface_id: String = claimed_surface_id.extract().map_err(|_| {
        PyTypeError::new_err(format!(
            "show() was given an argument of type {} whose claim names no surface, so there is \
             nothing for the window to present. A cast type declares the field its surface id \
             arrives in, and that field held {}",
            python_type_name_for_error_message(cast_object, "object"),
            python_repr_for_error_message(&claimed_surface_id),
        ))
    })?;

    let color_info = optional_attribute(cast_object, intern!(python, "color_info"))?;
    let mastering_display = optional_attribute(cast_object, intern!(python, "mastering_display"))?;
    let content_light = optional_attribute(cast_object, intern!(python, "content_light"))?;

    Ok(SurfaceNamedForTheWindowsPresentLoop {
        surface_id,
        source_width_in_pixels: optional_pixel_extent(cast_object, intern!(python, "width"))?,
        source_height_in_pixels: optional_pixel_extent(cast_object, intern!(python, "height"))?,
        producer_published_texture_layout: optional_attribute(
            cast_object,
            intern!(python, "texture_layout"),
        )?
        .map(|texture_layout| extract_named_field(&texture_layout, "texture_layout"))
        .transpose()?,
        color_traits_of_frame: color_info
            .map(|color_info| color_traits_named_by(&color_info))
            .transpose()?
            .flatten(),
        hdr_static_metadata_of_frame: mastering_display
            .map(|mastering_display| {
                hdr_static_metadata_named_by(&mastering_display, content_light.as_ref())
            })
            .transpose()?,
    })
}

/// The frame's colour description as the engine's own pair, or `None` when the
/// type carries a `color_info` that describes neither axis.
///
/// Either axis alone is a description — the seam resolves the absent one — but
/// both empty is not: answering `Some` there would renegotiate the window's
/// swapchain to the default pick on every frame.
fn color_traits_named_by(color_info: &Bound<'_, PyAny>) -> PyResult<Option<ColorTraits>> {
    let python = color_info.py();
    let primaries: Option<String> = optional_attribute(color_info, intern!(python, "primaries"))?
        .map(|primaries| extract_named_field(&primaries, "color_info.primaries"))
        .transpose()?;
    let transfer: Option<String> = optional_attribute(color_info, intern!(python, "transfer"))?
        .map(|transfer| extract_named_field(&transfer, "color_info.transfer"))
        .transpose()?;
    if primaries.is_none() && transfer.is_none() {
        return Ok(None);
    }
    // Through the bag's own H.273 vocabulary rather than a second reading of
    // it: the sixteen transfer characteristics collapse onto the five the
    // swapchain pick distinguishes, and that collapse has one home.
    Ok(Some(
        ColorInfo {
            primaries: primaries
                .map(|primaries| bag_color_axis_named(&primaries, "primaries"))
                .transpose()?,
            transfer: transfer
                .map(|transfer| bag_color_axis_named(&transfer, "transfer"))
                .transpose()?,
            matrix: None,
            range: None,
        }
        .engine_color_traits(),
    ))
}

/// One axis of the bag's H.273 vocabulary, parsed by the same names the bag
/// carries on the wire.
fn bag_color_axis_named<Axis: for<'de> Deserialize<'de>>(
    axis_value: &str,
    axis_name: &str,
) -> PyResult<Axis> {
    Axis::deserialize(IntoDeserializer::<serde::de::value::Error>::into_deserializer(axis_value))
        .map_err(|_| {
            PyValueError::new_err(format!(
                "show() was given a frame whose color_info.{axis_name} is {axis_value:?}, which is \
             not an H.273 {axis_name} name the bag carries"
            ))
        })
}

/// The wire spelling of an engine primaries id.
///
/// The inverse of [`EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries`]'s
/// serde renames, which this crate cannot reference — the engine takes only its
/// own ids in a public signature and its wire enums are private to it, so a
/// consumer owns this spelling at its own call site. An engine-side *rename*
/// would drift silently; an engine-side *added variant* is a compile error here.
///
/// [`EscalateRequestShowSurfaceOnProcessorOwnedWindowColorPrimaries`]: https://docs.rs/streamlib-engine
pub(crate) fn wire_name_of_primaries(primaries: PrimariesId) -> &'static str {
    match primaries {
        PrimariesId::Bt709 => "bt709",
        PrimariesId::Bt470M => "bt470_m",
        PrimariesId::Bt470Bg => "bt470_bg",
        PrimariesId::Smpte170m => "smpte170m",
        PrimariesId::Smpte240m => "smpte240m",
        PrimariesId::Film => "film",
        PrimariesId::Bt2020 => "bt2020",
        PrimariesId::Smpte428 => "smpte428",
        PrimariesId::Smpte431 => "smpte431",
        PrimariesId::Smpte432 => "smpte432",
        PrimariesId::Ebu3213 => "ebu3213",
    }
}

/// The wire spelling of an engine transfer id, on the same terms as
/// [`wire_name_of_primaries`].
pub(crate) fn wire_name_of_transfer(transfer: TransferId) -> &'static str {
    match transfer {
        TransferId::Linear => "linear",
        TransferId::Srgb => "srgb",
        TransferId::Bt709 => "bt709",
        TransferId::Pq => "pq",
        TransferId::Hlg => "hlg",
    }
}

/// The frame's HDR sidecar, translated out of the bag's integers by the one
/// map that owns that translation.
fn hdr_static_metadata_named_by(
    mastering_display: &Bound<'_, PyAny>,
    content_light: Option<&Bound<'_, PyAny>>,
) -> PyResult<HdrStaticMetadata> {
    let python = mastering_display.py();
    let content_light = content_light
        .map(|content_light| {
            Ok::<_, PyErr>(ContentLight {
                max_cll: required_whole_number(
                    content_light,
                    "content_light",
                    intern!(python, "max_cll"),
                )?,
                max_fall: required_whole_number(
                    content_light,
                    "content_light",
                    intern!(python, "max_fall"),
                )?,
            })
        })
        .transpose()?;
    // Interned: ten reads per HDR frame on the per-frame path, and a plain
    // `&str` allocates a Python string for each one.
    let increments = |field_name: &Bound<'_, PyString>| {
        required_whole_number(mastering_display, "mastering_display", field_name)
    };
    Ok(MasteringDisplay {
        display_primaries_r_x: increments(intern!(python, "display_primaries_r_x"))?,
        display_primaries_r_y: increments(intern!(python, "display_primaries_r_y"))?,
        display_primaries_g_x: increments(intern!(python, "display_primaries_g_x"))?,
        display_primaries_g_y: increments(intern!(python, "display_primaries_g_y"))?,
        display_primaries_b_x: increments(intern!(python, "display_primaries_b_x"))?,
        display_primaries_b_y: increments(intern!(python, "display_primaries_b_y"))?,
        white_point_x: increments(intern!(python, "white_point_x"))?,
        white_point_y: increments(intern!(python, "white_point_y"))?,
        max_luminance: increments(intern!(python, "max_luminance"))?,
        min_luminance: increments(intern!(python, "min_luminance"))?,
    }
    .engine_hdr_static_metadata(content_light.as_ref()))
}

/// One field a colour sidecar must carry, refused by name when it does not.
///
/// Absent is the only refusal this writes: a field that is there and raises is
/// the described object's own account of itself, and folding that into "carries
/// no {field}" would state something false about the caller's object.
fn required_whole_number(
    sidecar: &Bound<'_, PyAny>,
    sidecar_name: &str,
    field_name: &Bound<'_, PyString>,
) -> PyResult<u32> {
    let Some(value) = optional_attribute(sidecar, field_name)? else {
        return Err(PyTypeError::new_err(format!(
            "show() was given a frame whose {sidecar_name} carries no {field_name}; a \
             {sidecar_name} is its whole tuple or it is absent"
        )));
    };
    extract_named_field(&value, &format!("{sidecar_name}.{field_name}"))
}

/// One optional part of a frame's description: absent and `None` are the same
/// answer, because the bag treats an unset key and an unset value alike.
///
/// Read as a mapping key when the described thing is a mapping, and as an
/// attribute otherwise. A bag is a dict and reading it directly is always
/// enough — `VideoFrame` casts its nested metadata, a hand-authored cast type
/// need not — so a nested description left as the dict it arrived as has to
/// reach the window. Folding it into "absent" would present the frame in the
/// default colourspace with no refusal anywhere, which is the one quiet
/// failure this module would otherwise have.
fn optional_attribute<'py>(
    described: &Bound<'py, PyAny>,
    attribute_name: &Bound<'py, PyString>,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let named = match described.cast::<PyMapping>() {
        Ok(mapping) => mapping.get_item(attribute_name).ok(),
        Err(_) => described.getattr_opt(attribute_name)?,
    };
    Ok(named.filter(|value| !value.is_none()))
}

/// One of the frame's own pixel dimensions, or zero when the type declares
/// none — the same "nothing else is known about this surface" a bare id names.
fn optional_pixel_extent(
    cast_object: &Bound<'_, PyAny>,
    attribute_name: &Bound<'_, PyString>,
) -> PyResult<u32> {
    match optional_attribute(cast_object, attribute_name)? {
        Some(extent) => extract_named_field(&extent, &attribute_name.to_string_lossy()),
        None => Ok(0),
    }
}

/// A frame field extracted with its own name in the refusal, so a malformed
/// description says which part of it the window could not read.
fn extract_named_field<'a, 'py, Field: FromPyObject<'a, 'py>>(
    value: &'a Bound<'py, PyAny>,
    field_name: &str,
) -> PyResult<Field> {
    value.extract().map_err(|_| {
        PyTypeError::new_err(format!(
            "show() was given a frame whose {field_name} is {}, which the window cannot read",
            python_repr_for_error_message(value)
        ))
    })
}

fn nothing_about_this_names_a_published_surface_error(named: &Bound<'_, PyAny>) -> PyErr {
    PyTypeError::new_err(format!(
        "show() was given an argument of type {}, and nothing about it names a published \
         surface. It takes a cast object read with `ctx.inputs.read(port, into=T)` — whose claim \
         is what holds the frame still — a GpuSurfaceHandle a kernel wrote, or a bare surface id \
         string.",
        python_type_name_for_error_message(named, "object")
    ))
}

/// A value as it reads in a refusal, or a stand-in when even its `repr` raised.
fn python_repr_for_error_message(value: &Bound<'_, PyAny>) -> String {
    value.repr().map_or_else(
        |_| "a value that cannot be shown here".to_string(),
        |repr| repr.to_string(),
    )
}

/// The whole of what `show()` reads off its argument, proven without a GPU.
///
/// The wire this feeds is exercised end-to-end on the rig; what runs
/// everywhere is the translation — which id a frame names, which of the H.273
/// transfer characteristics collapse onto which of the five the swapchain pick
/// distinguishes, and how the ST.2086 integers scale. A mistake in any of
/// those shows a correct-looking window in the wrong colours, which no
/// assertion on the wire's shape would catch.
#[cfg(test)]
mod tests {
    use super::*;

    use crate::python_class_from_source_for_tests::class_from_source;

    /// A stand-in for the object a typed read hands back: `show()` reads a
    /// frame by attribute, so anything carrying the attributes reaches it on
    /// the same terms the shipped `VideoFrame` does — which is the point.
    const A_CAST_OBJECT_OVER_A_DESCRIBED_FRAME: &str = "
class Frame:
    surface_id_the_claim_was_taken_on = 'slot-4#11'
    surface_id = 'the-field-this-object-has-since-re-pointed'
    width = 1920
    height = 1080
    texture_layout = 1000001002

    class color_info:
        primaries = 'bt2020'
        transfer = 'smpte2084'

    class mastering_display:
        display_primaries_r_x = 35_400
        display_primaries_r_y = 14_600
        display_primaries_g_x = 8_500
        display_primaries_g_y = 39_850
        display_primaries_b_x = 6_550
        display_primaries_b_y = 2_300
        white_point_x = 15_635
        white_point_y = 16_450
        max_luminance = 10_000_000
        min_luminance = 50

    class content_light:
        max_cll = 1_000
        max_fall = 400
";

    fn named_by(
        python: Python<'_>,
        source: &str,
    ) -> PyResult<SurfaceNamedForTheWindowsPresentLoop> {
        let frame = class_from_source(python, source, "Frame").call0().unwrap();
        surface_named_by_the_caller_of_show(&frame)
    }

    fn wire_colour_of(
        named: &SurfaceNamedForTheWindowsPresentLoop,
    ) -> (Option<&'static str>, Option<&'static str>) {
        match named.color_traits_of_frame {
            None => (None, None),
            Some(color_traits) => (
                color_traits.primaries.map(wire_name_of_primaries),
                color_traits.transfer.map(wire_name_of_transfer),
            ),
        }
    }

    fn named_by_a_frame_whose_transfer_is(
        python: Python<'_>,
        h273_transfer_name: &str,
    ) -> Option<&'static str> {
        named_by(
            python,
            &format!(
                "
class Frame:
    surface_id_the_claim_was_taken_on = 'slot-1#1'
    class color_info:
        primaries = None
        transfer = {h273_transfer_name:?}
"
            ),
        )
        .map(|named| wire_colour_of(&named).1)
        .unwrap()
    }

    #[test]
    fn a_bare_surface_id_names_the_surface_and_nothing_else() {
        Python::initialize();
        Python::attach(|python| {
            let named =
                surface_named_by_the_caller_of_show(&"slot-9#3".into_pyobject(python).unwrap())
                    .unwrap();

            assert_eq!(named.surface_id, "slot-9#3");
            assert_eq!(
                (named.source_width_in_pixels, named.source_height_in_pixels),
                (0, 0),
                "a caller who named only an id knows no extent, and zero is how the host reads \
                 that"
            );
            assert_eq!(wire_colour_of(&named), (None, None));
            assert!(named.hdr_static_metadata_of_frame.is_none());
            assert!(named.producer_published_texture_layout.is_none());
        });
    }

    #[test]
    fn a_cast_object_names_the_surface_its_claim_was_taken_on() {
        Python::initialize();
        Python::attach(|python| {
            let named = named_by(python, A_CAST_OBJECT_OVER_A_DESCRIBED_FRAME).unwrap();

            assert_eq!(
                named.surface_id, "slot-4#11",
                "the claim is what holds the frame still, so the id it was taken on is the only \
                 one safe to show — never a declared field the object has since re-pointed"
            );
        });
    }

    #[test]
    fn a_cast_objects_extent_and_published_layout_reach_the_window() {
        Python::initialize();
        Python::attach(|python| {
            let named = named_by(python, A_CAST_OBJECT_OVER_A_DESCRIBED_FRAME).unwrap();

            assert_eq!(
                (named.source_width_in_pixels, named.source_height_in_pixels),
                (1920, 1080)
            );
            assert_eq!(named.producer_published_texture_layout, Some(1_000_001_002));
        });
    }

    #[test]
    fn a_cast_type_that_declares_no_extent_names_its_surface_the_way_a_bare_id_does() {
        Python::initialize();
        Python::attach(|python| {
            let named = named_by(
                python,
                "
class Frame:
    surface_id_the_claim_was_taken_on = 'slot-2#7'
",
            )
            .unwrap();

            assert_eq!(named.surface_id, "slot-2#7");
            assert_eq!(
                (named.source_width_in_pixels, named.source_height_in_pixels),
                (0, 0)
            );
        });
    }

    /// Sixteen H.273 transfer characteristics, five the swapchain pick can
    /// tell apart. Getting PQ or HLG wrong here is the difference between an
    /// HDR window and a washed-out one.
    #[test]
    fn the_h273_transfer_vocabulary_collapses_onto_what_the_swapchain_pick_reads() {
        Python::initialize();
        Python::attach(|python| {
            assert_eq!(
                named_by_a_frame_whose_transfer_is(python, "smpte2084"),
                Some("pq")
            );
            assert_eq!(
                named_by_a_frame_whose_transfer_is(python, "arib_std_b67"),
                Some("hlg")
            );
            assert_eq!(
                named_by_a_frame_whose_transfer_is(python, "srgb"),
                Some("srgb")
            );
            assert_eq!(
                named_by_a_frame_whose_transfer_is(python, "linear"),
                Some("linear")
            );
            assert_eq!(
                named_by_a_frame_whose_transfer_is(python, "gamma22"),
                Some("bt709"),
                "a transfer the engine has no id for takes the BT.709-shaped approximation, \
                 never Linear, which would skip decoding entirely"
            );
            assert_eq!(
                named_by_a_frame_whose_transfer_is(python, "bt2020_ten_bit"),
                Some("bt709")
            );
        });
    }

    #[test]
    fn a_frames_primaries_reach_the_window_by_their_own_name() {
        Python::initialize();
        Python::attach(|python| {
            let named = named_by(python, A_CAST_OBJECT_OVER_A_DESCRIBED_FRAME).unwrap();

            assert_eq!(wire_colour_of(&named).0, Some("bt2020"));
        });
    }

    /// Either axis alone is a description — the seam resolves the absent one —
    /// so one present must never drag the other onto the wire.
    #[test]
    fn one_colour_axis_alone_is_a_description() {
        Python::initialize();
        Python::attach(|python| {
            let primaries_only = named_by(
                python,
                "
class Frame:
    surface_id_the_claim_was_taken_on = 'slot-3#2'
    class color_info:
        primaries = 'bt709'
        transfer = None
",
            )
            .unwrap();

            assert_eq!(wire_colour_of(&primaries_only), (Some("bt709"), None));
        });
    }

    #[test]
    fn a_frame_carrying_no_colour_info_describes_neither_axis() {
        Python::initialize();
        Python::attach(|python| {
            let named = named_by(
                python,
                "
class Frame:
    surface_id_the_claim_was_taken_on = 'slot-5#1'
    color_info = None
",
            )
            .unwrap();

            assert_eq!(wire_colour_of(&named), (None, None));
        });
    }

    /// Both axes empty is not a description: answering `Some` there would
    /// renegotiate the window's swapchain to the default pick every frame.
    #[test]
    fn a_colour_description_naming_neither_axis_describes_nothing() {
        Python::initialize();
        Python::attach(|python| {
            let named = named_by(
                python,
                "
class Frame:
    surface_id_the_claim_was_taken_on = 'slot-9#1'
    class color_info:
        primaries = None
        transfer = None
",
            )
            .unwrap();

            assert!(named.color_traits_of_frame.is_none());
        });
    }

    /// The eleven spellings the engine renames its wire primaries to. The
    /// engine's own half is pinned by its golden vector; this is the half
    /// living in this crate, which nothing else would catch.
    #[test]
    fn every_primaries_id_keeps_its_wire_spelling() {
        assert_eq!(
            [
                PrimariesId::Bt709,
                PrimariesId::Bt470M,
                PrimariesId::Bt470Bg,
                PrimariesId::Smpte170m,
                PrimariesId::Smpte240m,
                PrimariesId::Film,
                PrimariesId::Bt2020,
                PrimariesId::Smpte428,
                PrimariesId::Smpte431,
                PrimariesId::Smpte432,
                PrimariesId::Ebu3213,
            ]
            .map(wire_name_of_primaries),
            [
                "bt709",
                "bt470_m",
                "bt470_bg",
                "smpte170m",
                "smpte240m",
                "film",
                "bt2020",
                "smpte428",
                "smpte431",
                "smpte432",
                "ebu3213",
            ]
        );
    }

    #[test]
    fn every_transfer_id_keeps_its_wire_spelling() {
        assert_eq!(
            [
                TransferId::Linear,
                TransferId::Srgb,
                TransferId::Bt709,
                TransferId::Pq,
                TransferId::Hlg,
            ]
            .map(wire_name_of_transfer),
            ["linear", "srgb", "bt709", "pq", "hlg"]
        );
    }

    #[test]
    fn the_hdr_sidecars_integers_scale_into_the_units_the_driver_takes() {
        Python::initialize();
        Python::attach(|python| {
            let named = named_by(python, A_CAST_OBJECT_OVER_A_DESCRIBED_FRAME).unwrap();

            let sidecar = named
                .hdr_static_metadata_of_frame
                .expect("a frame carrying a mastering display names its metadata");
            assert!((sidecar.display_primary_red[0] - 0.708).abs() < 1e-6);
            assert!((sidecar.max_luminance_cd_m2 - 1_000.0).abs() < 1e-3);
            assert!((sidecar.min_luminance_cd_m2 - 0.005).abs() < 1e-6);
            assert_eq!(sidecar.max_content_light_level, 1_000.0);
            assert_eq!(sidecar.max_frame_average_light_level, 400.0);
        });
    }

    /// Content light describes a mastering display; without one there is no
    /// sidecar to attach it to, and the built-in display reads a bag the same
    /// way.
    #[test]
    fn content_light_without_a_mastering_display_names_no_sidecar() {
        Python::initialize();
        Python::attach(|python| {
            let named = named_by(
                python,
                "
class Frame:
    surface_id_the_claim_was_taken_on = 'slot-6#4'
    mastering_display = None
    class content_light:
        max_cll = 600
        max_fall = 200
",
            )
            .unwrap();

            assert!(named.hdr_static_metadata_of_frame.is_none());
        });
    }

    #[test]
    fn a_mastering_display_without_content_light_still_describes_a_display() {
        Python::initialize();
        Python::attach(|python| {
            let named = named_by(
                python,
                "
class Frame:
    surface_id_the_claim_was_taken_on = 'slot-7#4'
    class mastering_display:
        display_primaries_r_x = 35_400
        display_primaries_r_y = 14_600
        display_primaries_g_x = 8_500
        display_primaries_g_y = 39_850
        display_primaries_b_x = 6_550
        display_primaries_b_y = 2_300
        white_point_x = 15_635
        white_point_y = 16_450
        max_luminance = 10_000_000
        min_luminance = 50
",
            )
            .unwrap();

            let sidecar = named
                .hdr_static_metadata_of_frame
                .expect("a mastering display is a description on its own");
            assert_eq!(sidecar.max_content_light_level, 0.0);
            assert_eq!(sidecar.max_frame_average_light_level, 0.0);
        });
    }

    /// A sidecar field that is *there* and raises is the described object's
    /// own account of itself. Folding that into "carries no max_luminance"
    /// would state something false about the caller's object — the same rule
    /// the claimed-id read follows.
    #[test]
    fn a_sidecar_field_that_raises_reaches_the_caller_rather_than_a_carries_no_refusal() {
        Python::initialize();
        Python::attach(|python| {
            let refusal = named_by(
                python,
                // An instance, because a property only fires on one — the
                // sidecar a real cast type carries is an object, not a class.
                "
class MasteringDisplayThatCannotReportItsPeak:
    display_primaries_r_x = 35_400
    display_primaries_r_y = 14_600
    display_primaries_g_x = 8_500
    display_primaries_g_y = 39_850
    display_primaries_b_x = 6_550
    display_primaries_b_y = 2_300
    white_point_x = 15_635
    white_point_y = 16_450
    min_luminance = 50

    @property
    def max_luminance(self):
        raise ValueError('this display never measured its peak luminance')

class Frame:
    surface_id_the_claim_was_taken_on = 'slot-10#1'
    mastering_display = MasteringDisplayThatCannotReportItsPeak()
",
            )
            .expect_err("the mastering display refused");

            let refusal = refusal.to_string();
            assert!(
                refusal.contains("never measured its peak luminance"),
                "the object's own refusal must reach the caller, got: {refusal}"
            );
            assert!(
                !refusal.contains("carries no max_luminance"),
                "a field that is present and raises is not a missing field, got: {refusal}"
            );
        });
    }

    /// A sidecar genuinely missing a field is refused by name, on both
    /// sidecars — the ten-tuple and the content-light pair alike.
    #[test]
    fn a_sidecar_missing_a_field_is_refused_by_that_fields_name() {
        Python::initialize();
        Python::attach(|python| {
            let refusal = named_by(
                python,
                "
class Frame:
    surface_id_the_claim_was_taken_on = 'slot-11#1'
    class mastering_display:
        display_primaries_r_x = 35_400
    class content_light:
        max_cll = 1_000
",
            )
            .expect_err("an incomplete mastering display describes no display");

            let refusal = refusal.to_string();
            assert!(
                refusal.contains("content_light carries no max_fall"),
                "the content-light pair is named the same way the ten-tuple is, got: {refusal}"
            );
        });
    }

    /// A bag is a dict, and a cast type may keep its nested description as the
    /// dict it arrived as. Reading that as "absent" would present the frame in
    /// the default colourspace with no refusal anywhere — the one silent
    /// wrongness this builder could have had.
    #[test]
    fn a_nested_description_left_as_the_dict_it_arrived_as_still_describes_the_frame() {
        Python::initialize();
        Python::attach(|python| {
            let named = named_by(
                python,
                "
class Frame:
    surface_id_the_claim_was_taken_on = 'slot-12#1'
    width = 1920
    height = 1080
    color_info = {'primaries': 'bt2020', 'transfer': 'smpte2084'}
    mastering_display = {
        'display_primaries_r_x': 35_400, 'display_primaries_r_y': 14_600,
        'display_primaries_g_x': 8_500, 'display_primaries_g_y': 39_850,
        'display_primaries_b_x': 6_550, 'display_primaries_b_y': 2_300,
        'white_point_x': 15_635, 'white_point_y': 16_450,
        'max_luminance': 10_000_000, 'min_luminance': 50,
    }
    content_light = {'max_cll': 1_000, 'max_fall': 400}
",
            )
            .unwrap();

            assert_eq!(wire_colour_of(&named), (Some("bt2020"), Some("pq")));
            let sidecar = named
                .hdr_static_metadata_of_frame
                .expect("a mapping describes a mastering display as well as an object does");
            assert!((sidecar.display_primary_red[0] - 0.708).abs() < 1e-6);
            assert_eq!(sidecar.max_content_light_level, 1_000.0);
        });
    }

    #[test]
    fn an_object_that_names_no_published_surface_is_refused_by_the_three_shapes() {
        Python::initialize();
        Python::attach(|python| {
            let refusal = named_by(
                python,
                "
class Frame:
    width = 640
",
            )
            .expect_err("nothing here names a surface");

            let refusal = refusal.to_string();
            assert!(
                refusal.contains("read(port, into=T)")
                    && refusal.contains("GpuSurfaceHandle")
                    && refusal.contains("surface id"),
                "the refusal names all three shapes a caller could have used, got: {refusal}"
            );
        });
    }

    /// A cast type that declares a field no bag filled claims nothing; saying
    /// so beats naming `None` to a present loop that would log an unresolvable
    /// id once per pool slot and keep the last frame up.
    #[test]
    fn a_cast_object_whose_claim_names_no_surface_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let refusal = named_by(
                python,
                "
class Frame:
    surface_id_the_claim_was_taken_on = None
",
            )
            .expect_err("a claim over nothing has no pixels to show");

            assert!(
                refusal.to_string().contains("names no surface"),
                "got: {refusal}"
            );
        });
    }

    #[test]
    fn a_colour_axis_the_bag_vocabulary_does_not_carry_is_refused_by_name() {
        Python::initialize();
        Python::attach(|python| {
            let refusal = named_by(
                python,
                "
class Frame:
    surface_id_the_claim_was_taken_on = 'slot-8#1'
    class color_info:
        primaries = 'bt709'
        transfer = 'a-transfer-nobody-standardised'
",
            )
            .expect_err("an unknown transfer name is not a description");

            let refusal = refusal.to_string();
            assert!(
                refusal.contains("color_info.transfer")
                    && refusal.contains("a-transfer-nobody-standardised"),
                "the refusal names the axis and the value, got: {refusal}"
            );
        });
    }

    /// A refusal a cast object raises for itself — over several surfaces, or
    /// built by no typed read — says more than "this is not one of the three
    /// shapes" could, so it must reach the caller intact.
    #[test]
    fn a_cast_objects_own_refusal_reaches_the_caller_rather_than_the_three_shapes_one() {
        Python::initialize();
        Python::attach(|python| {
            let refusal = named_by(
                python,
                "
class Frame:
    @property
    def surface_id_the_claim_was_taken_on(self):
        raise RuntimeError('a Frame claims 2 surfaces, so a bare view would have to guess')
",
            )
            .expect_err("the cast object refused");

            assert!(
                refusal.to_string().contains("claims 2 surfaces"),
                "got: {refusal}"
            );
        });
    }
}
