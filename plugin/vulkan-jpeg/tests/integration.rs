// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![allow(clippy::needless_range_loop)] // natural reading for 8x8 IDCT + per-block pixel iteration

use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use vulkan_jpeg::{DecodedJpeg, JpegError, decode};

const QUALITY: u8 = 90;

/// Encode `pixels` as a JPEG with the requested color type and chroma
/// subsampling, returning the bitstream bytes.
fn encode_jpeg(
    width: u16,
    height: u16,
    pixels: &[u8],
    color_type: ColorType,
    sampling: SamplingFactor,
) -> Vec<u8> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut encoder = Encoder::new(&mut bytes, QUALITY);
    encoder.set_sampling_factor(sampling);
    encoder.encode(pixels, width, height, color_type).unwrap();
    bytes
}

fn solid_grayscale(width: u16, height: u16, luma: u8) -> Vec<u8> {
    let pixels = vec![luma; (width as usize) * (height as usize)];
    encode_jpeg(
        width,
        height,
        &pixels,
        ColorType::Luma,
        SamplingFactor::F_1_1,
    )
}

fn solid_rgb(width: u16, height: u16, r: u8, g: u8, b: u8) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((width as usize) * (height as usize) * 3);
    for _ in 0..(width as usize) * (height as usize) {
        pixels.extend_from_slice(&[r, g, b]);
    }
    encode_jpeg(
        width,
        height,
        &pixels,
        ColorType::Rgb,
        SamplingFactor::R_4_2_0,
    )
}

fn gradient_grayscale(width: u16, height: u16) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            // Horizontal+vertical gradient: avoids any single-component
            // degenerate case while keeping DC predictable.
            let v = ((x + y) as f32 / (w + h) as f32 * 255.0) as u8;
            pixels.push(v);
        }
    }
    encode_jpeg(
        width,
        height,
        &pixels,
        ColorType::Luma,
        SamplingFactor::F_1_1,
    )
}

fn complex_rgb(width: u16, height: u16) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut pixels = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let r = ((x * 7) % 256) as u8;
            let g = ((y * 11) % 256) as u8;
            let b = (((x + y) * 13) % 256) as u8;
            pixels.extend_from_slice(&[r, g, b]);
        }
    }
    encode_jpeg(
        width,
        height,
        &pixels,
        ColorType::Rgb,
        SamplingFactor::R_4_2_0,
    )
}

fn assert_well_formed(decoded: &DecodedJpeg, width: u16, height: u16, components: usize) {
    assert_eq!(decoded.frame.width, width);
    assert_eq!(decoded.frame.height, height);
    assert_eq!(decoded.frame.components.len(), components);
    assert_eq!(decoded.components.len(), components);

    for component in &decoded.components {
        let expected_len = component.blocks_horizontal * component.blocks_vertical * 64;
        assert_eq!(component.coefficients.len(), expected_len);
    }
}

#[test]
fn parses_solid_grayscale() {
    let bytes = solid_grayscale(16, 16, 128);
    let decoded = decode(&bytes).expect("decode solid grayscale");

    assert_well_formed(&decoded, 16, 16, 1);
    assert_eq!(decoded.scan.spectral_start, 0);
    assert_eq!(decoded.scan.spectral_end, 63);
    assert_eq!(decoded.scan.successive_high, 0);
    assert_eq!(decoded.scan.successive_low, 0);

    // 1 component, 1x1 sampling, 16x16 image → 2x2 = 4 blocks.
    let luma = &decoded.components[0];
    assert_eq!(luma.h_sampling, 1);
    assert_eq!(luma.v_sampling, 1);
    assert_eq!(luma.blocks_horizontal, 2);
    assert_eq!(luma.blocks_vertical, 2);

    // Solid color: every AC coefficient in every block is zero.
    for by in 0..luma.blocks_vertical {
        for bx in 0..luma.blocks_horizontal {
            let block = luma.block(bx, by);
            for &ac in &block[1..64] {
                assert_eq!(
                    ac, 0,
                    "AC coefficient at block ({bx},{by}) should be zero for solid color"
                );
            }
        }
    }

    // Solid color → every block has the same (predictor-resolved) absolute
    // DC. The library stores absolute DCs in coefficient[0], so the buffer
    // values themselves should be identical across blocks.
    let first_dc = luma.block(0, 0)[0];
    for by in 0..luma.blocks_vertical {
        for bx in 0..luma.blocks_horizontal {
            assert_eq!(
                luma.block(bx, by)[0],
                first_dc,
                "DC at block ({bx},{by}) should equal the first block's DC for solid color"
            );
        }
    }
}

