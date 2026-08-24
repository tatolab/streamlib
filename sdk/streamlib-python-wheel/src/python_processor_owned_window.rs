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

use pyo3::exceptions::{PyAttributeError, PyRuntimeError};
use pyo3::prelude::*;

use streamlib::sdk::color::{PrimariesId, TransferId};
use streamlib_media_builtins::video_frame::{ColorInfo, ContentLight, MasteringDisplay};

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
    pub(crate) color_primaries_of_frame: Option<&'static str>,
    pub(crate) color_transfer_of_frame: Option<&'static str>,
    pub(crate) hdr_static_metadata_of_frame: Option<HdrStaticMetadataNamedForTheWindow>,
}

/// The HDR sidecar as the wire carries it — already the f32 units the driver
/// takes, so nothing downstream of here converts.
#[derive(Debug)]
pub(crate) struct HdrStaticMetadataNamedForTheWindow {
    pub(crate) display_primary_red: [f32; 2],
    pub(crate) display_primary_green: [f32; 2],
    pub(crate) display_primary_blue: [f32; 2],
    pub(crate) white_point: [f32; 2],
    pub(crate) min_luminance_cd_m2: f32,
    pub(crate) max_luminance_cd_m2: f32,
    pub(crate) max_content_light_level: f32,
    pub(crate) max_frame_average_light_level: f32,
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
        self.window_is_closed.load(Ordering::Acquire)
    }

    /// Name the frame this window shows next.
    ///
    /// Takes anything that names a published surface: a cast object — whose
    /// claim is what guarantees the id un-recycled — a surface handle a kernel
    /// wrote, or a bare surface id. Returns without waiting for the frame to
    /// be shown: the window presents at vsync, latest-wins, and naming nothing
    /// leaves the last frame up.
    ///
    /// A no-op once the window has closed, never an error — a user gesture
    /// does not take a pipeline down.
    fn show(
        &self,
        python: Python<'_>,
        frame_or_surface_to_show: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        #[cfg(target_os = "linux")]
        {
            if self.window_is_closed.load(Ordering::Acquire) {
                return Ok(());
            }
            let named_surface = surface_named_by_the_caller_of_show(frame_or_surface_to_show)?;
            let window_is_closed = self
                .helper_process_exchange_client
                .show_surface_on_processor_owned_window(python, &self.window_id, &named_surface)?;
            self.window_is_closed
                .store(window_is_closed, Ordering::Release);
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
                .store(drained.window_is_closed, Ordering::Release);
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
                .store(window_is_closed, Ordering::Release);
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
            self.window_is_closed.load(Ordering::Acquire),
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
    PyRuntimeError::new_err(
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
            surface_id,
            source_width_in_pixels: width,
            source_height_in_pixels: height,
            // A handle names an allocation this helper acquired, never a
            // frame another producer published, so there is no producer
            // layout to override the pool's own default with.
            producer_published_texture_layout: None,
            color_primaries_of_frame: None,
            color_transfer_of_frame: None,
            hdr_static_metadata_of_frame: None,
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
        color_primaries_of_frame: None,
        color_transfer_of_frame: None,
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
    let claimed_surface_id =
        match cast_object.getattr(THE_CAST_OBJECTS_CLAIMED_SURFACE_ID_ATTRIBUTE) {
            Ok(claimed_surface_id) => claimed_surface_id,
            // Only a missing attribute means "this is not one of the three
            // shapes". A cast type that has the attribute and refuses — one
            // over several surfaces, one no typed read built — is answering
            // with a refusal of its own, which says more than this one could.
            Err(refusal) if refusal.is_instance_of::<PyAttributeError>(python) => {
                return Err(nothing_about_this_names_a_published_surface_error(
                    cast_object,
                ));
            }
            Err(refusal) => return Err(refusal),
        };
    let surface_id: String = claimed_surface_id.extract().map_err(|_| {
        PyRuntimeError::new_err(format!(
            "show() was given a {} whose claim names no surface, so there is nothing for the \
             window to present. A cast type declares the field its surface id arrives in, and \
             that field held {}",
            type_name_of(cast_object),
            claimed_surface_id.repr().map_or_else(
                |_| "a value that cannot be shown here".to_string(),
                |repr| repr.to_string()
            ),
        ))
    })?;

    let color_info = optional_attribute(cast_object, "color_info")?;
    let color_traits = match &color_info {
        Some(color_info) => color_traits_named_by(color_info)?,
        None => (None, None),
    };
    let mastering_display = optional_attribute(cast_object, "mastering_display")?;
    let content_light = optional_attribute(cast_object, "content_light")?;

    Ok(SurfaceNamedForTheWindowsPresentLoop {
        surface_id,
        source_width_in_pixels: optional_pixel_extent(cast_object, "width")?,
        source_height_in_pixels: optional_pixel_extent(cast_object, "height")?,
        producer_published_texture_layout: optional_attribute(cast_object, "texture_layout")?
            .map(|texture_layout| extract_named_field(&texture_layout, "texture_layout"))
            .transpose()?,
        color_primaries_of_frame: color_traits.0,
        color_transfer_of_frame: color_traits.1,
        hdr_static_metadata_of_frame: mastering_display
            .map(|mastering_display| {
                hdr_static_metadata_named_by(&mastering_display, content_light.as_ref())
            })
            .transpose()?,
    })
}

/// The frame's colour description as the wire spells it, both axes.
///
/// Either axis alone is a description — the seam resolves the absent one — so
/// they are read independently and neither implies the other.
fn color_traits_named_by(
    color_info: &Bound<'_, PyAny>,
) -> PyResult<(Option<&'static str>, Option<&'static str>)> {
    let primaries: Option<String> = optional_attribute(color_info, "primaries")?
        .map(|primaries| extract_named_field(&primaries, "color_info.primaries"))
        .transpose()?;
    let transfer: Option<String> = optional_attribute(color_info, "transfer")?
        .map(|transfer| extract_named_field(&transfer, "color_info.transfer"))
        .transpose()?;
    // Through the bag's own H.273 vocabulary rather than a second reading of
    // it: the sixteen transfer characteristics collapse onto the five the
    // swapchain pick distinguishes, and that collapse has one home.
    let color_traits = ColorInfo {
        primaries: primaries
            .map(|primaries| bag_color_axis_named(&primaries, "primaries"))
            .transpose()?,
        transfer: transfer
            .map(|transfer| bag_color_axis_named(&transfer, "transfer"))
            .transpose()?,
        matrix: None,
        range: None,
    }
    .engine_color_traits();
    Ok((
        color_traits.primaries.map(wire_name_of_primaries),
        color_traits.transfer.map(wire_name_of_transfer),
    ))
}

/// One axis of the bag's H.273 vocabulary, parsed by the same names the bag
/// carries on the wire.
fn bag_color_axis_named<Axis: serde::de::DeserializeOwned>(
    axis_value: &str,
    axis_name: &str,
) -> PyResult<Axis> {
    serde_json::from_value(serde_json::Value::String(axis_value.to_string())).map_err(|_| {
        PyRuntimeError::new_err(format!(
            "show() was given a frame whose color_info.{axis_name} is {axis_value:?}, which is \
             not an H.273 {axis_name} name the bag carries"
        ))
    })
}

/// The wire spelling of an engine primaries id. The engine takes only its own
/// ids in a public signature, so naming them on a wire is the caller's job.
fn wire_name_of_primaries(primaries: PrimariesId) -> &'static str {
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

/// The wire spelling of an engine transfer id.
fn wire_name_of_transfer(transfer: TransferId) -> &'static str {
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
) -> PyResult<HdrStaticMetadataNamedForTheWindow> {
    let increments = |field_name: &str| -> PyResult<u32> {
        extract_named_field(
            &mastering_display.getattr(field_name).map_err(|_| {
                PyRuntimeError::new_err(format!(
                    "show() was given a frame whose mastering_display carries no {field_name}; a \
                     mastering display is the ST.2086 ten-tuple or it is absent"
                ))
            })?,
            field_name,
        )
    };
    let content_light = content_light
        .map(|content_light| {
            Ok::<_, PyErr>(ContentLight {
                max_cll: extract_named_field(
                    &content_light.getattr("max_cll")?,
                    "content_light.max_cll",
                )?,
                max_fall: extract_named_field(
                    &content_light.getattr("max_fall")?,
                    "content_light.max_fall",
                )?,
            })
        })
        .transpose()?;
    let engine_metadata = MasteringDisplay {
        display_primaries_r_x: increments("display_primaries_r_x")?,
        display_primaries_r_y: increments("display_primaries_r_y")?,
        display_primaries_g_x: increments("display_primaries_g_x")?,
        display_primaries_g_y: increments("display_primaries_g_y")?,
        display_primaries_b_x: increments("display_primaries_b_x")?,
        display_primaries_b_y: increments("display_primaries_b_y")?,
        white_point_x: increments("white_point_x")?,
        white_point_y: increments("white_point_y")?,
        max_luminance: increments("max_luminance")?,
        min_luminance: increments("min_luminance")?,
    }
    .engine_hdr_static_metadata(content_light.as_ref());
    Ok(HdrStaticMetadataNamedForTheWindow {
        display_primary_red: engine_metadata.display_primary_red,
        display_primary_green: engine_metadata.display_primary_green,
        display_primary_blue: engine_metadata.display_primary_blue,
        white_point: engine_metadata.white_point,
        min_luminance_cd_m2: engine_metadata.min_luminance_cd_m2,
        max_luminance_cd_m2: engine_metadata.max_luminance_cd_m2,
        max_content_light_level: engine_metadata.max_content_light_level,
        max_frame_average_light_level: engine_metadata.max_frame_average_light_level,
    })
}

/// One optional part of a frame's description: absent and `None` are the same
/// answer, because the bag treats an unset key and an unset value alike.
fn optional_attribute<'py>(
    described: &Bound<'py, PyAny>,
    attribute_name: &str,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    match described.getattr(attribute_name) {
        Ok(value) if value.is_none() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(refusal) if refusal.is_instance_of::<PyAttributeError>(described.py()) => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// One of the frame's own pixel dimensions, or zero when the type declares
/// none — the same "nothing else is known about this surface" a bare id names.
fn optional_pixel_extent(cast_object: &Bound<'_, PyAny>, attribute_name: &str) -> PyResult<u32> {
    match optional_attribute(cast_object, attribute_name)? {
        Some(extent) => extract_named_field(&extent, attribute_name),
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
        PyRuntimeError::new_err(format!(
            "show() was given a frame whose {field_name} is {}, which the window cannot read",
            value
                .repr()
                .map_or_else(|_| "unreadable".to_string(), |repr| repr.to_string())
        ))
    })
}

fn nothing_about_this_names_a_published_surface_error(named: &Bound<'_, PyAny>) -> PyErr {
    PyRuntimeError::new_err(format!(
        "show() was given a {}, and nothing about it names a published surface. It takes a \
         cast object read with `ctx.inputs.read(port, into=T)` — whose claim is what holds the \
         frame still — a GpuSurfaceHandle a kernel wrote, or a bare surface id string.",
        type_name_of(named)
    ))
}

fn type_name_of(named: &Bound<'_, PyAny>) -> String {
    named
        .get_type()
        .name()
        .map_or_else(|_| "object".to_string(), |name| name.to_string())
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
        .unwrap()
        .color_transfer_of_frame
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
            assert!(named.color_primaries_of_frame.is_none());
            assert!(named.color_transfer_of_frame.is_none());
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

            assert_eq!(named.color_primaries_of_frame, Some("bt2020"));
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

            assert_eq!(primaries_only.color_primaries_of_frame, Some("bt709"));
            assert!(primaries_only.color_transfer_of_frame.is_none());
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

            assert!(named.color_primaries_of_frame.is_none());
            assert!(named.color_transfer_of_frame.is_none());
        });
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
