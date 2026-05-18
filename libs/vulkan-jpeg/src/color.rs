// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JPEG color-metadata types parsed from APP segments.
//!
//! Parsed-as-bytes inside the crate's marker walker and surfaced on
//! [`crate::DecodedJpeg::color_info`]. Resolution to the engine's
//! `streamlib::sdk::color::ResolvedColorInfo` lives in
//! [`JpegColorInfo::resolve`] (Linux-only).

use crate::error::{JpegError, JpegResult};

/// Aggregate of every color-related field extracted from the JPEG's
/// application segments. Each `Option` is present iff the matching
/// segment was found and parsed cleanly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JpegColorInfo {
    /// APP0 JFIF marker. Present whenever an `APP0` segment carries
    /// the `"JFIF\0"` identifier — the canonical signal that the
    /// stream is JFIF-conformant (BT.601-Full YCbCr ↔ sRGB primaries).
    pub jfif: Option<JfifMetadata>,
    /// APP14 Adobe marker. Carries the YCbCr↔RGB transform field that
    /// overrides JFIF's implicit BT.601-Full conversion.
    pub adobe: Option<AdobeMetadata>,
    /// APP1 EXIF `ColorSpace` tag (0xA001) when present. Other EXIF
    /// fields are ignored — the issue scopes this to colorimetry.
    pub exif_color_space: Option<ExifColorSpace>,
    /// APP2 ICC profile bytes. Concatenated across multi-segment
    /// chunks per the ICC profile spec's APP2 fragmentation scheme.
    /// `None` until every declared chunk has been seen.
    pub icc_profile: Option<Vec<u8>>,
}

/// Parsed APP0 JFIF identifier segment.
///
/// JFIF freezes the YCbCr↔RGB conversion to BT.601-Full and sRGB
/// primaries; presence of this segment is the strongest "you can
/// trust the JFIF defaults" signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JfifMetadata {
    /// JFIF version, major.minor (e.g. `(1, 2)` for "JFIF 1.02").
    pub version: (u8, u8),
}

/// Parsed APP14 Adobe segment.
///
/// The `transform` field — known historically as the Adobe "color
/// transform" byte — is what tells a decoder whether the 3-component
/// stream is RGB-direct (no YCbCr↔RGB matrix), YCbCr (matrix), or
/// YCCK (4-component CMYK; out of scope for a 3-component kernel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdobeMetadata {
    /// Adobe transform byte (segment offset 11).
    pub transform: AdobeTransform,
}

/// Adobe APP14 `transform` field values per the Adobe TN5116 spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdobeTransform {
    /// `transform = 0`. RGB-direct for 3-component streams (no
    /// YCbCr↔RGB matrix); CMYK-direct for 4-component streams.
    Direct,
    /// `transform = 1`. YCbCr (3-component) — matches JFIF's
    /// implicit BT.601-Full default.
    YCbCr,
    /// `transform = 2`. YCCK (4-component CMYK encoded as YCCK).
    /// The fused JPEG kernel only handles 3-component YCbCr today —
    /// a 4-component decode would need separate dequant/IDCT plumbing.
    YCCK,
    /// Any value outside `{0, 1, 2}`. Per the spec the field is a
    /// `u8`, so we surface the raw byte without rejecting parse —
    /// the resolver can decide how to treat it.
    Other(u8),
}

impl AdobeTransform {
    fn from_byte(b: u8) -> Self {
        match b {
            0 => AdobeTransform::Direct,
            1 => AdobeTransform::YCbCr,
            2 => AdobeTransform::YCCK,
            other => AdobeTransform::Other(other),
        }
    }
}

/// EXIF `ColorSpace` tag (0xA001) value.
///
/// EXIF (TIFF) tag values are `u16`. The well-known interpretations:
/// `1` = sRGB, `0xFFFF` = Uncalibrated (often paired with an
/// `InteropIndex` of "R03" to mean Adobe RGB). Anything else is
/// surfaced as `Other` rather than guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExifColorSpace {
    Srgb,
    Uncalibrated,
    Other(u16),
}

impl ExifColorSpace {
    pub(crate) fn from_u16(v: u16) -> Self {
        match v {
            1 => ExifColorSpace::Srgb,
            0xFFFF => ExifColorSpace::Uncalibrated,
            other => ExifColorSpace::Other(other),
        }
    }
}

