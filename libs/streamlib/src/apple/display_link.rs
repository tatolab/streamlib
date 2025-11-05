
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

pub struct DisplayLink {
    display_link: CVDisplayLinkRef,
    clock: Arc<VideoClock>,
    _clock_box: Box<Arc<VideoClock>>,
}

unsafe impl Send for DisplayLink {}

impl DisplayLink {
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
