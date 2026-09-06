// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! Built-in virtual camera: video frames in, a camera any Linux application
//! can select out.
//!
//! Each instance is one camera that exists only while its processor runs —
//! created through the v4l2loopback module's control node at `setup()` and
//! removed at `teardown()`, a USB camera plugged in and pulled out from every
//! other application's point of view. The loopback device's buffers are
//! memory-mapped once and handed to the RHI, so the RGBA→YUYV pass writes
//! them straight from the GPU (or through one host-cached copy where the
//! driver declines the import); no CPU touches a pixel.
//!
//! The engine never loads the module, never writes a udev rule and never
//! asks for elevation. Without the one-time permission the sink refuses at
//! `setup()` by name and the runtime keeps running.

use std::ffi::{CStr, c_ulong};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use streamlib::sdk::color::{ColorSpaceKind, resolve_color_defaults};
use streamlib::sdk::context::{GpuContextLimitedAccess, RuntimeContextFullAccess};
use streamlib::sdk::engine::host_rhi::{
    HostMappingWrittenByGpu, RhiCommandRecorder, VulkanAccess, VulkanStage,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ReactiveProcessor;
use streamlib::sdk::rhi::{PixelFormat, RhiColorConverter, VulkanLayout};

use crate::cumulative_count_report_threshold::CumulativeCountReportThreshold;
use crate::v4l2_color::color_info_to_v4l2_color;
use crate::video_frame::{ColorInfo, Matrix, Primaries, Range, Transfer, VideoFrame};

/// The name every log line and refusal carries.
pub const VIRTUAL_CAMERA_SINK_PROCESSOR_NAME: &str = "VirtualCameraSink";

/// The v4l2loopback module's control node.
pub const V4L2LOOPBACK_CONTROL_NODE_PATH: &str = "/dev/v4l2loopback";

/// The one-time command that grants the loopback door.
pub const ENABLE_VIRTUAL_CAMERA_VERB: &str = "streamlib enable-virtual-camera";

/// What an unnamed camera is called, before its stable id.
const DEFAULT_CAMERA_NAME_PREFIX: &str = "StreamLib Camera";

/// Chrome asks for four buffers and the module clamps a request to the
/// device's count, so four is what every reader can have.
const LOOPBACK_DEVICE_BUFFER_COUNT: u32 = 4;

/// Dropped-frame log cadence, in frames.
const DROPPED_FRAME_REPORT_STEP: u64 = 300;

/// Written-frame log cadence, in frames.
const WRITTEN_FRAME_LOG_INTERVAL: u64 = 300;

/// `V4L2_FIELD_NONE` from `<linux/videodev2.h>`: progressive frames.
const V4L2_FIELD_NONE: u32 = 1;

/// `V4L2_BUF_TYPE_VIDEO_OUTPUT` as the `v4l` crate spells it.
const OUTPUT_BUFFER_TYPE: u32 = v4l::buffer::Type::VideoOutput as u32;

/// Which door a [`VirtualCameraSink`] takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VirtualCameraDoor {
    /// The loopback door when the control node is writable, else PipeWire.
    #[default]
    Auto,
    /// The loopback door or a refusal by name.
    #[serde(rename = "v4l2loopback")]
    V4l2Loopback,
    /// The PipeWire camera door.
    #[serde(rename = "pipewire")]
    PipeWire,
}

/// Configuration for [`VirtualCameraSink`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VirtualCameraSinkConfig {
    /// The camera's name in every picker. Absent: `StreamLib Camera` plus a
    /// short id that is unique per instance and app and stable across runs.
    #[serde(default)]
    pub name: Option<String>,
    /// Which door to take.
    #[serde(default)]
    pub door: VirtualCameraDoor,
}

// ---------------------------------------------------------------------------
// The module's control-node ABI (v4l2loopback.h, 0.15)
// ---------------------------------------------------------------------------

/// `struct v4l2_loopback_config` as the 0.15 module declares it: what
/// `CTL_ADD` takes and `CTL_QUERY` fills.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct V4l2LoopbackConfig {
    /// `/dev/video<nr>`; `-1` on `CTL_ADD` lets the module pick.
    pub output_nr: i32,
    /// The header's reserved `capture_nr` slot.
    pub unused: i32,
    /// NUL-terminated device label — what every picker shows.
    pub card_label: [u8; 32],
    pub min_width: u32,
    pub max_width: u32,
    pub min_height: u32,
    pub max_height: u32,
    /// Buffers per device; `<= 0` takes the module default.
    pub max_buffers: i32,
    /// Concurrent openers; `<= 0` takes the module default.
    pub max_openers: i32,
    pub debug: i32,
    /// `0` announces OUTPUT to the writer and CAPTURE to readers — the
    /// only mode Chromium's enumerator lists.
    pub announce_all_caps: i32,
}

impl V4l2LoopbackConfig {
    /// An all-zero config; `CTL_QUERY` for `device_number`.
    pub fn query_of(device_number: u32) -> Self {
        Self {
            output_nr: device_number as i32,
            ..Self::zeroed()
        }
    }

    /// The device one sink creates: labelled, capture-only to readers,
    /// four buffers, the number the module's choice.
    pub fn for_new_camera(label: &str) -> Self {
        let mut config = Self {
            output_nr: -1,
            max_buffers: LOOPBACK_DEVICE_BUFFER_COUNT as i32,
            announce_all_caps: 0,
            ..Self::zeroed()
        };
        let bytes = label.as_bytes();
        let copied = bytes.len().min(config.card_label.len() - 1);
        config.card_label[..copied].copy_from_slice(&bytes[..copied]);
        config
    }

    fn zeroed() -> Self {
        Self {
            output_nr: 0,
            unused: 0,
            card_label: [0; 32],
            min_width: 0,
            max_width: 0,
            min_height: 0,
            max_height: 0,
            max_buffers: 0,
            max_openers: 0,
            debug: 0,
            announce_all_caps: 0,
        }
    }

