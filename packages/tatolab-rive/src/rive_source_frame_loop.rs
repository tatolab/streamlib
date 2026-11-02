// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The frame thread: rive-runtime draws each frame straight into a texture
//! from the node's output pool, imported onto a device the consumer RHI
//! created, and a `VideoFrame` naming it is published. No frame is copied.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use streamlib_consumer_rhi::{
    ConsumerVulkanDeviceForExternalRenderer, ExternalRendererRenderTarget,
    runtime_vulkan_device_uuid_named_at_spawn,
};
use streamlib_surface_client::{
    CheckedOutSurface, check_out_surface_on_surface_share, connect_to_surface_share_socket,
    publish_image_layout_on_surface_share, release_check_out_on_surface_share,
};
use tatolab_module_sdk::{
    ModuleHost, ModuleHostInputPort, ModuleHostLogLevel, ModuleHostNodeOutputTextureRequest,
    ModuleHostOutputPort,
};

use crate::rive_scene_bridge::RiveSceneOnExternalRendererDevice;
use crate::video_frame_record::PublishedVideoFrame;
use crate::view_model_data_message::ViewModelDataMessageApplier;

/// The variable a processor interpreter is handed the surface-share socket in.
const SURFACE_SHARE_SOCKET_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_SURFACE_SOCKET";

const RENDERED_FRAME_POOL_KEY: &str = "rive_rendered_frames";
const RENDERED_FRAME_TEXTURE_FORMAT: &str = "rgba8_unorm";
const RENDERED_FRAME_TEXTURE_USAGE: [&str; 2] = ["render_attachment", "texture_binding"];

/// Render targets kept imported, by pool slot; the pool rotates far fewer.
const MOST_RENDER_TARGETS_KEPT: usize = 16;

/// The longest step one frame advances the animation by, so a stall does not
/// jump it.
const LONGEST_ANIMATION_STEP: Duration = Duration::from_millis(250);

/// Frames between two reports of the frame thread's rate and render time.
const FRAMES_BETWEEN_REPORTS: u64 = 600;

const RIVE_LOG_TARGET: &str = "tatolab_rive";

/// What a `RiveSource` renders and how often.
#[derive(Debug, Clone)]
pub struct RiveSourceSettings {
    pub riv_path: String,
    pub artboard: Option<String>,
    pub state_machine: Option<String>,
    pub width: u32,
    pub height: u32,
    pub frame_rate: u32,
    pub output_pool_rotation_depth: u32,
}

/// A `RiveSource` rendering on its own thread until stopped.
pub struct RiveSourceRendering {
    stop_requested: Arc<AtomicBool>,
    frame_thread: Option<JoinHandle<()>>,
}

impl RiveSourceRendering {
    /// Read the `.riv` and open the ports, then build the device and the scene
    /// on a new thread, which publishes on `video_output_port_name` and reads
    /// view-model updates from `data_input_port_name`. The thread logs what it
    /// plays, or why it cannot, to the node's log.
    pub fn start(
        module_host: ModuleHost,
        video_output_port_name: &str,
        data_input_port_name: &str,
        settings: RiveSourceSettings,
    ) -> Result<Self, String> {
        if settings.width == 0 || settings.height == 0 || settings.frame_rate == 0 {
            return Err(format!(
                "RiveSource: width, height and frame_rate must be positive; got {}x{} at {} fps",
                settings.width, settings.height, settings.frame_rate
            ));
        }
        let riv_bytes = std::fs::read(&settings.riv_path).map_err(|read_failure| {
            format!(
                "RiveSource: could not read {:?}: {read_failure}",
                settings.riv_path
            )
        })?;
        let video_output_port = module_host
            .open_output_port(video_output_port_name)
            .map_err(|refusal| refusal.to_string())?;
        let data_input_port = module_host
            .open_input_port(data_input_port_name)
            .map_err(|refusal| refusal.to_string())?;
        let stop_requested = Arc::new(AtomicBool::new(false));
        let frame_thread = {
            let stop_requested = Arc::clone(&stop_requested);
            std::thread::Builder::new()
                .name("tatolab-rive-frames".to_owned())
                .spawn(move || {
                    match RiveSourceFrameLoop::build(
                        module_host.clone(),
                        video_output_port,
                        data_input_port,
                        settings,
                        &riv_bytes,
                    ) {
                        Ok(frame_loop) => {
                            frame_loop.log_what_plays();
                            frame_loop.run_until_stopped(&stop_requested);
                        }
                        Err(refusal) => module_host.log(
                            ModuleHostLogLevel::Error,
                            RIVE_LOG_TARGET,
                            &format!("{refusal}; nothing is published"),
                        ),
                    }
                })
                .map_err(|spawn_failure| {
                    format!("RiveSource: the frame thread could not start: {spawn_failure}")
                })?
        };
        Ok(Self {
            stop_requested,
            frame_thread: Some(frame_thread),
        })
    }

