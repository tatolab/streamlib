use crate::core::{Result, StreamError};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// Opaque CVDisplayLink type
#[repr(C)]
struct CVDisplayLink {
    _opaque: [u8; 0],
}

type CVDisplayLinkRef = *mut CVDisplayLink;

const kCVReturnSuccess: i32 = 0;

// CVTimeStamp structure (simplified - we don't actually use it)
#[repr(C)]
struct CVTimeStamp {
    _data: [u8; 120], // Actual size of CVTimeStamp
}

type CVDisplayLinkOutputCallback = extern "C-unwind" fn(
    display_link: CVDisplayLinkRef,
    in_now: *const CVTimeStamp,
    in_output_time: *const CVTimeStamp,
    flags_in: u64,
    flags_out: *mut u64,
    display_link_context: *mut std::ffi::c_void,
) -> i32;

/// Display link callback context
struct DisplayLinkContext {
    /// Flag indicating a frame is ready
    frame_ready: Arc<AtomicBool>,
}

/// CVDisplayLink wrapper for vsync-synchronized loop mode
///
/// This provides hardware-synchronized frame pacing for video processing.
/// Instead of sleeping for arbitrary durations, we wait for the display's
/// vertical blanking interval, ensuring smooth frame delivery.
pub struct DisplayLink {
    display_link: CVDisplayLinkRef,
    frame_ready: Arc<AtomicBool>,
    context_ptr: *mut std::ffi::c_void,  // Track context for cleanup
}

unsafe impl Send for DisplayLink {}
unsafe impl Sync for DisplayLink {}

impl DisplayLink {
    /// Create a new display link for the main display
    ///
    /// Note: This must be called from the main thread to access NSScreen
    pub fn new() -> Result<Self> {
        unsafe {
            // Create display link
            let mut display_link: CVDisplayLinkRef = std::ptr::null_mut();
            let result = CVDisplayLinkCreateWithActiveCGDisplays(&mut display_link);

            if result != kCVReturnSuccess {
                return Err(StreamError::Runtime(format!(
                    "Failed to create CVDisplayLink: error code {}", result
                )));
            }

            // Set up callback context
            let frame_ready = Arc::new(AtomicBool::new(false));
            let context = Box::new(DisplayLinkContext {
                frame_ready: frame_ready.clone(),
            });

            let context_ptr = Box::into_raw(context) as *mut std::ffi::c_void;

            // Set callback
            let result = CVDisplayLinkSetOutputCallback(
                display_link,
                display_link_callback,
                context_ptr,
            );

            if result != kCVReturnSuccess {
                // Clean up
                CVDisplayLinkRelease(display_link);
                let _ = Box::from_raw(context_ptr as *mut DisplayLinkContext);
                return Err(StreamError::Runtime(format!(
                    "Failed to set CVDisplayLink callback: error code {}", result
                )));
            }

            Ok(Self {
                display_link,
                frame_ready,
                context_ptr,
            })
        }
    }

    /// Start the display link
    pub fn start(&self) -> Result<()> {
        unsafe {
            let result = CVDisplayLinkStart(self.display_link);
            if result != kCVReturnSuccess {
                return Err(StreamError::Runtime(format!(
                    "Failed to start CVDisplayLink: error code {}", result
                )));
            }
        }
        Ok(())
    }

    /// Stop the display link
    pub fn stop(&self) -> Result<()> {
        unsafe {
            let result = CVDisplayLinkStop(self.display_link);
            if result != kCVReturnSuccess {
                return Err(StreamError::Runtime(format!(
                    "Failed to stop CVDisplayLink: error code {}", result
                )));
            }
        }
        Ok(())
    }

    /// Wait for the next frame (vsync)
    ///
    /// This blocks until the display link callback signals a frame is ready.
    /// Provides hardware-synchronized timing for smooth video playback.
    pub fn wait_for_frame(&self) {
        // Spin-wait for frame ready flag
        // This is fine because CVDisplayLink runs at display refresh rate (60Hz typical)
        // so we only wait ~16ms max
        while !self.frame_ready.swap(false, Ordering::Acquire) {
            std::hint::spin_loop();
        }
    }