    /// The label as text, up to its NUL.
    pub fn label(&self) -> String {
        let end = self
            .card_label
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.card_label.len());
        String::from_utf8_lossy(&self.card_label[..end]).into_owned()
    }
}

/// Linux `_IOC(dir, type, nr, size)`.
const fn linux_ioc(direction: c_ulong, io_type: u8, number: u8, size: usize) -> c_ulong {
    (direction << 30) | ((size as c_ulong) << 16) | ((io_type as c_ulong) << 8) | (number as c_ulong)
}
const IOC_WRITE: c_ulong = 1;
const IOC_READ: c_ulong = 2;
const V4L2LOOPBACK_CTL_IOCTL_MAGIC: u8 = b'~';

/// `V4L2LOOPBACK_CTL_VERSION`: fills a `u32` with the module's version code.
pub const V4L2LOOPBACK_CTL_VERSION: c_ulong = linux_ioc(
    IOC_READ,
    V4L2LOOPBACK_CTL_IOCTL_MAGIC,
    0,
    std::mem::size_of::<u32>(),
);
/// `V4L2LOOPBACK_CTL_ADD`: takes a config, returns the device number.
pub const V4L2LOOPBACK_CTL_ADD: c_ulong = linux_ioc(
    IOC_WRITE,
    V4L2LOOPBACK_CTL_IOCTL_MAGIC,
    1,
    std::mem::size_of::<V4l2LoopbackConfig>(),
);
/// `V4L2LOOPBACK_CTL_REMOVE`: takes the device number by value; `EBUSY`
/// while any opener holds the device.
pub const V4L2LOOPBACK_CTL_REMOVE: c_ulong = linux_ioc(
    IOC_WRITE,
    V4L2LOOPBACK_CTL_IOCTL_MAGIC,
    2,
    std::mem::size_of::<u32>(),
);
/// `V4L2LOOPBACK_CTL_QUERY`: fills a config for `output_nr`.
pub const V4L2LOOPBACK_CTL_QUERY: c_ulong = linux_ioc(
    IOC_READ | IOC_WRITE,
    V4L2LOOPBACK_CTL_IOCTL_MAGIC,
    3,
    std::mem::size_of::<V4l2LoopbackConfig>(),
);

// ---------------------------------------------------------------------------
// The control node as a seam, so the door rule is provable without one
// ---------------------------------------------------------------------------

/// How the control node answered an `O_RDWR` open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlNodeAccess {
    /// Open read-write: the loopback door is ours.
    Writable,
    /// The node does not exist: the module is not loaded.
    Absent,
    /// The node exists but this user may not write it.
    NotWritable,
}

impl ControlNodeAccess {
    fn describe(self) -> &'static str {
        match self {
            Self::Writable => "writable",
            Self::Absent => "absent",
            Self::NotWritable => "not writable by this user",
        }
    }
}

/// What a sink needs from the module's control node.
pub(crate) trait LoopbackControlNode {
    fn access(&self) -> ControlNodeAccess;
    /// The `/dev/video<N>` numbers to ask the module about.
    fn candidate_device_numbers(&self) -> Vec<u32>;
    /// `CTL_QUERY`; `None` for a number the module does not own.
    fn query_device(&self, device_number: u32) -> Option<V4l2LoopbackConfig>;
    /// `CTL_ADD`; the device number on success.
    fn add_device(&self, config: &V4l2LoopbackConfig) -> std::io::Result<u32>;
    /// `CTL_REMOVE`.
    fn remove_device(&self, device_number: u32) -> std::io::Result<()>;
    /// `CTL_VERSION`, for the log line.
    fn module_version(&self) -> Option<(u32, u32, u32)>;
}

/// The real node at [`V4L2LOOPBACK_CONTROL_NODE_PATH`].
struct LoopbackControlNodeOnDisk;

impl LoopbackControlNodeOnDisk {
    fn open_read_write(&self) -> std::io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(V4L2LOOPBACK_CONTROL_NODE_PATH)
    }
}

impl LoopbackControlNode for LoopbackControlNodeOnDisk {
    fn access(&self) -> ControlNodeAccess {
        match self.open_read_write() {
            Ok(_) => ControlNodeAccess::Writable,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ControlNodeAccess::Absent,
            Err(_) => ControlNodeAccess::NotWritable,
        }
    }