// Identifier bytes that precede each APP-segment payload kind.
// Kept private — these are parser internals.
pub(crate) const JFIF_IDENTIFIER: &[u8] = b"JFIF\0";
pub(crate) const EXIF_IDENTIFIER: &[u8] = b"Exif\0\0";
pub(crate) const ICC_IDENTIFIER: &[u8] = b"ICC_PROFILE\0";
pub(crate) const ADOBE_IDENTIFIER: &[u8] = b"Adobe\0";

// ---------------------------------------------------------------------------
// APP0 — JFIF
// ---------------------------------------------------------------------------

pub(crate) fn parse_app0(payload: &[u8]) -> JpegResult<Option<JfifMetadata>> {
    if !payload.starts_with(JFIF_IDENTIFIER) {
        // APP0 can also carry JFXX (JFIF extension) or other vendor
        // formats; treat anything non-JFIF as a no-op.
        return Ok(None);
    }
    if payload.len() < JFIF_IDENTIFIER.len() + 2 {
        return Err(JpegError::Unsupported("APP0 JFIF segment too short"));
    }
    let version_offset = JFIF_IDENTIFIER.len();
    let major = payload[version_offset];
    let minor = payload[version_offset + 1];
    Ok(Some(JfifMetadata {
        version: (major, minor),
    }))
}

// ---------------------------------------------------------------------------
// APP1 — EXIF (scoped to ColorSpace tag)
// ---------------------------------------------------------------------------

pub(crate) fn parse_app1_exif_color_space(payload: &[u8]) -> JpegResult<Option<ExifColorSpace>> {
    if !payload.starts_with(EXIF_IDENTIFIER) {
        // APP1 also carries XMP (`http://ns.adobe.com/xap/1.0/`)
        // and others — skip anything that isn't EXIF.
        return Ok(None);
    }
    let tiff = &payload[EXIF_IDENTIFIER.len()..];
    walk_exif_color_space(tiff)
}

#[derive(Debug, Clone, Copy)]
enum TiffByteOrder {
    LittleEndian,
    BigEndian,
}

impl TiffByteOrder {
    fn read_u16(&self, bytes: &[u8], offset: usize) -> JpegResult<u16> {
        if offset + 2 > bytes.len() {
            return Err(JpegError::Unsupported("EXIF: u16 read out of bounds"));
        }
        let slice = [bytes[offset], bytes[offset + 1]];
        Ok(match self {
            TiffByteOrder::LittleEndian => u16::from_le_bytes(slice),
            TiffByteOrder::BigEndian => u16::from_be_bytes(slice),
        })
    }

    fn read_u32(&self, bytes: &[u8], offset: usize) -> JpegResult<u32> {
        if offset + 4 > bytes.len() {
            return Err(JpegError::Unsupported("EXIF: u32 read out of bounds"));
        }
        let slice = [
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ];
        Ok(match self {
            TiffByteOrder::LittleEndian => u32::from_le_bytes(slice),
            TiffByteOrder::BigEndian => u32::from_be_bytes(slice),
        })
    }
}

const EXIF_TAG_EXIF_IFD_POINTER: u16 = 0x8769;
const EXIF_TAG_COLOR_SPACE: u16 = 0xA001;

/// Walk the TIFF/EXIF structure looking specifically for ColorSpace.
/// Bounded; bails on any malformed offset instead of panicking.
fn walk_exif_color_space(tiff: &[u8]) -> JpegResult<Option<ExifColorSpace>> {
    if tiff.len() < 8 {
        return Err(JpegError::Unsupported("EXIF: TIFF header too short"));
    }
    let order = match &tiff[0..2] {
        b"II" => TiffByteOrder::LittleEndian,
        b"MM" => TiffByteOrder::BigEndian,
        _ => return Err(JpegError::Unsupported("EXIF: missing II/MM byte-order")),
    };
    let magic = order.read_u16(tiff, 2)?;
    if magic != 0x002A {
        return Err(JpegError::Unsupported("EXIF: bad TIFF magic"));
    }
    let ifd0_offset = order.read_u32(tiff, 4)? as usize;
    // IFD0 — search for ExifIFDPointer (0x8769). ColorSpace lives in
    // the linked ExifIFD, not in IFD0.
    let exif_ifd_offset = match find_ifd_entry(tiff, order, ifd0_offset, EXIF_TAG_EXIF_IFD_POINTER)?
    {
        Some(entry) => match entry.read_u32_value(tiff, order) {
            Some(v) => v as usize,
            None => return Ok(None),
        },
        None => return Ok(None),
    };
    let color_space_entry =
        match find_ifd_entry(tiff, order, exif_ifd_offset, EXIF_TAG_COLOR_SPACE)? {
            Some(e) => e,
            None => return Ok(None),
        };
    let raw = match color_space_entry.read_u16_value(tiff, order) {
        Some(v) => v,
        None => return Ok(None),
    };
    Ok(Some(ExifColorSpace::from_u16(raw)))
}

