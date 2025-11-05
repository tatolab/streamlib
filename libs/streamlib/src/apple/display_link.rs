//! CVDisplayLink integration for hardware-driven VSync timing
//!
//! This module provides a safe Rust wrapper around CoreVideo's CVDisplayLink API,
//! enabling hardware-synchronized video frame callbacks from the display.
//!
//! ## Purpose
//!
//! CVDisplayLink provides accurate, hardware-driven timing callbacks synchronized
//! to the display's vertical refresh (VSync). This is essential for:
//! - Smooth video playback without tearing
//! - Accurate frame rate measurement
//! - Synchronizing rendering to display refresh
//!
//! ## Architecture
//!
//! The DisplayLink wraps a VideoClock and increments it on every vsync callback:
//!
//! ```text
//! Display Hardware → CVDisplayLink → display_link_callback() → VideoClock::increment_frames()
//! ```
//!
//! The callback runs on a real-time priority thread managed by CoreVideo.
//!
//! ## Example
//!
//! ```rust,ignore
//! use streamlib::apple::display_link::{DisplayLink, get_main_display_refresh_rate};
//! use streamlib::core::clocks::VideoClock;
//! use std::sync::Arc;
//!
//! // Detect display refresh rate
//! let refresh_rate = get_main_display_refresh_rate()?;
//! println!("Display refresh rate: {:.2} Hz", refresh_rate);
//!
//! // Create video clock
//! let clock = Arc::new(VideoClock::new(refresh_rate, "Display VSync".to_string()));
//!
//! // Create and start DisplayLink
//! let display_link = DisplayLink::new(clock.clone())?;
//! display_link.start()?;
//!
//! // Clock is now being incremented by hardware vsync
//! // ...
//!
//! display_link.stop()?;
//! ```
//!
//! ## Thread Safety
//!
//! The callback runs on a separate real-time thread. The VideoClock uses atomic
//! operations for thread-safe frame counter updates.

use std::ffi::c_void;
use std::sync::Arc;
use crate::core::Result;
use crate::core::error::StreamError;
use crate::core::clocks::VideoClock;
use crate::core::clocks::Clock;

#[repr(C)]
pub struct CVDisplayLink {
    _private: [u8; 0],
}

pub type CVDisplayLinkRef = *mut CVDisplayLink;

#[repr(C)]
pub struct CVTimeStamp {
    pub version: u32,
    pub video_time_scale: i32,
    pub video_time: i64,
    pub host_time: u64,
    pub rate_scalar: f64,
    pub video_refresh_period: i64,
    pub smpte_time: CVSMPTETime,
    pub flags: u64,
    pub reserved: u64,
}

#[repr(C)]
pub struct CVSMPTETime {
    pub subframes: i16,
    pub subframe_divisor: i16,
    pub counter: u32,
    pub type_: u32,
    pub flags: u32,
    pub hours: i16,
    pub minutes: i16,
    pub seconds: i16,
    pub frames: i16,
}

pub type CVOptionFlags = u64;
pub type CVReturn = i32;

pub const K_CVRETURN_SUCCESS: CVReturn = 0;

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVDisplayLinkCreateWithActiveCGDisplays(displayLinkOut: *mut CVDisplayLinkRef) -> CVReturn;
    fn CVDisplayLinkSetOutputCallback(
        displayLink: CVDisplayLinkRef,
        callback: CVDisplayLinkOutputCallback,
        userInfo: *mut c_void,
    ) -> CVReturn;
    fn CVDisplayLinkStart(displayLink: CVDisplayLinkRef) -> CVReturn;
    fn CVDisplayLinkStop(displayLink: CVDisplayLinkRef) -> CVReturn;
    fn CVDisplayLinkRelease(displayLink: CVDisplayLinkRef);
    fn CVDisplayLinkGetNominalOutputVideoRefreshPeriod(displayLink: CVDisplayLinkRef) -> CVTime;
}

#[repr(C)]
pub struct CVTime {
    pub time_value: i64,
    pub time_scale: i32,
    pub flags: i32,
}

pub type CVDisplayLinkOutputCallback = extern "C" fn(
    displayLink: CVDisplayLinkRef,
    inNow: *const CVTimeStamp,
    inOutputTime: *const CVTimeStamp,
    flagsIn: CVOptionFlags,
    flagsOut: *mut CVOptionFlags,
    displayLinkContext: *mut c_void,
) -> CVReturn;

extern "C" fn display_link_callback(
    _display_link: CVDisplayLinkRef,
    _in_now: *const CVTimeStamp,
    _in_output_time: *const CVTimeStamp,
    _flags_in: CVOptionFlags,
    _flags_out: *mut CVOptionFlags,
    context: *mut c_void,
) -> CVReturn {
    unsafe {
        let clock = &*(context as *const Arc<VideoClock>);
        clock.increment_frames(1);
    }
    K_CVRETURN_SUCCESS
}

/// Safe wrapper around CVDisplayLink for hardware-driven vsync timing
///
/// DisplayLink connects a VideoClock to the display's vsync signal.
/// On each vertical refresh, the callback increments the clock's frame counter.
///
/// ## Memory Management
///
/// The clock is stored in `_clock_box` to ensure it stays alive as long as
/// the DisplayLink exists. The raw pointer passed to CVDisplayLink points
/// to this box's allocation.
pub struct DisplayLink {
    display_link: CVDisplayLinkRef,
    clock: Arc<VideoClock>,
    _clock_box: Box<Arc<VideoClock>>,
}