    fn candidate_device_numbers(&self) -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir("/dev") else {
            return Vec::new();
        };
        let mut numbers: Vec<u32> = entries
            .flatten()
            .filter_map(|entry| {
                entry
                    .file_name()
                    .to_str()?
                    .strip_prefix("video")?
                    .parse()
                    .ok()
            })
            .collect();
        numbers.sort_unstable();
        numbers
    }

    fn query_device(&self, device_number: u32) -> Option<V4l2LoopbackConfig> {
        use std::os::fd::AsRawFd as _;
        let node = self.open_read_write().ok()?;
        let mut config = V4l2LoopbackConfig::query_of(device_number);
        let result = unsafe { libc::ioctl(node.as_raw_fd(), V4L2LOOPBACK_CTL_QUERY, &mut config) };
        (result >= 0).then_some(config)
    }

    fn add_device(&self, config: &V4l2LoopbackConfig) -> std::io::Result<u32> {
        use std::os::fd::AsRawFd as _;
        let node = self.open_read_write()?;
        let result = unsafe { libc::ioctl(node.as_raw_fd(), V4L2LOOPBACK_CTL_ADD, config) };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(result as u32)
    }

    fn remove_device(&self, device_number: u32) -> std::io::Result<()> {
        use std::os::fd::AsRawFd as _;
        let node = self.open_read_write()?;
        let result = unsafe {
            libc::ioctl(
                node.as_raw_fd(),
                V4L2LOOPBACK_CTL_REMOVE,
                device_number as c_ulong,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn module_version(&self) -> Option<(u32, u32, u32)> {
        use std::os::fd::AsRawFd as _;
        let node = self.open_read_write().ok()?;
        let mut code: u32 = 0;
        let result = unsafe { libc::ioctl(node.as_raw_fd(), V4L2LOOPBACK_CTL_VERSION, &mut code) };
        (result >= 0).then(|| ((code >> 16) & 0xff, (code >> 8) & 0xff, code & 0xff))
    }
}

/// The door rule's answer at `setup()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DoorDecision {
    /// Create or reclaim a loopback device.
    Loopback,
    /// Refuse at `setup()` with this message.
    Refused(String),
}

/// The refusal text for a loopback door with no permission behind it.
fn no_permission_refusal(camera_name: &str, access: ControlNodeAccess, door: VirtualCameraDoor) -> String {
    let mut message = format!(
        "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME} \"{camera_name}\": no permission to create a \
         v4l2loopback camera: {V4L2LOOPBACK_CONTROL_NODE_PATH} is {}. Run \
         `{ENABLE_VIRTUAL_CAMERA_VERB}` once (it asks for your password), then re-run",
        access.describe()
    );
    match door {
        VirtualCameraDoor::Auto => message.push_str(
            "; the PipeWire door `auto` falls back to is not built yet, so `auto` refuses the \
             same way `v4l2loopback` does for now.",
        ),
        VirtualCameraDoor::V4l2Loopback | VirtualCameraDoor::PipeWire => message.push('.'),
    }
    message
}

/// Choose the door for one instance. The PipeWire door is not built yet,
/// so `auto` without permission refuses like `v4l2loopback`, saying so,
/// and `pipewire` refuses by name.
pub(crate) fn decide_door(
    door: VirtualCameraDoor,
    camera_name: &str,
    access: ControlNodeAccess,
) -> DoorDecision {
    match door {
        VirtualCameraDoor::PipeWire => DoorDecision::Refused(format!(
            "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME} \"{camera_name}\": the PipeWire door is not \
             built yet; set door=\"auto\" or door=\"v4l2loopback\""
        )),
        VirtualCameraDoor::Auto | VirtualCameraDoor::V4l2Loopback => match access {
            ControlNodeAccess::Writable => DoorDecision::Loopback,
            absent_or_locked => {
                DoorDecision::Refused(no_permission_refusal(camera_name, absent_or_locked, door))
            }
        },
    }
}

/// The loopback device already carrying `label` — left by a crash, or by
/// a reader that held it at the last teardown — if the module has one.
pub(crate) fn find_device_carrying_label(
    node: &dyn LoopbackControlNode,
    label: &str,
) -> Option<u32> {
    node.candidate_device_numbers()
        .into_iter()
        .find(|&number| node.query_device(number).is_some_and(|c| c.label() == label))
}

/// FNV-1a over the bytes, so the id is the same on every run and every
/// Rust version.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Four base-36 characters of a hash over the app's directory and the
/// instance's display name.
fn stable_camera_id(app_directory: &Path, processor_display_name: &str) -> String {
    let mut hash = fnv1a_64(app_directory.as_os_str().as_encoded_bytes());
    hash = fnv1a_64(&[&hash.to_le_bytes()[..], processor_display_name.as_bytes()].concat());
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    (0..4)
        .map(|_| {
            let c = ALPHABET[(hash % 36) as usize] as char;
            hash /= 36;
            c
        })
        .collect()
}

/// The camera's label: the configured name, else the default prefix plus
/// the stable id.
pub(crate) fn camera_name_for(
    configured_name: Option<&str>,
    app_directory: &Path,
    processor_display_name: &str,
) -> String {
    match configured_name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => name.to_string(),
        None => format!(
            "{DEFAULT_CAMERA_NAME_PREFIX} {}",
            stable_camera_id(app_directory, processor_display_name)
        ),
    }
}

// ---------------------------------------------------------------------------
// The device, once created
// ---------------------------------------------------------------------------

/// The loopback device this sink writes: open read-write, non-blocking.
struct OpenedLoopbackDevice {
    fd: RawFd,
    device_number: u32,
    path: PathBuf,
}

impl OpenedLoopbackDevice {
    fn open(device_number: u32) -> std::io::Result<Self> {
        let path = PathBuf::from(format!("/dev/video{device_number}"));
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            fd,
            device_number,
            path,
        })
    }

    fn ioctl<T>(&self, request: c_ulong, argument: &mut T, what: &str) -> Result<()> {
        let result = unsafe { libc::ioctl(self.fd, request, argument) };
        if result < 0 {
            return Err(Error::Runtime(format!(
                "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: {what} on {} failed: {}",
                self.path.display(),
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    fn driver_name(&self) -> Option<String> {
        let mut capability: v4l::v4l_sys::v4l2_capability = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::ioctl(
                self.fd,
                v4l::v4l2::vidioc::VIDIOC_QUERYCAP as c_ulong,
                &mut capability,
            )
        };
        (result == 0).then(|| {
            unsafe { CStr::from_ptr(capability.driver.as_ptr().cast()) }
                .to_string_lossy()
                .into_owned()
        })
    }
}

impl Drop for OpenedLoopbackDevice {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

/// One of the device's output buffers, mapped once and imported once.
struct MappedOutputBuffer {
    index: u32,
    mapping_ptr: *mut u8,
    mapping_len: usize,
    written_by_gpu: HostMappingWrittenByGpu,
    queued: bool,
}

impl Drop for MappedOutputBuffer {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.mapping_ptr.cast(), self.mapping_len) };
    }
}

// SAFETY: the mapping is this processor's alone — created, written through
// the RHI and unmapped on the processor's own thread — and the raw pointer
// is held only to unmap it.
unsafe impl Send for MappedOutputBuffer {}

/// The device's streaming state for one frame extent.
struct StreamingOutputFormat {
    width: u32,
    height: u32,
    bytesperline: u32,
    sizeimage: u32,
    buffers: Vec<MappedOutputBuffer>,
    next_buffer_index: usize,
    color_info: Option<ColorInfo>,
}

impl StreamingOutputFormat {
    fn matches(&self, frame: &VideoFrame) -> bool {
        self.width == frame.width && self.height == frame.height
    }
}

/// The GPU side of one sink: minted at `setup()`, used every frame.
struct GpuSide {
    gpu: GpuContextLimitedAccess,
    converter: RhiColorConverter,
    recorder: RhiCommandRecorder,
}

/// Why the sink is dropping every frame it is handed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LatchedRefusal {
    OddWidth { width: u32 },
    DeviceConfiguration(String),
}

