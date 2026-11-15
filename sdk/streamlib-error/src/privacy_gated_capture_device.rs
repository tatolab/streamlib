// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

/// A capture device the platform gates behind a privacy prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PrivacyGatedCaptureDevice {
    Camera,
    Microphone,
}

impl PrivacyGatedCaptureDevice {
    /// The device as a sentence names it mid-way.
    pub fn lowercase_name(self) -> &'static str {
        match self {
            Self::Camera => "camera",
            Self::Microphone => "microphone",
        }
    }

    /// The device as the platform's privacy settings list it.
    pub fn privacy_setting_name(self) -> &'static str {
        match self {
            Self::Camera => "Camera",
            Self::Microphone => "Microphone",
        }
    }

    /// What starts flowing once the user allows it.
    pub fn what_starts_once_allowed(self) -> &'static str {
        match self {
            Self::Camera => "Frames start",
            Self::Microphone => "Audio starts",
        }
    }
}

impl std::fmt::Display for PrivacyGatedCaptureDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.lowercase_name())
    }
}