#[test]
fn parses_solid_rgb_4_2_0() {
    let bytes = solid_rgb(32, 32, 200, 50, 50);
    let decoded = decode(&bytes).expect("decode solid RGB");

    assert_well_formed(&decoded, 32, 32, 3);

    // 4:2:0: Y has 2x2 sampling, Cb/Cr have 1x1.
    let y = &decoded.components[0];
    let cb = &decoded.components[1];
    let cr = &decoded.components[2];

    assert_eq!(y.h_sampling, 2);
    assert_eq!(y.v_sampling, 2);
    assert_eq!(cb.h_sampling, 1);
    assert_eq!(cb.v_sampling, 1);
    assert_eq!(cr.h_sampling, 1);
    assert_eq!(cr.v_sampling, 1);

    // 32x32 with max-sampling 2x2: MCU = 16x16 → 2x2 MCUs.
    // Y: 2x2 MCUs × 2x2 per-MCU blocks = 4x4 blocks.
    // Cb/Cr: 2x2 MCUs × 1x1 per-MCU blocks = 2x2 blocks.
    assert_eq!(y.blocks_horizontal, 4);
    assert_eq!(y.blocks_vertical, 4);
    assert_eq!(cb.blocks_horizontal, 2);
    assert_eq!(cb.blocks_vertical, 2);

    // Every AC coefficient in every component should be zero for a solid color.
    for component in &decoded.components {
        for by in 0..component.blocks_vertical {
            for bx in 0..component.blocks_horizontal {
                let block = component.block(bx, by);
                for (i, &ac) in block.iter().enumerate().skip(1) {
                    assert_eq!(
                        ac, 0,
                        "AC[{i}] at component {} block ({bx},{by}) should be zero",
                        component.component_id
                    );
                }
            }
        }
    }
}

#[test]
fn parses_gradient_grayscale_dc_increases() {
    let bytes = gradient_grayscale(32, 32);
    let decoded = decode(&bytes).expect("decode gradient");
    assert_well_formed(&decoded, 32, 32, 1);

    let luma = &decoded.components[0];
    assert_eq!(luma.blocks_horizontal, 4);
    assert_eq!(luma.blocks_vertical, 4);

    // Gradient → each block's mean pixel value differs, so absolute DCs
    // should span a range. Coefficient[0] is the predictor-resolved
    // absolute DC, so we can read directly.
    let mut abs_dcs = Vec::new();
    for by in 0..luma.blocks_vertical {
        for bx in 0..luma.blocks_horizontal {
            abs_dcs.push(luma.block(bx, by)[0]);
        }
    }
    assert!(
        abs_dcs.iter().min().unwrap() != abs_dcs.iter().max().unwrap(),
        "gradient DC range should span more than one value, got {abs_dcs:?}"
    );
}

#[test]
fn parses_complex_rgb_4_2_0() {
    // Complex pattern: just confirm the parser doesn't reject it and the
    // coefficient buffers are well-shaped. Anything more specific would
    // require a reference IDCT, which is the next issue's GPU kernel.
    let bytes = complex_rgb(48, 32);
    let decoded = decode(&bytes).expect("decode complex RGB");
    assert_well_formed(&decoded, 48, 32, 3);

    // Cross-check: zune-jpeg accepts the same bitstream. If we accept it
    // and zune doesn't (or vice versa), one of us is wrong.
    let mut zune = zune_jpeg::JpegDecoder::new(&bytes);
    let pixels = zune
        .decode()
        .expect("zune-jpeg should accept this bitstream");
    assert!(!pixels.is_empty());
}