#[streamlib::sdk::processor(
    description = "Presents video frames as a virtual camera any Linux application can select",
    execution = reactive,
    scheduling = high,
    config = crate::virtual_camera_sink::VirtualCameraSinkConfig,
    input("video", delivery_profile = "newest", description = "Video frames to present as the camera's picture"),
)]
pub struct VirtualCameraSink {
    camera_name: String,
    device: Option<OpenedLoopbackDevice>,
    streaming: Option<StreamingOutputFormat>,
    gpu_side: Option<GpuSide>,
    latched_refusal: Option<LatchedRefusal>,
    frames_written: u64,
    frames_dropped_every_buffer_queued: u64,
    frames_dropped_under_refusal: u64,
    dropped_frame_report: Option<CumulativeCountReportThreshold>,
    tier_logged: bool,
}

impl ReactiveProcessor for VirtualCameraSink::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let app_directory = std::env::current_dir().unwrap_or_default();
        let processor_display_name = ctx
            .processor_display_name()
            .or_else(|| ctx.processor_id())
            .unwrap_or_else(|| VIRTUAL_CAMERA_SINK_PROCESSOR_NAME.to_string());
        self.camera_name =
            camera_name_for(self.config.name.as_deref(), &app_directory, &processor_display_name);
        self.dropped_frame_report = Some(CumulativeCountReportThreshold::reporting_every(
            DROPPED_FRAME_REPORT_STEP,
        ));

        let node = LoopbackControlNodeOnDisk;
        let access = node.access();
        match decide_door(self.config.door, &self.camera_name, access) {
            DoorDecision::Loopback => {}
            DoorDecision::Refused(message) => {
                tracing::error!(
                    camera = %self.camera_name,
                    door = ?self.config.door,
                    control_node = access.describe(),
                    "{message}"
                );
                return Err(Error::Runtime(message));
            }
        }

        let (device_number, reclaimed) = match find_device_carrying_label(&node, &self.camera_name)
        {
            Some(number) => (number, true),
            None => {
                let config = V4l2LoopbackConfig::for_new_camera(&self.camera_name);
                let number = node.add_device(&config).map_err(|e| {
                    Error::Runtime(format!(
                        "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME} \"{}\": the module refused to \
                         create a camera (CTL_ADD on {V4L2LOOPBACK_CONTROL_NODE_PATH}): {e}",
                        self.camera_name
                    ))
                })?;
                (number, false)
            }
        };
        let device = OpenedLoopbackDevice::open(device_number).map_err(|e| {
            Error::Runtime(format!(
                "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME} \"{}\": /dev/video{device_number} was \
                 created but could not be opened: {e}",
                self.camera_name
            ))
        })?;
        let module_version = node
            .module_version()
            .map(|(major, minor, bugfix)| format!("{major}.{minor}.{bugfix}"));
        tracing::info!(
            camera = %self.camera_name,
            door = "v4l2loopback",
            reason = if reclaimed {
                "a device carrying this camera's label was left behind and is reclaimed"
            } else {
                "the control node is writable, so the sink created its own device"
            },
            device = %device.path.display(),
            driver = device.driver_name().as_deref().unwrap_or("?"),
            module_version = module_version.as_deref().unwrap_or("?"),
            "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: camera created"
        );

        let gpu_full = ctx.gpu_full_access();
        let converter = gpu_full
            .color_converter(PixelFormat::Rgba32, PixelFormat::Yuyv422)
            .map_err(|e| {
                Error::Runtime(format!(
                    "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME} \"{}\": no RGBA→YUYV converter: {e}",
                    self.camera_name
                ))
            })?;
        let recorder = gpu_full
            .create_command_recorder("virtual_camera_sink")
            .map_err(|e| {
                Error::Runtime(format!(
                    "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME} \"{}\": no command recorder: {e}",
                    self.camera_name
                ))
            })?;
        self.gpu_side = Some(GpuSide {
            gpu: ctx.gpu_limited_access().clone(),
            converter,
            recorder,
        });
        self.device = Some(device);
        Ok(())
    }

    fn process(&mut self, _ctx: &streamlib::sdk::context::RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("video") {
            return Ok(());
        }
        let frame: VideoFrame = self.inputs.read("video")?;
        if self.latched_refusal.is_some() {
            self.frames_dropped_under_refusal += 1;
            return Ok(());
        }
        match self.present_one_frame(&frame) {
            Ok(()) => Ok(()),
            Err(refusal) => {
                tracing::error!(
                    camera = %self.camera_name,
                    "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: every later frame is dropped: {refusal:?}"
                );
                self.latched_refusal = Some(refusal);
                Ok(())
            }
        }
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if let (Some(device), Some(mut streaming)) = (self.device.as_ref(), self.streaming.take()) {
            let _ = stop_streaming_and_release_buffers(device, &mut streaming);
        }
        self.gpu_side.take();
        let Some(device) = self.device.take() else {
            return Ok(());
        };
        let device_number = device.device_number;
        let path = device.path.clone();
        drop(device);
        match LoopbackControlNodeOnDisk.remove_device(device_number) {
            Ok(()) => tracing::info!(
                camera = %self.camera_name,
                device = %path.display(),
                frames_written = self.frames_written,
                frames_dropped_every_buffer_queued = self.frames_dropped_every_buffer_queued,
                frames_dropped_under_refusal = self.frames_dropped_under_refusal,
                "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: teardown — camera removed"
            ),
            Err(e) if e.raw_os_error() == Some(libc::EBUSY) => tracing::warn!(
                camera = %self.camera_name,
                device = %path.display(),
                "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: teardown — a reader still holds the \
                 camera, so the device is left in place and reclaimed by label at the next setup"
            ),
            Err(e) => tracing::warn!(
                camera = %self.camera_name,
                device = %path.display(),
                error = %e,
                "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: teardown — the device could not be \
                 removed and is left for the next setup to reclaim"
            ),
        }
        Ok(())
    }
}

