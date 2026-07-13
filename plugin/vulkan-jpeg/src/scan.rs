// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::bit_reader::BitReader;
use crate::error::{JpegError, JpegResult};
use crate::header::{ComponentScan, FrameHeader, HuffmanTables, ScanHeader};
use crate::huffman::HuffmanTable;
use crate::marker;

/// Run the entropy-decode loop, producing one [`ComponentScan`] per
/// component declared in the scan header, in scan order.
pub(crate) fn decode_entropy(
    frame: &FrameHeader,
    scan: &ScanHeader,
    huffman: &HuffmanTables,
    restart_interval: u16,
    entropy_data: &[u8],
) -> JpegResult<Vec<ComponentScan>> {
    let mcus_h = (frame.width as usize).div_ceil(8 * frame.max_h_sampling as usize);
    let mcus_v = (frame.height as usize).div_ceil(8 * frame.max_v_sampling as usize);

    // For each scanned component, capture the matching frame component +
    // pre-built Huffman tables, and allocate the output coefficient buffer.
    let mut planes: Vec<ComponentPlane<'_>> = Vec::with_capacity(scan.components.len());
    for scan_comp in &scan.components {
        let frame_comp = frame
            .components
            .iter()
            .find(|c| c.id == scan_comp.component_id)
            .ok_or(JpegError::InvalidScan("SOS component id missing from SOF"))?;
        let dc_table = huffman.dc[scan_comp.dc_table_id as usize].as_ref().ok_or(
            JpegError::MissingHuffmanTable {
                class: 0,
                id: scan_comp.dc_table_id,
            },
        )?;
        let ac_table = huffman.ac[scan_comp.ac_table_id as usize].as_ref().ok_or(
            JpegError::MissingHuffmanTable {
                class: 1,
                id: scan_comp.ac_table_id,
            },
        )?;
        let blocks_h = mcus_h * frame_comp.h_sampling as usize;
        let blocks_v = mcus_v * frame_comp.v_sampling as usize;
        planes.push(ComponentPlane {
            component_id: frame_comp.id,
            h_sampling: frame_comp.h_sampling,
            v_sampling: frame_comp.v_sampling,
            quant_table_id: frame_comp.quant_table_id,
            blocks_h,
            blocks_v,
            coefficients: vec![0i16; blocks_h * blocks_v * 64],
            dc_predictor: 0,
            dc_table,
            ac_table,
        });
    }

    let mut reader = BitReader::new(entropy_data);
    let mut mcus_since_restart: u32 = 0;
    let mut expected_restart: u8 = 0;

    for mcu_y in 0..mcus_v {
        for mcu_x in 0..mcus_h {
            if restart_interval > 0 && mcus_since_restart == restart_interval as u32 {
                handle_restart(&mut reader, &mut planes, &mut expected_restart)?;
                mcus_since_restart = 0;
            }
            for plane in planes.iter_mut() {
                let h_sampling = plane.h_sampling as usize;
                let v_sampling = plane.v_sampling as usize;
                for vy in 0..v_sampling {
                    for vx in 0..h_sampling {
                        let block_x = mcu_x * h_sampling + vx;
                        let block_y = mcu_y * v_sampling + vy;
                        decode_block(&mut reader, plane, block_x, block_y)?;
                    }
                }
            }
            mcus_since_restart += 1;
        }
    }

    Ok(planes
        .into_iter()
        .map(|p| ComponentScan {
            component_id: p.component_id,
            h_sampling: p.h_sampling,
            v_sampling: p.v_sampling,
            quant_table_id: p.quant_table_id,
            blocks_horizontal: p.blocks_h,
            blocks_vertical: p.blocks_v,
            coefficients: p.coefficients,
        })
        .collect())
}

struct ComponentPlane<'tables> {
    component_id: u8,
    h_sampling: u8,
    v_sampling: u8,
    quant_table_id: u8,
    blocks_h: usize,
    blocks_v: usize,
    coefficients: Vec<i16>,
    dc_predictor: i32,
    dc_table: &'tables HuffmanTable,
    ac_table: &'tables HuffmanTable,
}

fn decode_block(
    reader: &mut BitReader<'_>,
    plane: &mut ComponentPlane<'_>,
    block_x: usize,
    block_y: usize,
) -> JpegResult<()> {
    let block_offset = (block_y * plane.blocks_h + block_x) * 64;
    let block = &mut plane.coefficients[block_offset..block_offset + 64];

    // DC coefficient: read category, then `category` bits, then extend.
    let dc_category = plane.dc_table.decode_symbol(reader)?;
    if dc_category > 11 {
        return Err(JpegError::InvalidScan(
            "DC category exceeds baseline JPEG limit of 11",
        ));
    }
    let dc_diff = if dc_category == 0 {
        0
    } else {
        let raw = reader.read_bits(dc_category as u32)?;
        BitReader::extend(raw, dc_category as u32)
    };
    let dc = plane.dc_predictor + dc_diff;
    plane.dc_predictor = dc;
    block[0] = clamp_i16(dc)?;

    // AC coefficients 1..=63.
    let mut k = 1usize;
    while k < 64 {
        let rs = plane.ac_table.decode_symbol(reader)?;
        let run = (rs >> 4) as usize;
        let category = (rs & 0x0F) as u32;
        if category == 0 {
            if run == 15 {
                // ZRL: 16 zeros, no value.
                k += 16;
                if k > 64 {
                    return Err(JpegError::InvalidScan("ZRL would overrun 8x8 block"));
                }
                continue;
            }
            // EOB: remaining coefficients are zero (already initialized).
            break;
        }
        if category > 10 {
            return Err(JpegError::InvalidScan(
                "AC category exceeds baseline JPEG limit of 10",
            ));
        }
        k += run;
        if k >= 64 {
            return Err(JpegError::InvalidScan("AC run+category overruns 8x8 block"));
        }
        let raw = reader.read_bits(category)?;
        let value = BitReader::extend(raw, category);
        block[k] = clamp_i16(value)?;
        k += 1;
    }
    Ok(())
}

fn clamp_i16(value: i32) -> JpegResult<i16> {
    i16::try_from(value).map_err(|_| {
        JpegError::InvalidScan("coefficient outside i16 range (corrupt entropy stream)")
    })
}

fn handle_restart(
    reader: &mut BitReader<'_>,
    planes: &mut [ComponentPlane<'_>],
    expected_restart: &mut u8,
) -> JpegResult<()> {
    reader.reset_to_byte_boundary();
    let marker_byte = reader.read_marker()?;
    let expected = marker::RST0 + *expected_restart;
    if marker_byte != expected {
        return Err(JpegError::UnexpectedRestartMarker {
            expected: *expected_restart,
            marker: marker_byte,
            offset: reader.byte_offset(),
        });
    }
    *expected_restart = (*expected_restart + 1) & 0x07;
    for plane in planes.iter_mut() {
        plane.dc_predictor = 0;
    }
    Ok(())
}