    /// Stop rendering and release the device. Idempotent.
    pub fn stop(&mut self) {
        self.stop_requested.store(true, Ordering::Release);
        if let Some(frame_thread) = self.frame_thread.take() {
            let _ = frame_thread.join();
        }
    }
}

impl Drop for RiveSourceRendering {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The surface id a pool slot's every published frame shares: a published
/// frame id is `<pool slot>#<generation>`.
fn pool_slot_of_a_published_frame_id(surface_id: &str) -> &str {
    match surface_id.rsplit_once('#') {
        Some((pool_slot, generation))
            if !pool_slot.is_empty()
                && !generation.is_empty()
                && generation.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            pool_slot
        }
        _ => surface_id,
    }
}

/// What the frame thread owns. Declared so the render targets drop before
/// the scene, and the scene before the device both live on.
struct RiveSourceFrameLoop {
    render_targets_by_pool_slot: HashMap<String, ExternalRendererRenderTarget>,
    scene: RiveSceneOnExternalRendererDevice,
    device: ConsumerVulkanDeviceForExternalRenderer,
    surface_share_connection: UnixStream,
    module_host: ModuleHost,
    video_output_port: ModuleHostOutputPort,
    data_input_port: ModuleHostInputPort,
    view_model_data_message_applier: ViewModelDataMessageApplier,
    settings: RiveSourceSettings,
}

impl RiveSourceFrameLoop {
    fn build(
        module_host: ModuleHost,
        video_output_port: ModuleHostOutputPort,
        data_input_port: ModuleHostInputPort,
        settings: RiveSourceSettings,
        riv_bytes: &[u8],
    ) -> Result<Self, String> {
        let runtime_vulkan_device_uuid = runtime_vulkan_device_uuid_named_at_spawn()
            .map_err(|refusal| format!("RiveSource: {refusal}"))?;
        let device = ConsumerVulkanDeviceForExternalRenderer::new_on_the_device_the_runtime_named(
            runtime_vulkan_device_uuid,
        )
        .map_err(|refusal| {
            format!("RiveSource: the external-renderer device was refused: {refusal}")
        })?;
        let surface_share_socket_path = std::env::var_os(SURFACE_SHARE_SOCKET_ENVIRONMENT_VARIABLE)
            .ok_or_else(|| {
                format!(
                    "RiveSource: {SURFACE_SHARE_SOCKET_ENVIRONMENT_VARIABLE} is not set, so no \
                     output texture can be checked out"
                )
            })?;
        let surface_share_connection = connect_to_surface_share_socket(Path::new(
            &surface_share_socket_path,
        ))
        .map_err(|connect_failure| {
            format!(
                "RiveSource: the surface-share socket refused a connection: \
                         {connect_failure}"
            )
        })?;
        let scene = RiveSceneOnExternalRendererDevice::create(
            &device.raw_vulkan_handles_for_the_external_renderer(),
            riv_bytes,
            settings.artboard.as_deref(),
            settings.state_machine.as_deref(),
        )
        .map_err(|refusal| format!("RiveSource: {:?}: {refusal}", settings.riv_path))?;
        Ok(Self {
            render_targets_by_pool_slot: HashMap::new(),
            scene,
            device,
            surface_share_connection,
            module_host,
            video_output_port,
            data_input_port,
            view_model_data_message_applier: ViewModelDataMessageApplier::default(),
            settings,
        })
    }

