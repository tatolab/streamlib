// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A muted, private Core Audio process tap of this test process's own output,
//! inside a private aggregate device that carries only the tap: the capture
//! device a CoreAudio content test opens by UID to hear its own playback
//! digitally while nothing reaches a speaker.
//!
//! Both objects are private to this process and die with it, so a crashed run
//! strands nothing. Reading the tap needs System Audio Recording allowed for
//! the application macOS holds responsible for this process; without it the
//! tap delivers exact digital zeros and no error.
//!
//! Beside them, a read of the default output device's volume, which decides
//! whether a tap reading at unity gain sits before that volume or after it.

use std::ffi::CStr;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use objc2::AnyThread;
use objc2_core_audio::{
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectHasProperty,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectPropertyElement,
    AudioObjectPropertyScope, AudioObjectPropertySelector, CATapDescription, CATapMuteBehavior,
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioDevicePropertyPreferredChannelsForStereo,
    kAudioDevicePropertyStreams, kAudioDevicePropertyVolumeDecibels,
    kAudioDevicePropertyVolumeScalar, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDevices, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    kAudioObjectUnknown, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    kAudioTapPropertyFormat,
};
use objc2_core_audio_types::AudioStreamBasicDescription;
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_foundation::{NSArray, NSNumber, NSString, NSUUID};

/// `noErr`.
const NO_ERR: i32 = 0;

/// The HAL does not promise that the device list a capture stream resolves
/// UIDs from holds a new aggregate by the time the create call returns.
const AGGREGATE_DEVICE_PUBLICATION_DEADLINE: Duration = Duration::from_secs(5);

const AGGREGATE_DEVICE_PUBLICATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// A muted process tap of this process, and the private aggregate device a
/// capture stream opens it through. Dropping it destroys both, aggregate
/// first; the process is audible again from then on, so a caller drops every
/// playback stream before this.
pub struct MutedProcessTapOfThisProcessBehindAPrivateAggregateDevice {
    /// Declared before the tap so it is destroyed first: an aggregate must
    /// not outlive a tap it lists.
    aggregate_device_over_the_tap: PrivateAggregateDeviceOverOneProcessTap,
    muted_process_tap: MutedPrivateProcessTapOfThisProcess,
}

impl MutedProcessTapOfThisProcessBehindAPrivateAggregateDevice {
    /// Mute this process's audio output into a new private tap, and publish a
    /// private aggregate device carrying only that tap.
    pub fn create() -> Self {
        let muted_process_tap = MutedPrivateProcessTapOfThisProcess::create();
        let aggregate_device_over_the_tap =
            PrivateAggregateDeviceOverOneProcessTap::create_over(&muted_process_tap);
        Self {
            aggregate_device_over_the_tap,
            muted_process_tap,
        }
    }

    /// The CoreAudio UID a capture stream names to read the tap.
    pub fn aggregate_device_uid(&self) -> &str {
        &self.aggregate_device_over_the_tap.aggregate_device_uid
    }

    /// The format the tap mixes this process's output down to.
    pub fn tap_stream_format(&self) -> Option<AudioStreamBasicDescription> {
        self.muted_process_tap.tap_stream_format()
    }
}

/// A private process tap of this process with mute behaviour `Muted`, which
/// Core Audio documents as keeping everything this process plays off the
/// audio hardware from creation until the tap is destroyed.
struct MutedPrivateProcessTapOfThisProcess {
    tap_object_id: AudioObjectID,
    tap_uuid: String,
}

impl MutedPrivateProcessTapOfThisProcess {
    fn create() -> Self {
        let this_process_audio_object_id = this_process_audio_object_id();
        let tapped_processes =
            NSArray::from_retained_slice(&[NSNumber::new_u32(this_process_audio_object_id)]);
        let tap_uuid = NSUUID::new();
        // SAFETY: a freshly allocated description initialised with an array
        // of process object IDs, as the initialiser requires.
        let tap_description = unsafe {
            CATapDescription::initStereoMixdownOfProcesses(
                CATapDescription::alloc(),
                &tapped_processes,
            )
        };
        // SAFETY: plain property setters on a live description.
        unsafe {
            tap_description.setName(&NSString::from_str(&format!(
                "streamlib test tap of pid {}",
                std::process::id()
            )));
            tap_description.setUUID(&tap_uuid);
            tap_description.setPrivate(true);
            tap_description.setMuteBehavior(CATapMuteBehavior::Muted);
        }
        let mut tap_object_id: AudioObjectID = kAudioObjectUnknown;
        // SAFETY: a live description and a writable object ID.
        let status =
            unsafe { AudioHardwareCreateProcessTap(Some(&tap_description), &mut tap_object_id) };
        assert!(
            status == NO_ERR && tap_object_id != kAudioObjectUnknown,
            "AudioHardwareCreateProcessTap refused a muted private stereo tap of this process \
             (audio object {this_process_audio_object_id}): {}",
            osstatus_text(status)
        );
        Self {
            tap_object_id,
            tap_uuid: tap_uuid.UUIDString().to_string(),
        }
    }