#[test]
fn parses_dimensions_with_padding() {
    // 17x17 with 4:2:0 sampling: MCU = 16x16 → 2x2 MCUs (the bottom-right
    // MCU is padding-only). The parser shouldn't reject it; coefficient
    // buffers should be sized to cover the padded MCU grid.
    let pixels = vec![64u8; 17 * 17 * 3];
    let bytes = encode_jpeg(17, 17, &pixels, ColorType::Rgb, SamplingFactor::R_4_2_0);
    let decoded = decode(&bytes).expect("decode 17x17 with padding");
    assert_well_formed(&decoded, 17, 17, 3);

    // Y has 2x2 sampling, image is 17x17 → 2 MCUs in each direction.
    let y = &decoded.components[0];
    assert_eq!(y.blocks_horizontal, 4);
    assert_eq!(y.blocks_vertical, 4);
}

#[test]
fn empty_input_rejected() {
    let err = decode(&[]).unwrap_err();
    assert!(matches!(err, JpegError::UnexpectedEof { .. }));
}

#[test]
fn missing_soi_rejected() {
    // Bytes that aren't a JPEG.
    let bytes = [0u8, 1, 2, 3];
    let err = decode(&bytes).unwrap_err();
    assert!(matches!(err, JpegError::MissingSoi { .. }));
}

#[test]
fn truncated_after_soi_rejected() {
    let bytes = [0xFFu8, 0xD8];
    let err = decode(&bytes).unwrap_err();
    assert!(matches!(err, JpegError::UnexpectedEof { .. }));
}

#[test]
fn truncated_in_segment_rejected() {
    // SOI + DQT marker + truncated length.
    let bytes = [0xFFu8, 0xD8, 0xFF, 0xDB];
    let err = decode(&bytes).unwrap_err();
    assert!(matches!(err, JpegError::UnexpectedEof { .. }));
}

#[test]
fn progressive_sof_rejected() {
    // Encode a valid baseline JPEG, then rewrite SOF0 (0xFF 0xC0) to SOF2.
    let mut bytes = solid_grayscale(16, 16, 100);
    let pos = bytes
        .windows(2)
        .position(|w| w == [0xFF, 0xC0])
        .expect("solid_grayscale should contain SOF0");
    bytes[pos + 1] = 0xC2;
    let err = decode(&bytes).unwrap_err();
    assert!(matches!(
        err,
        JpegError::UnsupportedSof { marker: 0xC2, .. }
    ));
}

#[test]
fn invalid_huffman_table_rejected() {
    // Encode a valid JPEG, then corrupt the BITS region of the first DHT
    // segment so the code space overflows.
    let mut bytes = solid_grayscale(16, 16, 100);
    let dht_pos = bytes
        .windows(2)
        .position(|w| w == [0xFF, 0xC4])
        .expect("expected DHT in encoded JPEG");
    // dht_pos points at FF; payload starts at +4 (FF C4 LH LL Tc/Th BITS[16] ...)
    // First BITS entry is at dht_pos+5. Force more codes than possible.
    bytes[dht_pos + 5] = 5; // 5 codes of length 1 — only 2 are possible.
    let err = decode(&bytes).unwrap_err();
    assert!(matches!(err, JpegError::MalformedHuffmanTable(_)));
}

#[test]
fn missing_huffman_table_rejected() {
    // Construct a tiny minimal stream: SOI + SOF0 (8-bit, 8x8, 1 component
    // with quant table id 0) + DQT (id 0, all 16s) + SOS (component 0,
    // dc=0, ac=0, baseline) + a single 0x00 byte for entropy + EOI.
    // No DHT — should fail with MissingHuffmanTable.
    #[rustfmt::skip]
    let bytes = vec![
        0xFF, 0xD8, // SOI
        0xFF, 0xDB, 0x00, 0x43, // DQT, length 67
        0x00, // Pq=0, Tq=0
        16, 16, 16, 16, 16, 16, 16, 16,
        16, 16, 16, 16, 16, 16, 16, 16,
        16, 16, 16, 16, 16, 16, 16, 16,
        16, 16, 16, 16, 16, 16, 16, 16,
        16, 16, 16, 16, 16, 16, 16, 16,
        16, 16, 16, 16, 16, 16, 16, 16,
        16, 16, 16, 16, 16, 16, 16, 16,
        16, 16, 16, 16, 16, 16, 16, 16,
        0xFF, 0xC0, 0x00, 0x0B, // SOF0, length 11
        8, // precision
        0x00, 0x08, // height = 8
        0x00, 0x08, // width = 8
        0x01, // 1 component
        0x01, 0x11, 0x00, // component 1, sampling 1x1, quant id 0
        0xFF, 0xDA, 0x00, 0x08, // SOS, length 8
        0x01, // 1 component
        0x01, 0x00, // component 1, dc=0, ac=0
        0x00, 0x3F, 0x00, // Ss=0, Se=63, Ah=0, Al=0
        0x00, // entropy stub
        0xFF, 0xD9, // EOI
    ];
    let err = decode(&bytes).unwrap_err();
    assert!(
        matches!(err, JpegError::MissingHuffmanTable { .. }),
        "expected MissingHuffmanTable, got {err:?}"
    );
}