impl VirtualCameraSink::Processor {
    fn present_one_frame(&mut self, frame: &VideoFrame) -> std::result::Result<(), LatchedRefusal> {
        if frame.width % 2 != 0 || frame.width == 0 || frame.height == 0 {
            return Err(LatchedRefusal::OddWidth { width: frame.width });
        }
        let (Some(device), Some(gpu_side)) = (self.device.as_ref(), self.gpu_side.as_mut()) else {
            return Ok(());
        };

        if self.streaming.as_ref().is_some_and(|s| !s.matches(frame)) {
            let mut stale = self.streaming.take().expect("checked");
            tracing::info!(
                camera = %self.camera_name,
                from = format!("{}x{}", stale.width, stale.height),
                to = format!("{}x{}", frame.width, frame.height),
                "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: upstream extent changed — \
                 re-negotiating the device format; readers reopen as for a camera re-plugged"
            );
            if let Err(e) = stop_streaming_and_release_buffers(device, &mut stale) {
                return Err(LatchedRefusal::DeviceConfiguration(e.to_string()));
            }
        }
        if self.streaming.is_none() {
            let streaming = negotiate_output_format_and_start_streaming(
                device,
                &gpu_side.gpu,
                frame,
                &self.camera_name,
            )
            .map_err(|e| LatchedRefusal::DeviceConfiguration(e.to_string()))?;
            if !self.tier_logged {
                if let Some(first) = streaming.buffers.first() {
                    tracing::info!(
                        camera = %self.camera_name,
                        tier = first.written_by_gpu.tier().as_str(),
                        gpu_written_memory_is_host_cached = first.written_by_gpu.gpu_written_memory_is_host_cached(),
                        reason = first.written_by_gpu.fallback_reason().unwrap_or("the driver imported the device's mapping"),
                        "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: the GPU writes the device's buffers through this tier"
                    );
                }
                self.tier_logged = true;
            }
            self.streaming = Some(streaming);
        }
        let streaming = self.streaming.as_mut().expect("just negotiated");

        reclaim_dequeued_buffers(device, streaming);
        let Some(buffer_index) = next_free_buffer(streaming) else {
            self.frames_dropped_every_buffer_queued += 1;
            if self
                .dropped_frame_report
                .get_or_insert_with(|| {
                    CumulativeCountReportThreshold::reporting_every(DROPPED_FRAME_REPORT_STEP)
                })
                .count_is_worth_reporting(self.frames_dropped_every_buffer_queued)
            {
                tracing::warn!(
                    camera = %self.camera_name,
                    frames_dropped_every_buffer_queued = self.frames_dropped_every_buffer_queued,
                    "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: a frame arrived with every buffer queued"
                );
            }
            return Ok(());
        };

        match write_frame_into_buffer(gpu_side, streaming, buffer_index, frame) {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(
                    camera = %self.camera_name,
                    error = %e,
                    "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: a frame could not be written"
                );
                return Ok(());
            }
        }
        if let Err(e) = queue_buffer(device, streaming, buffer_index, frame.timestamp_ns) {
            tracing::warn!(camera = %self.camera_name, error = %e, "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: QBUF failed");
            return Ok(());
        }

        self.frames_written += 1;
        if self.frames_written == 1 {
            tracing::info!(
                camera = %self.camera_name,
                width = frame.width,
                height = frame.height,
                "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: first frame presented"
            );
        } else if self.frames_written.is_multiple_of(WRITTEN_FRAME_LOG_INTERVAL) {
            tracing::info!(
                camera = %self.camera_name,
                frames_written = self.frames_written,
                frames_dropped_every_buffer_queued = self.frames_dropped_every_buffer_queued,
                "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: progress"
            );
        }
        Ok(())
    }
}

