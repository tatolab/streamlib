// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkVideoEncoderDef.h
//!
//! Core encoder definitions: utility functions (alignment, div-up, log2, GCD)
//! and the ConstQpSettings struct.

/// H.264 macroblock size alignment (16x16).
pub const H264_MB_SIZE_ALIGNMENT: u32 = 16;

/// Align `size` up to the given power-of-two `alignment`.
///
/// Equivalent to the C++ `AlignSize` template.
#[inline]
pub fn align_size<T>(size: T, alignment: T) -> T
where
    T: Copy
        + std::ops::Add<Output = T>
        + std::ops::Sub<Output = T>
        + std::ops::BitAnd<Output = T>
        + std::ops::Not<Output = T>
        + From<u8>,
{
    let one: T = T::from(1u8);
    (size + alignment - one) & !(alignment - one)
}

/// Integer division rounding up: `(value + divisor - 1) / divisor`.
///
/// Equivalent to the C++ `DivUp` template.
#[inline]
pub fn div_up<T>(value: T, divisor: T) -> T
where
    T: Copy
        + std::ops::Add<Output = T>
        + std::ops::Sub<Output = T>
        + std::ops::Div<Output = T>
        + From<u8>,
{
    let one: T = T::from(1u8);
    (value + (divisor - one)) / divisor
}

/// Fast integer log2 (number of bits needed to represent `val`).
///
/// Returns 0 for `val == 0`, matching the C++ behavior where the while-loop
/// body is never entered.
///
/// Equivalent to the C++ `FastIntLog2` template.
#[inline]
pub fn fast_int_log2(mut val: u32) -> u32 {
    let mut log2: u32 = 0;
    while val != 0 {
        val >>= 1;
        log2 += 1;
    }
    log2
}

/// Integer absolute value using the bit-twiddling trick from the C++ source.
///
/// Equivalent to the C++ `IntAbs` template (for signed types).
#[inline]
pub fn int_abs(x: i32) -> i32 {
    let in_bits = (std::mem::size_of::<i32>() * 8) as i32 - 1;
    let y = x >> in_bits;
    (x ^ y) - y
}

/// Greatest common divisor (Euclidean subtraction algorithm).
///
/// Equivalent to the C++ `Gcd` template.
pub fn gcd(mut u: u32, mut v: u32) -> u32 {
    if u <= 1 || v <= 1 {
        return 1;
    }
    while u != 0 {
        if u >= v {
            u -= v;
        } else {
            v -= u;
        }
    }
    v
}

/// QP values for I, P, and B frames (constant-QP mode).
///
/// Equivalent to the C++ `ConstQpSettings` struct.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConstQpSettings {
    pub qp_inter_p: u32,
    pub qp_inter_b: u32,
    pub qp_intra: u32,
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_align_size() {
        assert_eq!(align_size(0u32, 16), 0);
        assert_eq!(align_size(1u32, 16), 16);
        assert_eq!(align_size(15u32, 16), 16);
        assert_eq!(align_size(16u32, 16), 16);
        assert_eq!(align_size(17u32, 16), 32);
        assert_eq!(align_size(1920u32, 16), 1920);
        assert_eq!(align_size(1921u32, 16), 1936);
    }

    #[test]
    fn test_div_up() {
        assert_eq!(div_up(0u32, 16), 0);
        assert_eq!(div_up(1u32, 16), 1);
        assert_eq!(div_up(15u32, 16), 1);
        assert_eq!(div_up(16u32, 16), 1);
        assert_eq!(div_up(17u32, 16), 2);
        assert_eq!(div_up(1920u32, 16), 120);
        assert_eq!(div_up(1080u32, 16), 68);
    }

    #[test]
    fn test_fast_int_log2() {
        assert_eq!(fast_int_log2(0), 0);
        assert_eq!(fast_int_log2(1), 1);
        assert_eq!(fast_int_log2(2), 2);
        assert_eq!(fast_int_log2(3), 2);
        assert_eq!(fast_int_log2(4), 3);
        assert_eq!(fast_int_log2(255), 8);
        assert_eq!(fast_int_log2(256), 9);
    }

    #[test]
    fn test_int_abs() {
        assert_eq!(int_abs(0), 0);
        assert_eq!(int_abs(5), 5);
        assert_eq!(int_abs(-5), 5);
        assert_eq!(int_abs(i32::MAX), i32::MAX);
        // Note: int_abs(i32::MIN) overflows just like the C++ version.
    }

    #[test]
    fn test_gcd() {
        assert_eq!(gcd(0, 5), 1);
        assert_eq!(gcd(1, 5), 1);
        assert_eq!(gcd(12, 8), 4);
        assert_eq!(gcd(8, 12), 4);
        assert_eq!(gcd(1920, 1080), 120);
        assert_eq!(gcd(17, 13), 1);
    }

    #[test]
    fn test_const_qp_settings_default() {
        let qp = ConstQpSettings::default();
        assert_eq!(qp.qp_inter_p, 0);
        assert_eq!(qp.qp_inter_b, 0);
        assert_eq!(qp.qp_intra, 0);
    }
}