    fn log_what_plays(&self) {
        let state_machine = match self.scene.state_machine_name() {
            Some(state_machine_name) => format!("state machine {state_machine_name:?}"),
            None => "no state machine, drawn still".to_owned(),
        };
        self.log(
            ModuleHostLogLevel::Info,
            &format!(
                "RiveSource: {:?} plays artboard {:?}, {state_machine}, at {}x{} and {} fps on {}",
                self.settings.riv_path,
                self.scene.artboard_name(),
                self.settings.width,
                self.settings.height,
                self.settings.frame_rate,
                self.device.name(),
            ),
        );
    }

    fn log(&self, level: ModuleHostLogLevel, message: &str) {
        self.module_host.log(level, RIVE_LOG_TARGET, message);
    }

    fn run_until_stopped(mut self, stop_requested: &AtomicBool) {
        let frame_interval = Duration::from_secs_f64(1.0 / f64::from(self.settings.frame_rate));
        let mut next_frame_due = Instant::now();
        let mut previous_frame_drawn_at: Option<Instant> = None;
        let mut frames_published: u64 = 0;
        let mut frames_failed: u64 = 0;
        let mut render_time_since_report = Duration::ZERO;
        let mut report_window_started = Instant::now();
        while !stop_requested.load(Ordering::Acquire) && !self.module_host.stop_requested() {
            let now = Instant::now();
            if now < next_frame_due {
                match self
                    .module_host
                    .wait_for_any_input_port(next_frame_due - now)
                {
                    Ok(true) => self.apply_every_pending_data_message(),
                    Ok(false) => {}
                    Err(refusal) => {
                        self.log(
                            ModuleHostLogLevel::Error,
                            &format!(
                                "RiveSource: waiting for data failed, so rendering ends: {refusal}"
                            ),
                        );
                        return;
                    }
                }
                continue;
            }
            self.apply_every_pending_data_message();

            let frame_started = Instant::now();
            let elapsed_since_previous_frame = previous_frame_drawn_at
                .map(|drawn_at| frame_started.duration_since(drawn_at))
                .unwrap_or_default()
                .min(LONGEST_ANIMATION_STEP);
            previous_frame_drawn_at = Some(frame_started);
            match self.render_and_publish_one_frame(elapsed_since_previous_frame.as_secs_f32()) {
                Ok(()) => frames_published += 1,
                Err(failure) => {
                    frames_failed += 1;
                    if frames_failed.is_power_of_two() {
                        self.log(
                            ModuleHostLogLevel::Warn,
                            &format!("RiveSource: a frame was dropped ({frames_failed} so far): {failure}"),
                        );
                    }
                }
            }
            render_time_since_report += frame_started.elapsed();

            next_frame_due += frame_interval;
            if next_frame_due < Instant::now() {
                next_frame_due = Instant::now() + frame_interval;
            }

            if frames_published > 0 && frames_published.is_multiple_of(FRAMES_BETWEEN_REPORTS) {
                let window = report_window_started.elapsed().as_secs_f64();
                self.log(
                    ModuleHostLogLevel::Info,
                    &format!(
                        "RiveSource: {frames_published} frames published, {:.1} fps over the \
                         last {FRAMES_BETWEEN_REPORTS}, {:.2} ms to draw and publish each, \
                         {frames_failed} dropped",
                        FRAMES_BETWEEN_REPORTS as f64 / window,
                        render_time_since_report.as_secs_f64() * 1_000.0
                            / FRAMES_BETWEEN_REPORTS as f64,
                    ),
                );
                render_time_since_report = Duration::ZERO;
                report_window_started = Instant::now();
            }
        }
    }

    fn apply_every_pending_data_message(&mut self) {
        loop {
            let message = match self.data_input_port.read() {
                Ok(Some(message)) => message,
                Ok(None) => return,
                Err(refusal) => {
                    self.log(
                        ModuleHostLogLevel::Warn,
                        &format!("RiveSource: reading data failed: {refusal}"),
                    );
                    return;
                }
            };
            let reports = self
                .view_model_data_message_applier
                .apply(message.payload(), &mut self.scene);
            for report in reports {
                self.log(ModuleHostLogLevel::Warn, &report);
            }
        }
    }