/// `S_FMT`, `S_PARM`, `REQBUFS`, `QUERYBUF` + `mmap` + import, `STREAMON` —
/// in that order and no other: the loopback's poll returns nothing between
/// `REQBUFS` and `STREAMON`, so a queue before start would hang forever.
fn negotiate_output_format_and_start_streaming(
    device: &OpenedLoopbackDevice,
    gpu: &GpuContextLimitedAccess,
    frame: &VideoFrame,
    camera_name: &str,
) -> Result<StreamingOutputFormat> {
    let yuyv = u32::from_le_bytes(*b"YUYV");
    let color_fields = color_info_to_v4l2_color(frame.color_info.as_ref().unwrap_or(&ColorInfo::default()));

    let mut format: v4l::v4l_sys::v4l2_format = unsafe { std::mem::zeroed() };
    format.type_ = OUTPUT_BUFFER_TYPE;
    format.fmt.pix.width = frame.width;
    format.fmt.pix.height = frame.height;
    format.fmt.pix.pixelformat = yuyv;
    format.fmt.pix.field = V4L2_FIELD_NONE;
    format.fmt.pix.bytesperline = frame.width * 2;
    format.fmt.pix.sizeimage = frame.width * 2 * frame.height;
    format.fmt.pix.colorspace = color_fields.colorspace;
    format.fmt.pix.xfer_func = color_fields.xfer_func;
    format.fmt.pix.__bindgen_anon_1.ycbcr_enc = color_fields.ycbcr_enc;
    format.fmt.pix.quantization = color_fields.quantization;
    device.ioctl(v4l::v4l2::vidioc::VIDIOC_S_FMT as c_ulong, &mut format, "S_FMT")?;
    let (bytesperline, sizeimage, set_pixelformat) = unsafe {
        (
            format.fmt.pix.bytesperline,
            format.fmt.pix.sizeimage,
            format.fmt.pix.pixelformat,
        )
    };
    if set_pixelformat != yuyv || bytesperline < frame.width * 2 || bytesperline % 4 != 0 {
        return Err(Error::Runtime(format!(
            "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME} \"{camera_name}\": the device set \
             {}x{} bytesperline={bytesperline} rather than YUYV at 2×width",
            frame.width, frame.height
        )));
    }

    if let Some(fps) = frame.fps.filter(|&fps| fps > 0) {
        let mut parm: v4l::v4l_sys::v4l2_streamparm = unsafe { std::mem::zeroed() };
        parm.type_ = OUTPUT_BUFFER_TYPE;
        parm.parm.output.timeperframe.numerator = 1;
        parm.parm.output.timeperframe.denominator = fps;
        if let Err(e) = device.ioctl(v4l::v4l2::vidioc::VIDIOC_S_PARM as c_ulong, &mut parm, "S_PARM") {
            tracing::debug!(camera = camera_name, error = %e, "S_PARM declined; readers keep the device's interval");
        }
    }

    let mut request: v4l::v4l_sys::v4l2_requestbuffers = unsafe { std::mem::zeroed() };
    request.count = LOOPBACK_DEVICE_BUFFER_COUNT;
    request.type_ = OUTPUT_BUFFER_TYPE;
    request.memory = v4l::memory::Memory::Mmap as u32;
    device.ioctl(v4l::v4l2::vidioc::VIDIOC_REQBUFS as c_ulong, &mut request, "REQBUFS")?;
    if request.count == 0 {
        return Err(Error::Runtime(format!(
            "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME} \"{camera_name}\": the device granted no output buffers"
        )));
    }

    let mut buffers = Vec::with_capacity(request.count as usize);
    for index in 0..request.count {
        let mut description: v4l::v4l_sys::v4l2_buffer = unsafe { std::mem::zeroed() };
        description.type_ = OUTPUT_BUFFER_TYPE;
        description.memory = v4l::memory::Memory::Mmap as u32;
        description.index = index;
        device.ioctl(v4l::v4l2::vidioc::VIDIOC_QUERYBUF as c_ulong, &mut description, "QUERYBUF")?;
        let mapping_len = description.length as usize;
        let offset = unsafe { description.m.offset } as libc::off_t;
        let mapping_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapping_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                device.fd,
                offset,
            )
        };
        if mapping_ptr == libc::MAP_FAILED {
            return Err(Error::Runtime(format!(
                "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME} \"{camera_name}\": mmap of output buffer \
                 {index} failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mapping_ptr = mapping_ptr.cast::<u8>();
        let written_by_gpu = gpu
            .escalate(|full| full.import_host_mapping_for_gpu_writes(mapping_ptr, mapping_len))
            .map_err(|e| {
                unsafe { libc::munmap(mapping_ptr.cast(), mapping_len) };
                Error::Runtime(format!(
                    "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME} \"{camera_name}\": the RHI could not \
                     take output buffer {index}: {e}"
                ))
            })?;
        buffers.push(MappedOutputBuffer {
            index,
            mapping_ptr,
            mapping_len,
            written_by_gpu,
            queued: false,
        });
    }

    let mut buffer_type = OUTPUT_BUFFER_TYPE;
    device.ioctl(v4l::v4l2::vidioc::VIDIOC_STREAMON as c_ulong, &mut buffer_type, "STREAMON")?;

    tracing::info!(
        camera = camera_name,
        width = frame.width,
        height = frame.height,
        bytesperline,
        sizeimage,
        buffers = buffers.len(),
        "{VIRTUAL_CAMERA_SINK_PROCESSOR_NAME}: device format set from the frame — YUYV, streaming"
    );
    Ok(StreamingOutputFormat {
        width: frame.width,
        height: frame.height,
        bytesperline,
        sizeimage,
        buffers,
        next_buffer_index: 0,
        color_info: frame.color_info.clone(),
    })
}

/// `STREAMOFF`, then `REQBUFS(0)`; the mappings unmap with the buffers.
fn stop_streaming_and_release_buffers(
    device: &OpenedLoopbackDevice,
    streaming: &mut StreamingOutputFormat,
) -> Result<()> {
    let mut buffer_type = OUTPUT_BUFFER_TYPE;
    let stream_off = device.ioctl(v4l::v4l2::vidioc::VIDIOC_STREAMOFF as c_ulong, &mut buffer_type, "STREAMOFF");
    streaming.buffers.clear();
    let mut request: v4l::v4l_sys::v4l2_requestbuffers = unsafe { std::mem::zeroed() };
    request.count = 0;
    request.type_ = OUTPUT_BUFFER_TYPE;
    request.memory = v4l::memory::Memory::Mmap as u32;
    let release = device.ioctl(v4l::v4l2::vidioc::VIDIOC_REQBUFS as c_ulong, &mut request, "REQBUFS(0)");
    stream_off.and(release)
}

/// `DQBUF` until the driver has nothing more to hand back; each returned
/// buffer is free again.
fn reclaim_dequeued_buffers(device: &OpenedLoopbackDevice, streaming: &mut StreamingOutputFormat) {
    loop {
        let mut description: v4l::v4l_sys::v4l2_buffer = unsafe { std::mem::zeroed() };
        description.type_ = OUTPUT_BUFFER_TYPE;
        description.memory = v4l::memory::Memory::Mmap as u32;
        let result = unsafe {
            libc::ioctl(
                device.fd,
                v4l::v4l2::vidioc::VIDIOC_DQBUF as c_ulong,
                &mut description,
            )
        };
        if result < 0 {
            return;
        }
        if let Some(buffer) = streaming
            .buffers
            .iter_mut()
            .find(|b| b.index == description.index)
        {
            buffer.queued = false;
        }
    }
}

/// The next buffer the driver is not holding, round-robin from the last one.
fn next_free_buffer(streaming: &mut StreamingOutputFormat) -> Option<usize> {
    let count = streaming.buffers.len();
    (0..count)
        .map(|step| (streaming.next_buffer_index + step) % count)
        .find(|&candidate| !streaming.buffers[candidate].queued)
        .inspect(|&chosen| streaming.next_buffer_index = (chosen + 1) % count)
}

/// Resolve the frame's texture and run the RGBA→YUYV pass with the
/// buffer's mapping as the kernel's output, leaving the texture in the
/// layout it was found in.
fn write_frame_into_buffer(
    gpu_side: &mut GpuSide,
    streaming: &mut StreamingOutputFormat,
    buffer_index: usize,
    frame: &VideoFrame,
) -> Result<()> {
    let registration = gpu_side.gpu.resolve_texture_registration_by_surface_id(
        &frame.surface_id,
        frame.texture_layout,
        frame.width,
        frame.height,
    )?;
    let color_info = frame.color_info.as_ref().or(streaming.color_info.as_ref());
    let resolved_color = resolve_color_defaults(
        color_info.and_then(|c| c.primaries.as_ref()).map(Primaries::engine_id),
        color_info.and_then(|c| c.transfer.as_ref()).map(Transfer::engine_id),
        color_info.and_then(|c| c.matrix.as_ref()).map(Matrix::engine_id),
        color_info.and_then(|c| c.range.as_ref()).map(Range::engine_id),
        ColorSpaceKind::Yuv,
    );
    let buffer = &streaming.buffers[buffer_index];
    let kernel = gpu_side.converter.prepare_image_to_yuyv_buffer(
        registration.texture(),
        buffer.written_by_gpu.storage_buffer(),
        streaming.bytesperline,
        &resolved_color,
    )?;

    let found_layout = registration.current_layout();
    let recorder = &mut gpu_side.recorder;
    recorder.begin()?;
    recorder.record_image_barrier(
        registration.texture(),
        found_layout,
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        VulkanStage::ALL_COMMANDS,
        VulkanStage::COMPUTE_SHADER,
        VulkanAccess::MEMORY_WRITE,
        VulkanAccess::SHADER_SAMPLED_READ,
    )?;
    const WORKGROUP: u32 = 16;
    recorder.record_dispatch(
        &kernel,
        frame.width.div_ceil(2).div_ceil(WORKGROUP),
        frame.height.div_ceil(WORKGROUP),
        1,
    )?;
    buffer.written_by_gpu.record_release_to_host(recorder)?;
    if found_layout != VulkanLayout::UNDEFINED && found_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL {
        recorder.record_image_barrier(
            registration.texture(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            found_layout,
            VulkanStage::COMPUTE_SHADER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::SHADER_SAMPLED_READ,
            VulkanAccess::MEMORY_READ,
        )?;
    } else if found_layout == VulkanLayout::UNDEFINED {
        registration.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
    }
    recorder.submit_and_wait()?;
    buffer.written_by_gpu.publish_to_host();
    Ok(())
}

/// `QBUF` with the frame's monotonic stamp; the driver copies it through
/// under `TIMESTAMP_COPY`.
fn queue_buffer(
    device: &OpenedLoopbackDevice,
    streaming: &mut StreamingOutputFormat,
    buffer_index: usize,
    timestamp_ns: i64,
) -> Result<()> {
    let buffer = &mut streaming.buffers[buffer_index];
    let mut description: v4l::v4l_sys::v4l2_buffer = unsafe { std::mem::zeroed() };
    description.type_ = OUTPUT_BUFFER_TYPE;
    description.memory = v4l::memory::Memory::Mmap as u32;
    description.index = buffer.index;
    description.field = V4L2_FIELD_NONE;
    description.bytesused = streaming.sizeimage;
    description.timestamp.tv_sec = timestamp_ns.div_euclid(1_000_000_000) as libc::time_t;
    description.timestamp.tv_usec = (timestamp_ns.rem_euclid(1_000_000_000) / 1_000) as libc::suseconds_t;
    device.ioctl(v4l::v4l2::vidioc::VIDIOC_QBUF as c_ulong, &mut description, "QBUF")?;
    buffer.queued = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use streamlib::sdk::processors::GeneratedProcessor;

    use super::*;

    #[test]
    fn the_only_port_is_one_newest_input_and_there_is_no_output() {
        let descriptor = <VirtualCameraSink::Processor as GeneratedProcessor>::descriptor()
            .expect("the macro emits a descriptor");
        assert_eq!(descriptor.inputs.len(), 1);
        assert_eq!(
            descriptor.outputs.len(),
            0,
            "a camera is presented to other applications, never published on a link"
        );
        let video = &descriptor.inputs[0];
        assert_eq!(video.name, "video");
        assert_eq!(
            video.delivery_profile.as_deref(),
            Some("newest"),
            "a camera shows the latest picture, never a backlog"
        );
    }

    #[test]
    fn the_config_names_the_camera_and_the_door_and_nothing_else() {
        let defaults: VirtualCameraSinkConfig =
            serde_json::from_value(serde_json::json!({})).expect("everything is optional");
        assert_eq!(defaults.name, None);
        assert_eq!(defaults.door, VirtualCameraDoor::Auto);

        let named: VirtualCameraSinkConfig = serde_json::from_value(
            serde_json::json!({ "name": "Desk cam", "door": "v4l2loopback" }),
        )
        .expect("both keys");
        assert_eq!(named.name.as_deref(), Some("Desk cam"));
        assert_eq!(named.door, VirtualCameraDoor::V4l2Loopback);
        let pipewire: VirtualCameraSinkConfig =
            serde_json::from_value(serde_json::json!({ "door": "pipewire" })).expect("pipewire");
        assert_eq!(pipewire.door, VirtualCameraDoor::PipeWire);
        for misspelt in ["V4L2Loopback", "v4l2_loopback", "pipe_wire"] {
            assert!(
                serde_json::from_value::<VirtualCameraSinkConfig>(
                    serde_json::json!({ "door": misspelt })
                )
                .is_err(),
                "the door vocabulary is exactly auto | v4l2loopback | pipewire, not {misspelt}"
            );
        }
    }

    #[test]
    fn an_unnamed_camera_gets_a_stable_id_that_differs_between_instances() {
        let app = Path::new("/home/someone/apps/desk");
        let first = camera_name_for(None, app, "VirtualCameraSink");
        let second = camera_name_for(None, app, "VirtualCameraSink 2");
        let other_app = camera_name_for(None, Path::new("/home/someone/apps/lab"), "VirtualCameraSink");

        assert!(first.starts_with("StreamLib Camera "), "{first}");
        assert_eq!(first.len(), "StreamLib Camera ".len() + 4, "four characters of id");
        assert_ne!(first, second, "two instances in one app never share a label");
        assert_ne!(first, other_app, "two apps never share a label");
        assert_eq!(
            first,
            camera_name_for(None, app, "VirtualCameraSink"),
            "the same app and instance get the same label on every run, which is what reclaim keys on"
        );
        assert_eq!(camera_name_for(Some("Desk cam"), app, "x"), "Desk cam");
        assert_eq!(
            camera_name_for(Some("   "), app, "VirtualCameraSink"),
            first,
            "a blank name is no name"
        );
        assert!(first.len() < 32, "the label fits the module's 32-byte field");
    }

    #[test]
    fn auto_takes_the_loopback_door_when_the_control_node_opens_and_refuses_naming_the_verb_otherwise() {
        assert_eq!(
            decide_door(VirtualCameraDoor::Auto, "Desk cam", ControlNodeAccess::Writable),
            DoorDecision::Loopback
        );
        for access in [ControlNodeAccess::Absent, ControlNodeAccess::NotWritable] {
            let DoorDecision::Refused(message) =
                decide_door(VirtualCameraDoor::Auto, "Desk cam", access)
            else {
                panic!("auto without permission refuses until the PipeWire door lands");
            };
            assert!(message.contains(ENABLE_VIRTUAL_CAMERA_VERB), "{message}");
            assert!(message.contains("PipeWire"), "auto says why it refuses today: {message}");
        }
    }

    #[test]
    fn a_forced_loopback_door_without_permission_refuses_naming_the_verb() {
        let DoorDecision::Refused(absent) =
            decide_door(VirtualCameraDoor::V4l2Loopback, "Desk cam", ControlNodeAccess::Absent)
        else {
            panic!("refused");
        };
        assert_eq!(
            absent,
            "VirtualCameraSink \"Desk cam\": no permission to create a v4l2loopback camera: \
             /dev/v4l2loopback is absent. Run `streamlib enable-virtual-camera` once (it asks \
             for your password), then re-run."
        );
        let DoorDecision::Refused(locked) = decide_door(
            VirtualCameraDoor::V4l2Loopback,
            "Desk cam",
            ControlNodeAccess::NotWritable,
        ) else {
            panic!("refused");
        };
        assert!(locked.contains("is not writable by this user."), "{locked}");
        assert_eq!(
            decide_door(VirtualCameraDoor::V4l2Loopback, "Desk cam", ControlNodeAccess::Writable),
            DoorDecision::Loopback
        );
    }

    /// A control node answering from a table: which numbers the module
    /// owns and what each is labelled, plus a log of what was added.
    struct FakeControlNode {
        devices: RefCell<Vec<(u32, String)>>,
        added: RefCell<Vec<String>>,
    }

    impl LoopbackControlNode for FakeControlNode {
        fn access(&self) -> ControlNodeAccess {
            ControlNodeAccess::Writable
        }
        fn candidate_device_numbers(&self) -> Vec<u32> {
            // The camera nodes the module does not own are in the scan too.
            let mut numbers = vec![0, 1, 2];
            numbers.extend(self.devices.borrow().iter().map(|(n, _)| *n));
            numbers.sort_unstable();
            numbers.dedup();
            numbers
        }
        fn query_device(&self, device_number: u32) -> Option<V4l2LoopbackConfig> {
            self.devices
                .borrow()
                .iter()
                .find(|(n, _)| *n == device_number)
                .map(|(n, label)| {
                    let mut config = V4l2LoopbackConfig::for_new_camera(label);
                    config.output_nr = *n as i32;
                    config
                })
        }
        fn add_device(&self, config: &V4l2LoopbackConfig) -> std::io::Result<u32> {
            self.added.borrow_mut().push(config.label());
            Ok(42)
        }
        fn remove_device(&self, _: u32) -> std::io::Result<()> {
            Ok(())
        }
        fn module_version(&self) -> Option<(u32, u32, u32)> {
            Some((0, 15, 3))
        }
    }

    #[test]
    fn a_device_carrying_this_sinks_label_is_reclaimed_rather_than_duplicated() {
        let node = FakeControlNode {
            devices: RefCell::new(vec![(10, "Other cam".into()), (11, "Desk cam".into())]),
            added: RefCell::new(Vec::new()),
        };
        assert_eq!(find_device_carrying_label(&node, "Desk cam"), Some(11));
        assert_eq!(
            find_device_carrying_label(&node, "Nobody's cam"),
            None,
            "a label nothing carries is a fresh CTL_ADD"
        );
        assert!(node.added.borrow().is_empty(), "a query adds nothing");
    }

    /// The module's ABI: the config struct's size and the four ioctl
    /// numbers, as `v4l2loopback.h` 0.15 declares them.
    #[test]
    fn the_control_config_struct_matches_the_modules_layout() {
        assert_eq!(std::mem::size_of::<V4l2LoopbackConfig>(), 72);
        assert_eq!(std::mem::align_of::<V4l2LoopbackConfig>(), 4);
        assert_eq!(V4L2LOOPBACK_CTL_VERSION, 0x8004_7e00);
        assert_eq!(V4L2LOOPBACK_CTL_ADD, 0x4048_7e01);
        assert_eq!(V4L2LOOPBACK_CTL_REMOVE, 0x4004_7e02);
        assert_eq!(V4L2LOOPBACK_CTL_QUERY, 0xc048_7e03);

        let config = V4l2LoopbackConfig::for_new_camera("Desk cam");
        assert_eq!(config.output_nr, -1, "the module picks the number");
        assert_eq!(config.max_buffers, 4, "Chrome asks for four");
        assert_eq!(config.announce_all_caps, 0, "capture-only to readers");
        assert_eq!(config.label(), "Desk cam");
        let long = V4l2LoopbackConfig::for_new_camera(&"x".repeat(40));
        assert_eq!(long.label().len(), 31, "a label keeps its NUL");
    }
}