#[test]
fn missing_sos_rejected() {
    // SOI followed directly by EOI — no scan.
    let bytes = [0xFFu8, 0xD8, 0xFF, 0xD9];
    let err = decode(&bytes).unwrap_err();
    assert!(matches!(err, JpegError::MissingSos));
}

#[test]
fn round_trip_via_zune_jpeg_sanity() {
    // For every fixture we encode, zune-jpeg should also decode it without
    // errors. This guards against accidentally generating bitstreams that
    // are only well-formed in our parser's reading.
    let bytes = complex_rgb(64, 64);
    let mut zune = zune_jpeg::JpegDecoder::new(&bytes);
    zune.decode().expect("zune-jpeg sanity check");
    // And our parser accepts it too.
    decode(&bytes).expect("our parser accepts the same bitstream");
}

#[test]
fn parses_with_restart_intervals() {
    // Force jpeg-encoder to emit DRI + RST markers so the entropy
    // decoder's restart resync path runs end-to-end. 32x32 4:2:0 → 4 MCUs;
    // restart_interval=2 yields two restart groups → RST0 fires after MCU 1.
    let width: u16 = 32;
    let height: u16 = 32;
    let pixels: Vec<u8> = (0..(width as usize) * (height as usize) * 3)
        .map(|i| (i & 0xFF) as u8)
        .collect();
    let mut bytes: Vec<u8> = Vec::new();
    let mut encoder = Encoder::new(&mut bytes, QUALITY);
    encoder.set_sampling_factor(SamplingFactor::R_4_2_0);
    encoder.set_restart_interval(2);
    encoder
        .encode(&pixels, width, height, ColorType::Rgb)
        .unwrap();

    // Confirm the encoder actually emitted DRI; otherwise the test is vacuous.
    assert!(
        bytes.windows(2).any(|w| w == [0xFF, 0xDD]),
        "expected DRI marker in the encoded bitstream"
    );

    let decoded = decode(&bytes).expect("decode JPEG with restart markers");
    assert_well_formed(&decoded, width, height, 3);
    assert_eq!(decoded.restart_interval, 2);

    // Cross-validate against zune-jpeg to make sure restart handling
    // didn't desynchronize the entropy stream.
    let mut zune = zune_jpeg::JpegDecoder::new(&bytes);
    zune.decode()
        .expect("zune-jpeg accepts restart-bearing JPEG");
}

#[test]
fn parses_rgb_4_2_2() {
    let pixels = vec![100u8; 32 * 16 * 3];
    let bytes = encode_jpeg(32, 16, &pixels, ColorType::Rgb, SamplingFactor::R_4_2_2);
    let decoded = decode(&bytes).expect("decode RGB 4:2:2");
    assert_well_formed(&decoded, 32, 16, 3);
    let y = &decoded.components[0];
    let cb = &decoded.components[1];
    // 4:2:2 → Y has 2x1 sampling, chroma 1x1.
    assert_eq!(y.h_sampling, 2);
    assert_eq!(y.v_sampling, 1);
    assert_eq!(cb.h_sampling, 1);
    assert_eq!(cb.v_sampling, 1);
}

