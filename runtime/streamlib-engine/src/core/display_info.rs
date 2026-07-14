// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-platform display refresh-rate query.

use winit::window::Window;

/// Default fallback refresh rate when the platform query fails or the
/// driver reports an unusable value.
pub const DEFAULT_REFRESH_RATE_HZ: f64 = 60.0;

/// Refresh rate of the monitor this window belongs to, in Hz.
///
/// On Linux the rate comes from `winit::MonitorHandle::refresh_rate_millihertz`
/// (returns `None` on Wayland compositors that do not advertise a rate —
/// fallback applied). On macOS / iOS the rate comes from the main display
/// via `CGDisplayModeGetRefreshRate` and the `window` argument is ignored
/// (CoreGraphics is global, no per-window query needed).
#[cfg(target_os = "linux")]
pub fn get_refresh_rate(window: Option<&Window>) -> f64 {
    let monitor = window
        .and_then(|w| w.current_monitor())
        .or_else(|| window.and_then(|w| w.available_monitors().next()));
    match monitor.and_then(|m| m.refresh_rate_millihertz()) {
        Some(mhz) if mhz > 0 => f64::from(mhz) / 1000.0,
        _ => {
            tracing::warn!(
                "display_info: monitor refresh rate unavailable; falling back to {DEFAULT_REFRESH_RATE_HZ} Hz"
            );
            DEFAULT_REFRESH_RATE_HZ
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple_link {
    use std::ffi::c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGMainDisplayID() -> u32;
        pub fn CGDisplayCopyDisplayMode(display: u32) -> *const c_void;
        pub fn CGDisplayModeGetRefreshRate(mode: *const c_void) -> f64;
        pub fn CGDisplayModeRelease(mode: *const c_void);
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub fn get_refresh_rate(_window: Option<&Window>) -> f64 {
    use apple_link::*;
    unsafe {
        let display_id = CGMainDisplayID();
        let mode = CGDisplayCopyDisplayMode(display_id);
        if mode.is_null() {
            return DEFAULT_REFRESH_RATE_HZ;
        }
        let rate = CGDisplayModeGetRefreshRate(mode);
        CGDisplayModeRelease(mode);
        if rate <= 0.0 {
            DEFAULT_REFRESH_RATE_HZ
        } else {
            rate
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
pub fn get_refresh_rate(_window: Option<&Window>) -> f64 {
    DEFAULT_REFRESH_RATE_HZ
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_refresh_rate_is_60() {
        assert_eq!(DEFAULT_REFRESH_RATE_HZ, 60.0);
    }

    #[test]
    fn get_refresh_rate_with_no_window_returns_positive() {
        // Linux: warns + falls back to 60. macOS: queries main display.
        // Either way the result must be a positive Hz value.
        let rate = get_refresh_rate(None);
        assert!(rate > 0.0, "refresh rate must be > 0, got {rate}");
    }
}