#[derive(Debug, Clone, Copy)]
struct IfdEntry {
    field_type: u16,
    count: u32,
    /// Raw 4 bytes of the value-or-offset slot, in the file's byte order
    /// (kept as a raw `[u8; 4]` to defer interpretation to the reader).
    value_offset_bytes: [u8; 4],
}

impl IfdEntry {
    fn read_u32_value(&self, _tiff: &[u8], order: TiffByteOrder) -> Option<u32> {
        // Type LONG (4) with count 1 fits inline.
        if self.field_type != 4 || self.count != 1 {
            return None;
        }
        Some(match order {
            TiffByteOrder::LittleEndian => u32::from_le_bytes(self.value_offset_bytes),
            TiffByteOrder::BigEndian => u32::from_be_bytes(self.value_offset_bytes),
        })
    }

    fn read_u16_value(&self, _tiff: &[u8], order: TiffByteOrder) -> Option<u16> {
        // Type SHORT (3) with count 1 fits inline in the low half of the
        // 4-byte slot.
        if self.field_type != 3 || self.count != 1 {
            return None;
        }
        let slice = [self.value_offset_bytes[0], self.value_offset_bytes[1]];
        Some(match order {
            TiffByteOrder::LittleEndian => u16::from_le_bytes(slice),
            TiffByteOrder::BigEndian => u16::from_be_bytes(slice),
        })
    }
}

fn find_ifd_entry(
    tiff: &[u8],
    order: TiffByteOrder,
    ifd_offset: usize,
    target_tag: u16,
) -> JpegResult<Option<IfdEntry>> {
    if ifd_offset == 0 || ifd_offset + 2 > tiff.len() {
        return Ok(None);
    }
    let count = order.read_u16(tiff, ifd_offset)? as usize;
    let entries_start = ifd_offset + 2;
    let entries_end = entries_start
        .checked_add(count * 12)
        .ok_or(JpegError::Unsupported("EXIF: IFD count overflow"))?;
    if entries_end > tiff.len() {
        return Err(JpegError::Unsupported("EXIF: IFD truncated"));
    }
    for i in 0..count {
        let base = entries_start + i * 12;
        let tag = order.read_u16(tiff, base)?;
        if tag != target_tag {
            continue;
        }
        let field_type = order.read_u16(tiff, base + 2)?;
        let count_u32 = order.read_u32(tiff, base + 4)?;
        let value_offset_bytes = [
            tiff[base + 8],
            tiff[base + 9],
            tiff[base + 10],
            tiff[base + 11],
        ];
        let _ = tag; // matched by predicate above
        return Ok(Some(IfdEntry {
            field_type,
            count: count_u32,
            value_offset_bytes,
        }));
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// APP2 — ICC profile (multi-segment reassembly)
// ---------------------------------------------------------------------------

/// Per-fragment view extracted from an APP2 ICC segment.
pub(crate) struct IccFragment<'a> {
    /// 1-based fragment index.
    pub seq: u8,
    /// Total fragment count.
    pub total: u8,
    /// Fragment payload bytes (no identifier, no seq/total bytes).
    pub data: &'a [u8],
}

pub(crate) fn parse_app2_icc_fragment(payload: &[u8]) -> JpegResult<Option<IccFragment<'_>>> {
    if !payload.starts_with(ICC_IDENTIFIER) {
        return Ok(None);
    }
    let rest = &payload[ICC_IDENTIFIER.len()..];
    if rest.len() < 2 {
        return Err(JpegError::Unsupported("APP2 ICC: missing seq/total bytes"));
    }
    let seq = rest[0];
    let total = rest[1];
    if seq == 0 || total == 0 || seq > total {
        return Err(JpegError::Unsupported(
            "APP2 ICC: invalid seq/total ordering",
        ));
    }
    Ok(Some(IccFragment {
        seq,
        total,
        data: &rest[2..],
    }))
}