unsafe impl Send for DisplayLink {}

impl DisplayLink {
    /// Create a new DisplayLink for the active displays
    ///
    /// This creates a CVDisplayLink that fires callbacks synchronized to
    /// the vertical refresh of the active displays.
    ///
    /// # Arguments
    ///
    /// * `clock` - The VideoClock to increment on each vsync
    ///
    /// # Returns
    ///
    /// Returns a DisplayLink that is ready to start, or an error if
    /// CVDisplayLink creation fails.
    pub fn new(clock: Arc<VideoClock>) -> Result<Self> {
        let mut display_link: CVDisplayLinkRef = std::ptr::null_mut();

        let status = unsafe {
            CVDisplayLinkCreateWithActiveCGDisplays(&mut display_link)
        };

        if status != K_CVRETURN_SUCCESS {
            return Err(StreamError::Configuration(
                format!("Failed to create CVDisplayLink: {}", status)
            ));
        }

        let clock_box = Box::new(clock.clone());
        let context = Box::into_raw(clock_box) as *mut c_void;

        let status = unsafe {
            CVDisplayLinkSetOutputCallback(
                display_link,
                display_link_callback,
                context,
            )
        };

        if status != K_CVRETURN_SUCCESS {
            unsafe {
                CVDisplayLinkRelease(display_link);
                let _ = Box::from_raw(context as *mut Arc<VideoClock>);
            }
            return Err(StreamError::Configuration(
                format!("Failed to set CVDisplayLink callback: {}", status)
            ));
        }

        let clock_box = unsafe { Box::from_raw(context as *mut Arc<VideoClock>) };

        Ok(Self {
            display_link,
            clock: clock.clone(),
            _clock_box: clock_box,
        })
    }

    /// Start receiving vsync callbacks
    ///
    /// This activates the DisplayLink, causing the callback to be invoked
    /// on every vertical refresh. The callback runs on a high-priority
    /// real-time thread managed by CoreVideo.
    ///
    /// # Returns
    ///
    /// Returns Ok(()) if started successfully, or an error if CVDisplayLink
    /// fails to start.
    pub fn start(&self) -> Result<()> {
        let status = unsafe { CVDisplayLinkStart(self.display_link) };

        if status != K_CVRETURN_SUCCESS {
            return Err(StreamError::Configuration(
                format!("Failed to start CVDisplayLink: {}", status)
            ));
        }

        tracing::info!("CVDisplayLink started for clock: {}", self.clock.description());
        Ok(())
    }

    /// Stop receiving vsync callbacks
    ///
    /// This deactivates the DisplayLink. The callback will no longer be invoked.
    /// The clock's frame counter will stop incrementing.
    ///
    /// # Returns
    ///
    /// Returns Ok(()) if stopped successfully, or an error if CVDisplayLink
    /// fails to stop.
    pub fn stop(&self) -> Result<()> {
        let status = unsafe { CVDisplayLinkStop(self.display_link) };

        if status != K_CVRETURN_SUCCESS {
            return Err(StreamError::Configuration(
                format!("Failed to stop CVDisplayLink: {}", status)
            ));
        }

        tracing::info!("CVDisplayLink stopped for clock: {}", self.clock.description());
        Ok(())
    }

    /// Get the nominal refresh rate of the display
    ///
    /// Returns the refresh rate in Hz (e.g., 60.0, 120.0).
    /// Falls back to 60.0 if the refresh rate cannot be determined.
    ///
    /// # Returns
    ///
    /// The refresh rate in Hz.
    pub fn get_refresh_rate(&self) -> f64 {
        let refresh_period = unsafe {
            CVDisplayLinkGetNominalOutputVideoRefreshPeriod(self.display_link)
        };

        if refresh_period.time_scale == 0 {
            return 60.0;
        }

        refresh_period.time_scale as f64 / refresh_period.time_value as f64
    }
}

impl Drop for DisplayLink {
    fn drop(&mut self) {
        let _ = self.stop();
        unsafe {
            CVDisplayLinkRelease(self.display_link);
        }
    }
}

/// Get the refresh rate of the main display
///
/// This queries the CoreGraphics display system to determine the actual
/// refresh rate of the main display.
///
/// # Returns
///
/// Returns the refresh rate in Hz (60.0, 120.0, etc.), or 60.0 as a fallback
/// if the rate cannot be determined.
///
/// # Example
///
/// ```rust,ignore
/// use streamlib::apple::display_link::get_main_display_refresh_rate;
///
/// let rate = get_main_display_refresh_rate()?;
/// println!("Main display refresh rate: {:.2} Hz", rate);
/// ```
pub fn get_main_display_refresh_rate() -> Result<f64> {
    use core_graphics::display::CGDisplay;

    let main_display = CGDisplay::main();

    match main_display.display_mode() {
        Some(mode) => {
            let rate = mode.refresh_rate();
            if rate > 0.0 {
                Ok(rate)
            } else {
                tracing::warn!("Display reported 0 Hz, defaulting to 60 Hz");
                Ok(60.0)
            }
        }
        None => {
            tracing::warn!("Could not get display mode, defaulting to 60 Hz");
            Ok(60.0)
        }
    }
}
