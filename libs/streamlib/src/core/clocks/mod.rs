//! Clock abstraction for timing and synchronization
//!
//! Following GStreamer's GstClock pattern, clocks provide **passive time references**
//! for sources and sinks to synchronize against.
//!
//! ## Design Philosophy
//!
//! Clocks are NOT active schedulers - they don't wake processors or generate events.
//! Instead, they provide a `now()` method that processors can query:
//!
//! - **Sources** check: "Is it time to push this buffer yet?"
//! - **Sinks** check: "Is it time to render this buffer yet?"
//! - **Transforms** don't use clocks (purely reactive to data)
//!
//! ## Clock Types
//!
//! - **AudioClock**: Hardware-driven (CoreAudio callback updates sample counter)
//! - **VideoClock**: Hardware-driven (CVDisplayLink updates frame counter)
//! - **SoftwareClock**: CPU timestamps for fallback
//! - **PTPClock**: IEEE 1588 network sync (future)
//! - **GenlockClock**: SDI hardware sync (future)
//!
//! ## Usage in Sources
//!
//! ```rust,ignore
//! // Runtime spawns source loop with clock reference
//! loop {
//!     let frame = source.generate()?;
//!     let sync_point = source.clock_sync_point();
//!
//!     // Wait for clock to reach target time
//!     let now = clock.now_ns();
//!     if now < next_sync_time_ns {
//!         sleep(next_sync_time_ns - now);
//!     }
//!
//!     source.write_output(frame);
//! }
//! ```
//!
//! ## Usage in Sinks
//!
//! ```rust,ignore
//! fn render(&mut self, frame: VideoFrame) {
//!     // Wait until presentation time
//!     let now = self.clock.now_ns();
//!     if now < frame.timestamp_ns {
//!         sleep(frame.timestamp_ns - now);
//!     }
//!
//!     // Render to hardware
//!     self.display.render(frame);
//! }
//! ```

mod clock_trait;
mod software_clock;
mod audio_clock;
mod video_clock;
mod ptp_clock;
mod genlock_clock;

pub use clock_trait::Clock;
pub use software_clock::SoftwareClock;
pub use audio_clock::AudioClock;
pub use video_clock::VideoClock;
pub use ptp_clock::PTPClock;
pub use genlock_clock::GenlockClock;