#[test]
fn parses_rgb_4_4_4() {
    let pixels = vec![100u8; 16 * 16 * 3];
    let bytes = encode_jpeg(16, 16, &pixels, ColorType::Rgb, SamplingFactor::R_4_4_4);
    let decoded = decode(&bytes).expect("decode RGB 4:4:4");
    assert_well_formed(&decoded, 16, 16, 3);
    // All three components have 1x1 sampling.
    for component in &decoded.components {
        assert_eq!(component.h_sampling, 1);
        assert_eq!(component.v_sampling, 1);
    }
}

/// Minimal CPU dequant + IDCT + level-shift on a single 8x8 block of
/// zig-zag-ordered coefficients. Lives in the test crate so the library
/// itself stays pure parser + Huffman. Used only to cross-check our
/// Huffman output against `zune-jpeg`'s decoded pixels.
fn idct_block_to_pixels(zigzag_coeffs: &[i16; 64], quant: &[u16; 64]) -> [[u8; 8]; 8] {
    use std::f64::consts::PI;

    // 1. Dequantize while undoing zig-zag back to natural row-major order.
    let mut natural = [0f64; 64];
    for (zz_idx, &nat_idx) in vulkan_jpeg::ZIGZAG.iter().enumerate() {
        natural[nat_idx] = (zigzag_coeffs[zz_idx] as i32 * quant[zz_idx] as i32) as f64;
    }

    // 2. Reference 2D IDCT (ITU-T T.81 A.3.3).
    let mut out = [[0u8; 8]; 8];
    for y in 0..8 {
        for x in 0..8 {
            let mut sum = 0f64;
            for v in 0..8 {
                for u in 0..8 {
                    let cu = if u == 0 { 1.0 / 2f64.sqrt() } else { 1.0 };
                    let cv = if v == 0 { 1.0 / 2f64.sqrt() } else { 1.0 };
                    let coef = natural[v * 8 + u];
                    sum += cu
                        * cv
                        * coef
                        * ((2.0 * x as f64 + 1.0) * u as f64 * PI / 16.0).cos()
                        * ((2.0 * y as f64 + 1.0) * v as f64 * PI / 16.0).cos();
                }
            }
            sum /= 4.0;
            sum += 128.0; // level shift
            out[y][x] = sum.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

#[test]
fn huffman_output_idct_matches_zune_jpeg_grayscale() {
    // Numeric cross-check: our Huffman + a textbook CPU IDCT should
    // produce pixel values within rounding of zune-jpeg's decoded pixels
    // for a grayscale fixture with non-trivial AC content.
    let bytes = gradient_grayscale(16, 16);
    let decoded = decode(&bytes).expect("our parser");
    let mut zune = zune_jpeg::JpegDecoder::new(&bytes);
    let zune_pixels = zune.decode().expect("zune-jpeg");
    let info = zune.info().expect("zune info");
    assert_eq!(info.width, 16);
    assert_eq!(info.height, 16);
    let stride = info.width as usize;

    let luma = &decoded.components[0];
    assert_eq!(luma.blocks_horizontal, 2);
    assert_eq!(luma.blocks_vertical, 2);

    let quant_id = luma.quant_table_id;
    let quant = decoded
        .quantization_table(quant_id)
        .expect("luma quant table");

    // Coefficient[0] is the predictor-resolved absolute DC; the test just
    // needs to copy the block as-is, dequantize via the JPEG-stored quant
    // table, and run the reference IDCT.
    for by in 0..luma.blocks_vertical {
        for bx in 0..luma.blocks_horizontal {
            let mut block_coeffs = [0i16; 64];
            block_coeffs.copy_from_slice(luma.block(bx, by));
            let pixels = idct_block_to_pixels(&block_coeffs, &quant.values);
            for py in 0..8 {
                for px in 0..8 {
                    let global_x = bx * 8 + px;
                    let global_y = by * 8 + py;
                    let zune_pixel = zune_pixels[global_y * stride + global_x];
                    let our_pixel = pixels[py][px];
                    let diff = (zune_pixel as i32 - our_pixel as i32).abs();
                    assert!(
                        diff <= 1,
                        "pixel mismatch at ({global_x},{global_y}): ours {our_pixel}, zune {zune_pixel}"
                    );
                }
            }
        }
    }
}