    fn tap_stream_format(&self) -> Option<AudioStreamBasicDescription> {
        // SAFETY: `AudioStreamBasicDescription` is plain data for which all-zero
        // is a valid value.
        let mut tap_stream_format: AudioStreamBasicDescription = unsafe { std::mem::zeroed() };
        let mut byte_count = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
        let address = property_address(kAudioTapPropertyFormat, kAudioObjectPropertyScopeGlobal);
        // SAFETY: the destination is a writable description of the size passed.
        let status = unsafe {
            AudioObjectGetPropertyData(
                self.tap_object_id,
                NonNull::from(&address),
                0,
                std::ptr::null(),
                NonNull::from(&mut byte_count),
                NonNull::from(&mut tap_stream_format).cast(),
            )
        };
        (status == NO_ERR).then_some(tap_stream_format)
    }
}

impl Drop for MutedPrivateProcessTapOfThisProcess {
    fn drop(&mut self) {
        // SAFETY: a tap this process created and has not destroyed.
        let status = unsafe { AudioHardwareDestroyProcessTap(self.tap_object_id) };
        if !std::thread::panicking() {
            assert_eq!(
                status,
                NO_ERR,
                "AudioHardwareDestroyProcessTap: {}",
                osstatus_text(status)
            );
        }
    }
}

/// A private aggregate device whose only member is one process tap, so its
/// one input stream is the tap's mixdown.
struct PrivateAggregateDeviceOverOneProcessTap {
    aggregate_device_object_id: AudioObjectID,
    aggregate_device_uid: String,
}

impl PrivateAggregateDeviceOverOneProcessTap {
    fn create_over(muted_process_tap: &MutedPrivateProcessTapOfThisProcess) -> Self {
        let aggregate_device_uid =
            format!("streamlib-test-muted-process-tap-{}", std::process::id());
        let aggregate_device_name = format!(
            "StreamLib test: muted process tap of pid {}",
            std::process::id()
        );
        let one = CFNumber::new_i32(1);

        let tap_uuid = CFString::from_str(&muted_process_tap.tap_uuid);
        let tap_list_entry = CFDictionary::<CFString, CFType>::from_slices(
            &[
                &*cf_string_of_hal_key(kAudioSubTapUIDKey),
                &*cf_string_of_hal_key(kAudioSubTapDriftCompensationKey),
            ],
            &[&tap_uuid, &one],
        );
        let tap_list = CFArray::from_retained_objects(&[tap_list_entry]);

        let uid = CFString::from_str(&aggregate_device_uid);
        let name = CFString::from_str(&aggregate_device_name);
        let composition = CFDictionary::<CFString, CFType>::from_slices(
            &[
                &*cf_string_of_hal_key(kAudioAggregateDeviceUIDKey),
                &*cf_string_of_hal_key(kAudioAggregateDeviceNameKey),
                &*cf_string_of_hal_key(kAudioAggregateDeviceIsPrivateKey),
                &*cf_string_of_hal_key(kAudioAggregateDeviceTapAutoStartKey),
                &*cf_string_of_hal_key(kAudioAggregateDeviceTapListKey),
            ],
            &[&uid, &name, &one, &one, &tap_list],
        );

        let mut aggregate_device_object_id: AudioObjectID = kAudioObjectUnknown;
        // SAFETY: a composition dictionary of the documented key and value
        // types, and a writable object ID.
        let status = unsafe {
            AudioHardwareCreateAggregateDevice(
                composition.as_opaque(),
                NonNull::from(&mut aggregate_device_object_id),
            )
        };
        assert!(
            status == NO_ERR && aggregate_device_object_id != kAudioObjectUnknown,
            "AudioHardwareCreateAggregateDevice refused a private aggregate over tap {}: {}",
            muted_process_tap.tap_uuid,
            osstatus_text(status)
        );
        let aggregate_device = Self {
            aggregate_device_object_id,
            aggregate_device_uid,
        };
        aggregate_device.wait_until_this_process_lists_it_with_an_input_stream();
        aggregate_device
    }

