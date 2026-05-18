// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JPEG marker constants per ITU-T T.81 / ISO/IEC 10918-1 Table B.1.

pub const PREFIX: u8 = 0xFF;

pub const SOI: u8 = 0xD8;
pub const EOI: u8 = 0xD9;
pub const SOS: u8 = 0xDA;
pub const DQT: u8 = 0xDB;
pub const DHT: u8 = 0xC4;
pub const DRI: u8 = 0xDD;
pub const COM: u8 = 0xFE;

pub const APP0: u8 = 0xE0;
pub const APP1: u8 = 0xE1;
pub const APP2: u8 = 0xE2;
pub const APP14: u8 = 0xEE;

pub const SOF0: u8 = 0xC0;
pub const SOF1: u8 = 0xC1;
pub const SOF2: u8 = 0xC2;
pub const SOF3: u8 = 0xC3;
pub const SOF5: u8 = 0xC5;
pub const SOF6: u8 = 0xC6;
pub const SOF7: u8 = 0xC7;
pub const SOF9: u8 = 0xC9;
pub const SOF10: u8 = 0xCA;
pub const SOF11: u8 = 0xCB;
pub const SOF13: u8 = 0xCD;
pub const SOF14: u8 = 0xCE;
pub const SOF15: u8 = 0xCF;

pub const RST0: u8 = 0xD0;
pub const RST7: u8 = 0xD7;

pub fn is_app(marker: u8) -> bool {
    (0xE0..=0xEF).contains(&marker)
}

pub fn is_restart(marker: u8) -> bool {
    (RST0..=RST7).contains(&marker)
}

pub fn is_standalone(marker: u8) -> bool {
    matches!(marker, SOI | EOI) || is_restart(marker)
}

pub fn is_unsupported_sof(marker: u8) -> Option<&'static str> {
    match marker {
        SOF1 => Some("extended sequential DCT (SOF1)"),
        SOF2 => Some("progressive DCT (SOF2)"),
        SOF3 => Some("lossless sequential (SOF3)"),
        SOF5 => Some("differential sequential DCT (SOF5)"),
        SOF6 => Some("differential progressive DCT (SOF6)"),
        SOF7 => Some("differential lossless (SOF7)"),
        SOF9 => Some("arithmetic-coded extended sequential (SOF9)"),
        SOF10 => Some("arithmetic-coded progressive (SOF10)"),
        SOF11 => Some("arithmetic-coded lossless (SOF11)"),
        SOF13 => Some("differential arithmetic sequential (SOF13)"),
        SOF14 => Some("differential arithmetic progressive (SOF14)"),
        SOF15 => Some("differential arithmetic lossless (SOF15)"),
        _ => None,
    }
}

/// Zig-zag scan order from natural 8x8 row-major to JPEG sequential order
/// (ITU-T T.81 Figure A.6). Maps natural index -> zig-zag index.
pub const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10,
    17, 24, 32, 25, 18, 11, 4, 5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];
