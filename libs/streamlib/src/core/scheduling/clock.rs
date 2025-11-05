//! Clock sources and synchronization
//!
//! This module previously contained ClockSource, ClockType, and SyncMode enums.
//! These have been removed as they were unused legacy code. The Clock trait
//! implementations (AudioClock, VideoClock, etc.) now only provide timing via
//! their now() method, with no type categorization or sync mode logic.