    /// Check if running
    pub fn is_running(&self) -> bool {
        unsafe {
            CVDisplayLinkIsRunning(self.display_link)
        }
    }

    /// Get the nominal output video refresh period
    pub fn get_nominal_output_video_refresh_period(&self) -> Result<std::time::Duration> {
        unsafe {
            let time = CVDisplayLinkGetNominalOutputVideoRefreshPeriod(self.display_link);

            // CVTime has timeValue (i64) and timeScale (i32)
            // Duration in seconds = timeValue / timeScale
            let duration_secs = time.timeValue as f64 / time.timeScale as f64;
            let duration = std::time::Duration::from_secs_f64(duration_secs);

            Ok(duration)
        }
    }
}

impl Drop for DisplayLink {
    fn drop(&mut self) {
        unsafe {
            // Stop if running
            if self.is_running() {
                let _ = self.stop();
            }

            // Free context
            if !self.context_ptr.is_null() {
                let _ = Box::from_raw(self.context_ptr as *mut DisplayLinkContext);
            }

            // Release display link
            CVDisplayLinkRelease(self.display_link);
        }
    }
}

/// CVDisplayLink callback - called on every vsync
extern "C-unwind" fn display_link_callback(
    _display_link: CVDisplayLinkRef,
    _in_now: *const CVTimeStamp,
    _in_output_time: *const CVTimeStamp,
    _flags_in: u64,
    _flags_out: *mut u64,
    display_link_context: *mut std::ffi::c_void,
) -> i32 {
    unsafe {
        let context = &*(display_link_context as *const DisplayLinkContext);
        // Signal that a frame is ready
        context.frame_ready.store(true, Ordering::Release);
    }
    kCVReturnSuccess
}

// External C declarations for CVDisplayLink functions
extern "C" {
    fn CVDisplayLinkCreateWithActiveCGDisplays(display_link_out: *mut CVDisplayLinkRef) -> i32;
    fn CVDisplayLinkSetOutputCallback(
        display_link: CVDisplayLinkRef,
        callback: CVDisplayLinkOutputCallback,
        user_info: *mut std::ffi::c_void,
    ) -> i32;
    fn CVDisplayLinkStart(display_link: CVDisplayLinkRef) -> i32;
    fn CVDisplayLinkStop(display_link: CVDisplayLinkRef) -> i32;
    fn CVDisplayLinkIsRunning(display_link: CVDisplayLinkRef) -> bool;
    fn CVDisplayLinkRelease(display_link: CVDisplayLinkRef);
    fn CVDisplayLinkGetNominalOutputVideoRefreshPeriod(display_link: CVDisplayLinkRef) -> CVTime;
}

#[repr(C)]
struct CVTime {
    timeValue: i64,
    timeScale: i32,
    flags: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires display hardware
    fn test_display_link_creation() {
        let display_link = DisplayLink::new();
        // May fail on CI without display
        if let Ok(dl) = display_link {
            assert!(!dl.is_running());
        }
    }

    #[test]
    #[ignore] // Requires display hardware
    fn test_display_link_start_stop() {
        let display_link = DisplayLink::new();
        if let Ok(dl) = display_link {
            assert!(dl.start().is_ok());
            assert!(dl.is_running());
            assert!(dl.stop().is_ok());
            assert!(!dl.is_running());
        }
    }

    #[test]
    #[ignore] // Requires display hardware
    fn test_display_link_vsync() {
        use std::time::Instant;

        let display_link = DisplayLink::new();
        if let Ok(dl) = display_link {
            dl.start().unwrap();

            let start = Instant::now();

            // Wait for 5 frames
            for _ in 0..5 {
                dl.wait_for_frame();
            }

            let elapsed = start.elapsed();

            // At 60Hz, 5 frames should take ~83ms (16.67ms per frame)
            // Allow some margin: 60-100ms
            assert!(elapsed.as_millis() >= 60 && elapsed.as_millis() <= 100,
                "5 frames took {:?}, expected ~83ms", elapsed);

            dl.stop().unwrap();
        }
    }
}
