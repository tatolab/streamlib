// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The ALSA PCMs a capture stream can open, read from libasound's device name hints.

use super::*;
use crate::core::context::AudioCaptureDeviceKind;

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

/// The capture devices among `hints`: each PCM that captures, `default` first and then in
/// libasound's order, named by its description.
pub(super) fn alsa_capture_devices_among(hints: Vec<AlsaPcmHint>) -> Vec<AudioCaptureDevice> {
    let mut capture_hints: Vec<AlsaPcmHint> = hints
        .into_iter()
        .filter(|hint| hint.direction.as_deref() != Some("Output"))
        .filter(|hint| hint.name != DISCARDING_PCM_NAME)
        .collect();
    let mut names_seen = std::collections::HashSet::new();
    capture_hints.retain(|hint| names_seen.insert(hint.name.clone()));
    if let Some(default_position) = capture_hints
        .iter()
        .position(|hint| hint.name == DEFAULT_PCM_NAME)
    {
        let default_hint = capture_hints.remove(default_position);
        capture_hints.insert(0, default_hint);
    }
    capture_hints
        .into_iter()
        .map(|hint| AudioCaptureDevice {
            name: hint
                .description
                .as_deref()
                .map(|description| {
                    description
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| hint.name.clone()),
            id: hint.name,
            kind: AudioCaptureDeviceKind::Input,
        })
        .collect()
}

/// Every PCM libasound's configuration and cards declare, as hints.
pub(super) fn pcm_hints_of(entry_points: &Arc<AlsaLibraryEntryPoints>) -> Result<Vec<AlsaPcmHint>> {
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