    /// Block until the device list a capture stream resolves UIDs from holds
    /// the aggregate, carrying the tap's input stream.
    fn wait_until_this_process_lists_it_with_an_input_stream(&self) {
        let deadline = Instant::now() + AGGREGATE_DEVICE_PUBLICATION_DEADLINE;
        loop {
            let is_listed = audio_object_id_list_property(
                kAudioObjectSystemObject as AudioObjectID,
                kAudioHardwarePropertyDevices,
                kAudioObjectPropertyScopeGlobal,
            )
            .contains(&self.aggregate_device_object_id);
            let has_an_input_stream = !audio_object_id_list_property(
                self.aggregate_device_object_id,
                kAudioDevicePropertyStreams,
                kAudioObjectPropertyScopeInput,
            )
            .is_empty();
            if is_listed && has_an_input_stream {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the private aggregate '{}' was created but, after {:?}, this process {} — a \
                 capture stream cannot resolve it by UID",
                self.aggregate_device_uid,
                AGGREGATE_DEVICE_PUBLICATION_DEADLINE,
                if is_listed {
                    "lists it with no input stream"
                } else {
                    "does not list it among its audio devices"
                }
            );
            std::thread::sleep(AGGREGATE_DEVICE_PUBLICATION_POLL_INTERVAL);
        }
    }
}

impl Drop for PrivateAggregateDeviceOverOneProcessTap {
    fn drop(&mut self) {
        // SAFETY: an aggregate this process created and has not destroyed.
        let status =
            unsafe { AudioHardwareDestroyAggregateDevice(self.aggregate_device_object_id) };
        if !std::thread::panicking() {
            assert_eq!(
                status,
                NO_ERR,
                "AudioHardwareDestroyAggregateDevice: {}",
                osstatus_text(status)
            );
        }
    }
}

/// One volume control of the default output device, as the HAL reported it.
struct OutputVolumeControlReading {
    /// `kAudioObjectPropertyElementMain` for the device's main volume,
    /// otherwise the channel it scales.
    volume_control_element: AudioObjectPropertyElement,
    volume_scalar: f32,
    volume_decibels: Option<f32>,
}

/// The default output device's volume, read without changing it: the level a
/// tap placed after the device's volume would carry this process's output at.
pub struct DefaultOutputDeviceVolumeControls {
    default_output_device_object_id: Option<AudioObjectID>,
    /// The device's main control or, where it has none, those of the two
    /// channels macOS plays stereo on. Empty on a device without a volume.
    volume_control_readings: Vec<OutputVolumeControlReading>,
}

impl DefaultOutputDeviceVolumeControls {
    /// Read the default output device's volume controls. A property query: it
    /// raises no prompt and changes nothing.
    pub fn read() -> Self {
        let Some(default_output_device_object_id) = fixed_size_property::<AudioObjectID>(
            kAudioObjectSystemObject as AudioObjectID,
            property_address(
                kAudioHardwarePropertyDefaultOutputDevice,
                kAudioObjectPropertyScopeGlobal,
            ),
        )
        .filter(|&object_id| object_id != kAudioObjectUnknown) else {
            return Self {
                default_output_device_object_id: None,
                volume_control_readings: Vec::new(),
            };
        };
        let main_volume_control_reading = output_volume_control_reading(
            default_output_device_object_id,
            kAudioObjectPropertyElementMain,
        );
        let volume_control_readings = match main_volume_control_reading {
            Some(main_volume_control_reading) => vec![main_volume_control_reading],
            None => fixed_size_property::<[u32; 2]>(
                default_output_device_object_id,
                property_address(
                    kAudioDevicePropertyPreferredChannelsForStereo,
                    kAudioObjectPropertyScopeOutput,
                ),
            )
            .unwrap_or([1, 2])
            .into_iter()
            .filter_map(|channel| {
                output_volume_control_reading(default_output_device_object_id, channel)
            })
            .collect(),
        };
        Self {
            default_output_device_object_id: Some(default_output_device_object_id),
            volume_control_readings,
        }
    }
}

impl std::fmt::Display for DefaultOutputDeviceVolumeControls {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Some(default_output_device_object_id) = self.default_output_device_object_id else {
            return formatter.write_str("unreadable: CoreAudio names no default output device");
        };
        if self.volume_control_readings.is_empty() {
            return write!(
                formatter,
                "none: output device {default_output_device_object_id} has no volume control"
            );
        }
        for (index, reading) in self.volume_control_readings.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            if reading.volume_control_element == kAudioObjectPropertyElementMain {
                formatter.write_str("main")?;
            } else {
                write!(formatter, "channel {}", reading.volume_control_element)?;
            }
            write!(formatter, " scalar {:.3}", reading.volume_scalar)?;
            if let Some(volume_decibels) = reading.volume_decibels {
                write!(formatter, " ({volume_decibels:+.2} dB)")?;
            }
        }
        write!(
            formatter,
            " on output device {default_output_device_object_id}"
        )
    }
}