/// Multi-segment ICC reassembly state. Fragments arrive in arbitrary
/// order; the accumulator stores them by 1-based index and assembles
/// once the last one is in.
#[derive(Debug, Default)]
pub(crate) struct IccAccumulator {
    /// Expected fragment count (set on first fragment). Resets only
    /// when the value would otherwise conflict — a malformed source
    /// changing `total` mid-stream is treated as a hard error rather
    /// than silently overwriting.
    expected_total: Option<u8>,
    /// Fragments collected so far (`fragments[i] = data for seq=i+1`).
    fragments: Vec<Option<Vec<u8>>>,
}

impl IccAccumulator {
    pub(crate) fn ingest(&mut self, fragment: IccFragment<'_>) -> JpegResult<()> {
        let total = match self.expected_total {
            None => {
                self.expected_total = Some(fragment.total);
                self.fragments = (0..fragment.total).map(|_| None).collect();
                fragment.total
            }
            Some(t) if t == fragment.total => t,
            Some(_) => {
                return Err(JpegError::Unsupported(
                    "APP2 ICC: fragment `total` disagrees across segments",
                ));
            }
        };
        if fragment.seq > total {
            return Err(JpegError::Unsupported(
                "APP2 ICC: fragment seq exceeds declared total",
            ));
        }
        let slot = &mut self.fragments[fragment.seq as usize - 1];
        if slot.is_some() {
            return Err(JpegError::Unsupported(
                "APP2 ICC: duplicate fragment seq",
            ));
        }
        *slot = Some(fragment.data.to_vec());
        Ok(())
    }