    fn render_and_publish_one_frame(&mut self, elapsed_seconds: f32) -> Result<(), String> {
        let output_texture = self
            .module_host
            .acquire_texture_from_node_output_pool(&ModuleHostNodeOutputTextureRequest {
                pool_key: RENDERED_FRAME_POOL_KEY,
                rotation_depth: self.settings.output_pool_rotation_depth,
                width: self.settings.width,
                height: self.settings.height,
                texture_format: RENDERED_FRAME_TEXTURE_FORMAT,
                usage: &RENDERED_FRAME_TEXTURE_USAGE,
            })
            .map_err(|refusal| refusal.to_string())?;
        let surface_id = output_texture.surface_id().to_owned();
        let drawn = match check_out_surface_on_surface_share(
            &self.surface_share_connection,
            &surface_id,
        )? {
            CheckedOutSurface::Texture(checked_out) => {
                self.draw_into_checked_out_texture(&surface_id, checked_out, elapsed_seconds)
            }
            CheckedOutSurface::PixelBuffer(_) => Err(format!(
                "output pool surface {surface_id:?} checked out as a pixel buffer, not the \
                 texture it was acquired as"
            )),
        };
        let released =
            release_check_out_on_surface_share(&self.surface_share_connection, &surface_id);
        let published_layout = drawn?;
        released?;
        let record = PublishedVideoFrame {
            surface_id: &surface_id,
            width: self.settings.width,
            height: self.settings.height,
            timestamp_ns: self.module_host.monotonic_now_ns(),
            frames_per_second: self.settings.frame_rate,
            texture_layout: published_layout,
        };
        self.video_output_port
            .write(&record.to_msgpack(), Some(record.timestamp_ns), true)
            .map_err(|refusal| refusal.to_string())?;
        drop(output_texture);
        Ok(())
    }

    fn draw_into_checked_out_texture(
        &mut self,
        surface_id: &str,
        checked_out: streamlib_surface_client::CheckedOutTexture,
        elapsed_seconds: f32,
    ) -> Result<i32, String> {
        let pool_slot = pool_slot_of_a_published_frame_id(surface_id).to_owned();
        if !self.render_targets_by_pool_slot.contains_key(&pool_slot) {
            let render_target = self
                .device
                .import_node_output_texture_as_render_target(surface_id, checked_out)
                .map_err(|refusal| refusal.to_string())?;
            if self.render_targets_by_pool_slot.len() >= MOST_RENDER_TARGETS_KEPT {
                self.render_targets_by_pool_slot.clear();
            }
            self.render_targets_by_pool_slot
                .insert(pool_slot.clone(), render_target);
        }
        let render_target = &self.render_targets_by_pool_slot[&pool_slot];
        let frame = self
            .device
            .begin_frame_into(render_target)
            .map_err(|refusal| refusal.to_string())?;
        let layout_the_renderer_left_the_target_in = self
            .scene
            .advance_and_record(elapsed_seconds, &frame.raw_vulkan_handles())?;
        let published_layout = frame
            .submit_and_wait(layout_the_renderer_left_the_target_in)
            .map_err(|refusal| refusal.to_string())?;
        publish_image_layout_on_surface_share(
            &self.surface_share_connection,
            surface_id,
            published_layout.0,
        )?;
        Ok(published_layout.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_generation_of_a_pool_slot_shares_its_render_target() {
        assert_eq!(pool_slot_of_a_published_frame_id("slot-3#41"), "slot-3");
        assert_eq!(pool_slot_of_a_published_frame_id("slot-3#42"), "slot-3");
    }

    #[test]
    fn an_id_without_a_generation_is_its_own_slot() {
        assert_eq!(pool_slot_of_a_published_frame_id("one-off"), "one-off");
        assert_eq!(pool_slot_of_a_published_frame_id("odd#name"), "odd#name");
        assert_eq!(pool_slot_of_a_published_frame_id("#7"), "#7");
    }
}
