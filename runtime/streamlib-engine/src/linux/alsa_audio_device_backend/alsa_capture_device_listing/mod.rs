// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The ALSA PCMs a capture stream can open, read from libasound's device name hints.

use super::*;
use crate::core::context::{AudioCaptureDeviceKind, moved_to_the_front};

/// One PCM as `snd_device_name_hint` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AlsaPcmHint {
    /// `NAME`: what `snd_pcm_open` opens.
    pub(super) name: String,
    /// `DESC`: lines a person reads, `None` where the hint carries none.
    pub(super) description: Option<String>,
    /// `IOID`: `Input` or `Output` for a one-way PCM, `None` for one that goes both ways.
    pub(super) direction: Option<String>,
}

/// `null`: a PCM that discards what is played and captures nothing, which no person picks as
/// an input.
const DISCARDING_PCM_NAME: &str = "null";

/// What `default` is called in the list. libasound's own description of it reads "Default ALSA
/// Output" on most machines, which misnames the PCM a capture stream opens.
const DEFAULT_PCM_LISTED_NAME: &str = "Default input";

/// The prefixes of the raw card PCMs. Each card's `sysdefault` and `dsnoop` PCMs already list
/// it, in a form more than one stream can open at once, so these are left out of the list (they
/// still open by id).
const RAW_CARD_PCM_PREFIXES: [&str; 2] = ["hw:", "plughw:"];

/// The capture devices among `hints`: each PCM that captures, but the raw card ones, `default`
/// first and then in libasound's order, named by its description.
pub(super) fn alsa_capture_devices_among(hints: Vec<AlsaPcmHint>) -> Vec<AudioCaptureDevice> {
    let mut names_seen = std::collections::HashSet::new();
    let capture_hints: Vec<AlsaPcmHint> = hints
        .into_iter()
        .filter(|hint| hint.direction.as_deref() != Some("Output"))
        .filter(|hint| hint.name != DISCARDING_PCM_NAME)
        .filter(|hint| {
            !RAW_CARD_PCM_PREFIXES
                .iter()
                .any(|prefix| hint.name.starts_with(prefix))
        })
        .filter(|hint| names_seen.insert(hint.name.clone()))
        .collect();
    moved_to_the_front(capture_hints, |hint| hint.name == DEFAULT_PCM_NAME)
        .into_iter()
        .map(|hint| {
            let name = if hint.name == DEFAULT_PCM_NAME {
                Some(DEFAULT_PCM_LISTED_NAME.to_owned())
            } else {
                hint.description.as_deref().map(|description| {
                    description
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
            };
            AudioCaptureDevice::named_or_by_id(hint.name, name, AudioCaptureDeviceKind::Input)
        })
        .collect()
}

/// Every PCM libasound's configuration and cards declare, as hints.
pub(super) fn pcm_hints_of(entry_points: &AlsaLibraryEntryPoints) -> Result<Vec<AlsaPcmHint>> {
    let mut hints: *mut *mut c_void = std::ptr::null_mut();
    // SAFETY: card -1 asks for every card; the interface is a NUL-terminated literal and `hints`
    // an owned out-parameter.
    let return_code =
        unsafe { (entry_points.snd_device_name_hint)(-1, c"pcm".as_ptr(), &raw mut hints) };
    entry_points.refuse_a_negative_return_code(format_args!("listing its PCMs"), return_code)?;
    if hints.is_null() {
        return Ok(Vec::new());
    }
    let mut read = Vec::new();
    let mut cursor = hints;
    // SAFETY: libasound ends the hint array with a null entry, and each entry stays valid until
    // the array is freed below.
    unsafe {
        while !(*cursor).is_null() {
            let hint = (*cursor).cast_const();
            if let Some(name) = hint_field(entry_points, hint, c"NAME") {
                read.push(AlsaPcmHint {
                    name,
                    description: hint_field(entry_points, hint, c"DESC"),
                    direction: hint_field(entry_points, hint, c"IOID"),
                });
            }
            cursor = cursor.add(1);
        }
        (entry_points.snd_device_name_free_hint)(hints);
    }
    Ok(read)
}

/// One field of a hint, copied out of the string libasound allocated for it.
///
/// # Safety
///
/// `hint` must be an entry of a hint array that has not been freed.
unsafe fn hint_field(
    entry_points: &AlsaLibraryEntryPoints,
    hint: *const c_void,
    field: &CStr,
) -> Option<String> {
    // SAFETY: the caller's contract; the field name is a NUL-terminated literal.
    let value = unsafe { (entry_points.snd_device_name_get_hint)(hint, field.as_ptr()) };
    if value.is_null() {
        return None;
    }
    // SAFETY: non-null is a NUL-terminated string libasound allocated with `malloc` for the
    // caller to free.
    let copied = unsafe { CStr::from_ptr(value) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: the string is the caller's to free, and is freed once.
    unsafe { libc::free(value.cast()) };
    Some(copied)
}

#[cfg(test)]
mod tests;