    pub(crate) fn try_finish(&mut self) -> Option<Vec<u8>> {
        self.expected_total?;
        if self.fragments.iter().any(|f| f.is_none()) {
            return None;
        }
        let mut out = Vec::new();
        for slot in self.fragments.drain(..) {
            out.extend(slot.expect("checked above"));
        }
        self.expected_total = None;
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// APP14 — Adobe
// ---------------------------------------------------------------------------

pub(crate) fn parse_app14_adobe(payload: &[u8]) -> JpegResult<Option<AdobeMetadata>> {
    if !payload.starts_with(ADOBE_IDENTIFIER) {
        return Ok(None);
    }
    // Adobe segment layout after "Adobe\0":
    //   u16 version (BE), u16 flags0, u16 flags1, u8 transform.
    // The transform byte sits at offset 11 from the start of the
    // segment payload (5 + 1 + 2 + 2 + 2 = 12, transform is the 12th
    // byte / offset 11).
    const TRANSFORM_OFFSET: usize = ADOBE_IDENTIFIER.len() + 6;
    if payload.len() <= TRANSFORM_OFFSET {
        return Err(JpegError::Unsupported(
            "APP14 Adobe segment truncated before transform byte",
        ));
    }
    let transform = AdobeTransform::from_byte(payload[TRANSFORM_OFFSET]);
    Ok(Some(AdobeMetadata { transform }))
}

// ---------------------------------------------------------------------------
// Resolution to engine `ResolvedColorInfo` (Linux-only)
// ---------------------------------------------------------------------------

/// Outcome of [`JpegColorInfo::resolve`].
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
pub struct ResolvedJpegColor {
    /// Engine-shaped 4-tuple the kernel consumes via push constants.
    pub info: streamlib::sdk::color::ResolvedColorInfo,
    /// Why this resolution was picked — useful for logging and tests
    /// that need to verify which branch fired without inspecting the
    /// numeric 4-tuple.
    pub source: JpegColorSource,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JpegColorSource {
    /// No APP-segment metadata honored — JFIF default
    /// (BT.601-Full YCbCr ↔ sRGB primaries).
    JfifDefault,
    /// APP14 Adobe `transform = 0` → RGB-direct (matrix collapses to
    /// `Identity`; no YCbCr↔RGB conversion).
    AdobeRgbDirect,
    /// APP14 Adobe `transform = 1` → JFIF-compatible YCbCr.
    AdobeYCbCr,
    /// APP1 EXIF `ColorSpace = sRGB`. Same resolved 4-tuple as
    /// `JfifDefault`; surfaced separately so callers can tell the
    /// metadata was actually inspected.
    ExifSrgb,
    /// Parsed metadata declared a colorimetry the engine can't yet
    /// represent (Adobe RGB / Display P3 / Rec.2020 via EXIF or ICC).
    /// Falls back to JFIF default.
    UnsupportedDeclarationFallback,
}

#[cfg(target_os = "linux")]
impl JpegColorInfo {
    /// Resolve to the engine 4-tuple that drives the kernel's push
    /// constants. JFIF default when no metadata is honored; APP14
    /// `transform = 0` swaps the matrix axis to `Identity`. YCCK is
    /// rejected with a typed error (the 4-component decode path
    /// doesn't exist today).
    pub fn resolve(&self) -> JpegResult<ResolvedJpegColor> {
        use streamlib::sdk::color::{MatrixId, PrimariesId, RangeId, ResolvedColorInfo, TransferId};

        let jfif_default = ResolvedColorInfo {
            primaries: PrimariesId::Bt709,
            transfer: TransferId::Srgb,
            matrix: MatrixId::Smpte170m,
            range: RangeId::Full,
        };

        if let Some(adobe) = self.adobe {
            match adobe.transform {
                AdobeTransform::Direct => {
                    return Ok(ResolvedJpegColor {
                        info: ResolvedColorInfo {
                            matrix: MatrixId::Identity,
                            range: RangeId::Full,
                            ..jfif_default
                        },
                        source: JpegColorSource::AdobeRgbDirect,
                    });
                }
                AdobeTransform::YCbCr => {
                    return Ok(ResolvedJpegColor {
                        info: jfif_default,
                        source: JpegColorSource::AdobeYCbCr,
                    });
                }
                AdobeTransform::YCCK => {
                    return Err(JpegError::Unsupported(
                        "APP14 Adobe transform=2 (YCCK / 4-component CMYK) — the 3-component fused kernel cannot handle this stream",
                    ));
                }
                AdobeTransform::Other(_) => {
                    return Ok(ResolvedJpegColor {
                        info: jfif_default,
                        source: JpegColorSource::UnsupportedDeclarationFallback,
                    });
                }
            }
        }

        if let Some(cs) = self.exif_color_space {
            return Ok(match cs {
                ExifColorSpace::Srgb => ResolvedJpegColor {
                    info: jfif_default,
                    source: JpegColorSource::ExifSrgb,
                },
                ExifColorSpace::Uncalibrated | ExifColorSpace::Other(_) => ResolvedJpegColor {
                    info: jfif_default,
                    source: JpegColorSource::UnsupportedDeclarationFallback,
                },
            });
        }

        if self.icc_profile.is_some() {
            // The ICC primaries→`PrimariesId` map needs `Adobe RGB` /
            // `Display P3` variants that the engine enum doesn't carry
            // today. Parse, surface, fall back to JFIF — a future engine
            // extension is the right place to honor non-sRGB profiles.
            return Ok(ResolvedJpegColor {
                info: jfif_default,
                source: JpegColorSource::UnsupportedDeclarationFallback,
            });
        }

        // No metadata honored — JFIF default applies whether or not an
        // explicit APP0 JFIF segment was present.
        Ok(ResolvedJpegColor {
            info: jfif_default,
            source: JpegColorSource::JfifDefault,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// APP0 JFIF identifier round-trips into [`JfifMetadata`].
    #[test]
    fn parse_app0_jfif_basic() {
        let mut payload = Vec::new();
        payload.extend_from_slice(JFIF_IDENTIFIER);
        payload.extend_from_slice(&[1, 2]); // version 1.02
        payload.extend_from_slice(&[0, 0, 1, 0, 1, 0, 0]); // density + thumbnail dims
        let result = parse_app0(&payload).unwrap();
        assert_eq!(result, Some(JfifMetadata { version: (1, 2) }));
    }

    /// Non-JFIF APP0 (e.g. JFXX) → `None`, no error.
    #[test]
    fn parse_app0_non_jfif_returns_none() {
        let payload = b"JFXX\x00..."[..].to_vec();
        assert_eq!(parse_app0(&payload).unwrap(), None);
    }

    /// APP14 Adobe transform=1 (YCbCr) round-trips.
    #[test]
    fn parse_app14_ycbcr() {
        let payload = build_adobe_segment(1);
        let result = parse_app14_adobe(&payload).unwrap();
        assert_eq!(
            result,
            Some(AdobeMetadata {
                transform: AdobeTransform::YCbCr
            })
        );
    }

    /// APP14 Adobe transform=0 (RGB direct) round-trips.
    #[test]
    fn parse_app14_rgb_direct() {
        let payload = build_adobe_segment(0);
        let result = parse_app14_adobe(&payload).unwrap();
        assert_eq!(
            result,
            Some(AdobeMetadata {
                transform: AdobeTransform::Direct
            })
        );
    }

    /// APP14 Adobe transform=2 (YCCK) round-trips.
    #[test]
    fn parse_app14_ycck() {
        let payload = build_adobe_segment(2);
        let result = parse_app14_adobe(&payload).unwrap();
        assert_eq!(
            result,
            Some(AdobeMetadata {
                transform: AdobeTransform::YCCK
            })
        );
    }

    /// Non-Adobe APP14 payload → `None`, no error.
    #[test]
    fn parse_app14_non_adobe_returns_none() {
        let payload = b"Other\x00..."[..].to_vec();
        assert_eq!(parse_app14_adobe(&payload).unwrap(), None);
    }

    /// EXIF ColorSpace=1 (sRGB) extracted from a little-endian TIFF
    /// directory.
    #[test]
    fn parse_app1_exif_color_space_srgb_little_endian() {
        let payload = build_exif_payload_color_space(1, /* big_endian */ false);
        let result = parse_app1_exif_color_space(&payload).unwrap();
        assert_eq!(result, Some(ExifColorSpace::Srgb));
    }

    /// EXIF ColorSpace=0xFFFF (Uncalibrated) extracted from a
    /// big-endian TIFF directory. Catches byte-order parsing
    /// regressions.
    #[test]
    fn parse_app1_exif_color_space_uncalibrated_big_endian() {
        let payload = build_exif_payload_color_space(0xFFFF, /* big_endian */ true);
        let result = parse_app1_exif_color_space(&payload).unwrap();
        assert_eq!(result, Some(ExifColorSpace::Uncalibrated));
    }

    /// XMP-flavored APP1 (non-EXIF) → `None`.
    #[test]
    fn parse_app1_non_exif_returns_none() {
        let payload = b"http://ns.adobe.com/xap/1.0/\x00..."[..].to_vec();
        assert_eq!(parse_app1_exif_color_space(&payload).unwrap(), None);
    }

    /// Single-segment ICC profile reassembles into the original bytes.
    #[test]
    fn icc_single_segment_reassembles() {
        let body = b"my fake ICC profile bytes".to_vec();
        let payload = build_icc_segment(1, 1, &body);
        let fragment = parse_app2_icc_fragment(&payload).unwrap().unwrap();
        let mut accum = IccAccumulator::default();
        accum.ingest(fragment).unwrap();
        assert_eq!(accum.try_finish(), Some(body));
    }

    /// Two-segment ICC profile reassembles in delivery order.
    #[test]
    fn icc_two_segments_reassemble_in_order() {
        let a = b"part-A".to_vec();
        let b = b"part-B".to_vec();
        let payload_a = build_icc_segment(1, 2, &a);
        let payload_b = build_icc_segment(2, 2, &b);
        let mut accum = IccAccumulator::default();
        accum
            .ingest(parse_app2_icc_fragment(&payload_a).unwrap().unwrap())
            .unwrap();
        assert!(accum.try_finish().is_none(), "incomplete after 1 of 2");
        accum
            .ingest(parse_app2_icc_fragment(&payload_b).unwrap().unwrap())
            .unwrap();
        let combined = accum.try_finish().expect("complete after 2 of 2");
        assert_eq!(combined, [a, b].concat());
    }

    /// Two-segment ICC profile reassembles when fragments arrive in
    /// reverse order — exercises out-of-order indexing.
    #[test]
    fn icc_two_segments_reassemble_out_of_order() {
        let a = b"part-A".to_vec();
        let b = b"part-B".to_vec();
        let payload_a = build_icc_segment(1, 2, &a);
        let payload_b = build_icc_segment(2, 2, &b);
        let mut accum = IccAccumulator::default();
        accum
            .ingest(parse_app2_icc_fragment(&payload_b).unwrap().unwrap())
            .unwrap();
        accum
            .ingest(parse_app2_icc_fragment(&payload_a).unwrap().unwrap())
            .unwrap();
        let combined = accum.try_finish().expect("complete");
        assert_eq!(combined, [a, b].concat());
    }

    /// Duplicate fragment seq → typed error rather than silent overwrite.
    #[test]
    fn icc_duplicate_fragment_rejected() {
        let payload = build_icc_segment(1, 2, b"x");
        let mut accum = IccAccumulator::default();
        accum
            .ingest(parse_app2_icc_fragment(&payload).unwrap().unwrap())
            .unwrap();
        let dup = parse_app2_icc_fragment(&payload).unwrap().unwrap();
        let err = accum.ingest(dup);
        assert!(matches!(err, Err(JpegError::Unsupported(_))));
    }

    // -----------------------------------------------------------------
    // Resolution tests (Linux-only — depend on streamlib::sdk::color).
    // -----------------------------------------------------------------

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_empty_color_info_returns_jfif_default() {
        use streamlib::sdk::color::{MatrixId, PrimariesId, RangeId, TransferId};
        let info = JpegColorInfo::default();
        let resolved = info.resolve().expect("resolve");
        assert_eq!(resolved.source, JpegColorSource::JfifDefault);
        assert_eq!(resolved.info.primaries, PrimariesId::Bt709);
        assert_eq!(resolved.info.transfer, TransferId::Srgb);
        assert_eq!(resolved.info.matrix, MatrixId::Smpte170m);
        assert_eq!(resolved.info.range, RangeId::Full);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_adobe_rgb_direct_collapses_matrix_to_identity() {
        use streamlib::sdk::color::MatrixId;
        let info = JpegColorInfo {
            adobe: Some(AdobeMetadata {
                transform: AdobeTransform::Direct,
            }),
            ..Default::default()
        };
        let resolved = info.resolve().expect("resolve");
        assert_eq!(resolved.source, JpegColorSource::AdobeRgbDirect);
        assert_eq!(resolved.info.matrix, MatrixId::Identity);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_adobe_ycbcr_equals_jfif_default() {
        use streamlib::sdk::color::MatrixId;
        let info = JpegColorInfo {
            adobe: Some(AdobeMetadata {
                transform: AdobeTransform::YCbCr,
            }),
            ..Default::default()
        };
        let resolved = info.resolve().expect("resolve");
        assert_eq!(resolved.source, JpegColorSource::AdobeYCbCr);
        assert_eq!(resolved.info.matrix, MatrixId::Smpte170m);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_adobe_ycck_returns_typed_error() {
        let info = JpegColorInfo {
            adobe: Some(AdobeMetadata {
                transform: AdobeTransform::YCCK,
            }),
            ..Default::default()
        };
        let err = info.resolve().expect_err("YCCK must be rejected");
        assert!(matches!(err, JpegError::Unsupported(_)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_exif_srgb_marks_source_explicitly() {
        let info = JpegColorInfo {
            exif_color_space: Some(ExifColorSpace::Srgb),
            ..Default::default()
        };
        let resolved = info.resolve().expect("resolve");
        assert_eq!(resolved.source, JpegColorSource::ExifSrgb);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_exif_uncalibrated_falls_back_with_unsupported_source() {
        let info = JpegColorInfo {
            exif_color_space: Some(ExifColorSpace::Uncalibrated),
            ..Default::default()
        };
        let resolved = info.resolve().expect("resolve");
        assert_eq!(
            resolved.source,
            JpegColorSource::UnsupportedDeclarationFallback
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_icc_profile_present_falls_back_with_unsupported_source() {
        let info = JpegColorInfo {
            icc_profile: Some(vec![0u8; 128]),
            ..Default::default()
        };
        let resolved = info.resolve().expect("resolve");
        assert_eq!(
            resolved.source,
            JpegColorSource::UnsupportedDeclarationFallback
        );
    }

    // -----------------------------------------------------------------
    // Test fixture builders — exposed for cross-module integration use
    // via `pub(crate)` so the parser-level tests in `parser.rs` can
    // re-use them.
    // -----------------------------------------------------------------

    pub(super) fn build_adobe_segment(transform: u8) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(ADOBE_IDENTIFIER); // "Adobe\0"
        payload.extend_from_slice(&[0x00, 0x65]); // version (BE)
        payload.extend_from_slice(&[0x00, 0x00]); // flags0
        payload.extend_from_slice(&[0x00, 0x00]); // flags1
        payload.push(transform);
        payload
    }

    pub(super) fn build_icc_segment(seq: u8, total: u8, body: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(ICC_IDENTIFIER);
        payload.push(seq);
        payload.push(total);
        payload.extend_from_slice(body);
        payload
    }

    /// Build a synthetic APP1 EXIF payload with a TIFF directory
    /// containing one ExifIFD pointer and one ColorSpace SHORT tag.
    pub(super) fn build_exif_payload_color_space(color_space: u16, big_endian: bool) -> Vec<u8> {
        let pack_u16 = |v: u16| -> [u8; 2] {
            if big_endian {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let pack_u32 = |v: u32| -> [u8; 4] {
            if big_endian {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };

        // TIFF layout (offsets relative to the TIFF header start —
        // i.e. *after* the 6-byte "Exif\0\0" identifier):
        //   0..2  : "II" or "MM"
        //   2..4  : 0x002A (TIFF magic)
        //   4..8  : offset to IFD0 = 0x08 (immediately after header)
        //   8..10 : IFD0 entry count = 1
        //   10..22: IFD0 entry 0 — tag 0x8769 (ExifIFDPointer), LONG, count 1, value = offset to ExifIFD
        //   22..26: IFD0 next pointer = 0 (no IFD1)
        //   26..28: ExifIFD entry count = 1
        //   28..40: ExifIFD entry 0 — tag 0xA001 (ColorSpace), SHORT, count 1, value
        //   40..44: ExifIFD next pointer = 0
        const TIFF_HEADER_LEN: u32 = 8;
        const IFD0_ENTRY_COUNT: u32 = 1;
        const EXIF_IFD_OFFSET: u32 = TIFF_HEADER_LEN + 2 + (IFD0_ENTRY_COUNT * 12) + 4;
        let mut tiff = Vec::new();
        // Header.
        if big_endian {
            tiff.extend_from_slice(b"MM");
        } else {
            tiff.extend_from_slice(b"II");
        }
        tiff.extend_from_slice(&pack_u16(0x002A));
        tiff.extend_from_slice(&pack_u32(TIFF_HEADER_LEN));
        // IFD0.
        tiff.extend_from_slice(&pack_u16(IFD0_ENTRY_COUNT as u16));
        tiff.extend_from_slice(&pack_u16(EXIF_TAG_EXIF_IFD_POINTER));
        tiff.extend_from_slice(&pack_u16(4)); // LONG
        tiff.extend_from_slice(&pack_u32(1));
        tiff.extend_from_slice(&pack_u32(EXIF_IFD_OFFSET));
        tiff.extend_from_slice(&pack_u32(0)); // no IFD1
        // ExifIFD.
        tiff.extend_from_slice(&pack_u16(1));
        tiff.extend_from_slice(&pack_u16(EXIF_TAG_COLOR_SPACE));
        tiff.extend_from_slice(&pack_u16(3)); // SHORT
        tiff.extend_from_slice(&pack_u32(1));
        let value_slot = pack_u16(color_space);
        tiff.extend_from_slice(&value_slot);
        tiff.extend_from_slice(&[0, 0]); // unused 2 bytes of the 4-byte value slot
        tiff.extend_from_slice(&pack_u32(0)); // no next IFD

        let mut payload = Vec::new();
        payload.extend_from_slice(EXIF_IDENTIFIER);
        payload.extend_from_slice(&tiff);
        payload
    }
}