fn output_volume_control_reading(
    output_device_object_id: AudioObjectID,
    volume_control_element: AudioObjectPropertyElement,
) -> Option<OutputVolumeControlReading> {
    let volume_property_address = |selector| AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeOutput,
        mElement: volume_control_element,
    };
    let volume_scalar = fixed_size_property::<f32>(
        output_device_object_id,
        volume_property_address(kAudioDevicePropertyVolumeScalar),
    )?;
    Some(OutputVolumeControlReading {
        volume_control_element,
        volume_scalar,
        volume_decibels: fixed_size_property::<f32>(
            output_device_object_id,
            volume_property_address(kAudioDevicePropertyVolumeDecibels),
        ),
    })
}

/// A property whose value is one `T` of integers or floats, for which every
/// bit pattern the HAL writes is valid; `None` when the object lacks it.
fn fixed_size_property<T: Default + Copy>(
    object_id: AudioObjectID,
    address: AudioObjectPropertyAddress,
) -> Option<T> {
    // SAFETY: `address` is a live property address.
    if !unsafe { AudioObjectHasProperty(object_id, NonNull::from(&address)) } {
        return None;
    }
    let mut value = T::default();
    let mut byte_count = std::mem::size_of::<T>() as u32;
    // SAFETY: the destination is a writable `T` of the size passed, and every
    // bit pattern is a valid `T`.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut byte_count),
            NonNull::from(&mut value).cast(),
        )
    };
    (status == NO_ERR && byte_count as usize == std::mem::size_of::<T>()).then_some(value)
}

/// The audio object CoreAudio keeps for this process, which a tap names to
/// capture it.
fn this_process_audio_object_id() -> AudioObjectID {
    let this_process_id = std::process::id() as i32;
    let address = property_address(
        kAudioHardwarePropertyTranslatePIDToProcessObject,
        kAudioObjectPropertyScopeGlobal,
    );
    let mut process_object_id: AudioObjectID = kAudioObjectUnknown;
    let mut byte_count = std::mem::size_of::<AudioObjectID>() as u32;
    // SAFETY: the qualifier is a `pid_t` of the size passed, and the
    // destination a writable object ID of the size passed.
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            std::mem::size_of::<i32>() as u32,
            (&this_process_id as *const i32).cast(),
            NonNull::from(&mut byte_count),
            NonNull::from(&mut process_object_id).cast(),
        )
    };
    assert!(
        status == NO_ERR && process_object_id != kAudioObjectUnknown,
        "CoreAudio has no process object for this process (pid {this_process_id}), so nothing \
         can tap it: {}",
        osstatus_text(status)
    );
    process_object_id
}

fn property_address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn audio_object_id_list_property(
    object_id: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Vec<AudioObjectID> {
    let address = property_address(selector, scope);
    let mut byte_count = 0u32;
    // SAFETY: `byte_count` is a writable `u32`.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object_id,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut byte_count),
        )
    };
    if status != NO_ERR {
        return Vec::new();
    }
    let mut object_ids =
        vec![kAudioObjectUnknown; byte_count as usize / std::mem::size_of::<AudioObjectID>()];
    if object_ids.is_empty() {
        return object_ids;
    }
    // SAFETY: `object_ids` holds `byte_count` writable bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut byte_count),
            NonNull::from(object_ids.as_mut_slice()).cast(),
        )
    };
    if status != NO_ERR {
        return Vec::new();
    }
    object_ids.truncate(byte_count as usize / std::mem::size_of::<AudioObjectID>());
    object_ids
}

/// The HAL spells its dictionary keys as C string macros; a `CFDictionary`
/// wants them as `CFString`s.
fn cf_string_of_hal_key(hal_key: &CStr) -> CFRetained<CFString> {
    CFString::from_str(
        hal_key
            .to_str()
            .expect("CoreAudio's dictionary keys are ASCII"),
    )
}

fn osstatus_text(status: i32) -> String {
    let four_char_code = status.to_be_bytes();
    if four_char_code.iter().all(|byte| byte.is_ascii_graphic()) {
        format!(
            "OSStatus {status} ('{}')",
            String::from_utf8_lossy(&four_char_code)
        )
    } else {
        format!("OSStatus {status}")
    }
}
