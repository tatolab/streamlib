// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The Bundle frame: one message carrying a bundle, or a List, that holds a
//! Texture, Audio or an encoded unit. Its payload opens with a row table, one
//! row for each heavy member or element: what the row holds, the full
//! descriptor that member would have on a wire of its own, and the offset and
//! length of its bytes in the payload. One more row, of kind Records message,
//! holds every small member. The descriptor body states how many rows the
//! table has; each row packs little-endian at its documented offsets, and its
//! reserved tail is zero.

use crate::{
    DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind,
};

/// Bytes of a descriptor body a Bundle frame uses: its row count.
pub const BUNDLE_FRAME_BODY_USED_BYTES: usize = 4;

/// Byte offset of what a row holds in a row: 0 the small members, 1 a member
/// by name, 2 a List element by index.
pub const BUNDLE_FRAME_ROW_HOLDS_OFFSET: usize = 0;
/// Byte offset of a List element's index in a row; zero on any other row.
pub const BUNDLE_FRAME_ROW_ELEMENT_INDEX_OFFSET: usize = 4;
/// Byte offset of a member's name in a row, UTF-8 and zero-padded; zero on
/// any other row.
pub const BUNDLE_FRAME_ROW_MEMBER_NAME_OFFSET: usize = 8;
/// Bytes a row gives a member's name.
pub const BUNDLE_FRAME_ROW_MEMBER_NAME_BYTES: usize = 32;
/// Byte offset of the kind of the descriptor the row's bytes travel under.
pub const BUNDLE_FRAME_ROW_DESCRIPTOR_KIND_OFFSET: usize = 40;
/// Byte offset of the body of the descriptor the row's bytes travel under,
/// its [`DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES`] bytes.
pub const BUNDLE_FRAME_ROW_DESCRIPTOR_BODY_OFFSET: usize = 44;
/// Byte offset of the row's bytes from the payload's start.
pub const BUNDLE_FRAME_ROW_PAYLOAD_OFFSET_OFFSET: usize = 104;
/// Byte offset of the length of the row's bytes.
pub const BUNDLE_FRAME_ROW_PAYLOAD_BYTE_LEN_OFFSET: usize = 112;
/// Bytes one row takes; the eight past its length are reserved zero.
pub const BUNDLE_FRAME_ROW_BYTES: usize = 128;
/// The alignment of each row's bytes within the payload.
pub const BUNDLE_FRAME_ROW_PAYLOAD_ALIGNMENT_BYTES: usize = 64;

/// The body of a Bundle frame descriptor: how many rows the table at the
/// start of the payload holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BundleFrameDescriptorBody {
    /// Rows in the table.
    pub row_count: u32,
}

impl BundleFrameDescriptorBody {
    /// This body as descriptor body bytes, the reserved tail zero.
    pub fn to_descriptor_body(&self) -> [u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES] {
        let mut body = [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES];
        body[..BUNDLE_FRAME_BODY_USED_BYTES].copy_from_slice(&self.row_count.to_le_bytes());
        body
    }

    /// The body `descriptor_body` packs.
    pub fn from_descriptor_body(
        descriptor_body: &[u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
    ) -> Self {
        Self {
            row_count: u32_le_at(descriptor_body, 0),
        }
    }
}

/// What one row of a Bundle frame holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BundleFrameRowHolds {
    /// Every small member, as one Records message.
    SmallMembers,
    /// The heavy member of this name.
    Member(String),
    /// The List element at this index.
    Element(u32),
}

impl BundleFrameRowHolds {
    /// Its words in a refusal.
    fn words(&self) -> String {
        match self {
            Self::SmallMembers => "the small members' row".to_owned(),
            Self::Member(name) => format!("member `{name}`"),
            Self::Element(index) => format!("element {index}"),
        }
    }
}

/// One row of a Bundle frame's table: what it holds, the descriptor its bytes
/// travel under, and where those bytes are in the payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleFrameRow {
    /// What the row holds.
    pub holds: BundleFrameRowHolds,
    /// The descriptor the row's bytes would travel under on a wire of their own.
    pub descriptor: DataChannelPayloadDescriptor,
    /// Offset of the row's bytes from the payload's start.
    pub payload_byte_offset: u64,
    /// Length of the row's bytes.
    pub payload_byte_len: u64,
}

/// Why a Bundle frame cannot be packed or read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleFrameRefusal {
    /// What is wrong, naming the row.
    pub reason: String,
}

impl std::fmt::Display for BundleFrameRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for BundleFrameRefusal {}

fn refusal(reason: String) -> BundleFrameRefusal {
    BundleFrameRefusal { reason }
}

fn u32_le_at(bytes: &[u8], offset: usize) -> u32 {
    let mut field = [0u8; 4];
    field.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(field)
}

fn u64_le_at(bytes: &[u8], offset: usize) -> u64 {
    let mut field = [0u8; 8];
    field.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(field)
}

/// Why `descriptor` cannot travel as the row `holds`: the small members ride a
/// Records message and nothing else does, a Bundle frame holds no Bundle
/// frame, and a kind this build does not know is refused by kind.
fn refusal_of_the_rows_kind(
    holds: &BundleFrameRowHolds,
    descriptor: &DataChannelPayloadDescriptor,
) -> Option<BundleFrameRefusal> {
    let kind = match descriptor.known_kind() {
        Ok(kind) => kind,
        Err(why) => {
            return Some(refusal(format!(
                "{} of a Bundle frame: {why}",
                holds.words()
            )));
        }
    };
    let is_records = kind == DataChannelPayloadDescriptorKind::RecordsMessage;
    match holds {
        BundleFrameRowHolds::SmallMembers if !is_records => Some(refusal(format!(
            "the small members of a Bundle frame ride a Records message, and its row states \
             {kind:?}"
        ))),
        BundleFrameRowHolds::Member(_) | BundleFrameRowHolds::Element(_) if is_records => {
            Some(refusal(format!(
                "{} of a Bundle frame states a Records message, which only the small members' \
                 row rides",
                holds.words()
            )))
        }
        _ if kind == DataChannelPayloadDescriptorKind::BundleFrame => Some(refusal(format!(
            "{} of a Bundle frame states a Bundle frame, and a bundle holds no bundle",
            holds.words()
        ))),
        _ => None,
    }
}

impl BundleFrameRow {
    /// This row as its [`BUNDLE_FRAME_ROW_BYTES`] bytes, refused when a
    /// member's name does not fit or the row's kind is not one it may travel as.
    pub fn to_row_bytes(&self) -> Result<[u8; BUNDLE_FRAME_ROW_BYTES], BundleFrameRefusal> {
        if let Some(why) = refusal_of_the_rows_kind(&self.holds, &self.descriptor) {
            return Err(why);
        }
        let mut row = [0u8; BUNDLE_FRAME_ROW_BYTES];
        let holds_code: u32 = match &self.holds {
            BundleFrameRowHolds::SmallMembers => 0,
            BundleFrameRowHolds::Member(name) => {
                let name_bytes = name.as_bytes();
                if name_bytes.is_empty()
                    || name_bytes.len() > BUNDLE_FRAME_ROW_MEMBER_NAME_BYTES
                    || name_bytes.contains(&0)
                {
                    return Err(refusal(format!(
                        "a Bundle frame's member name {name:?} is not 1 to \
                         {BUNDLE_FRAME_ROW_MEMBER_NAME_BYTES} bytes of UTF-8 without a NUL"
                    )));
                }
                row[BUNDLE_FRAME_ROW_MEMBER_NAME_OFFSET
                    ..BUNDLE_FRAME_ROW_MEMBER_NAME_OFFSET + name_bytes.len()]
                    .copy_from_slice(name_bytes);
                1
            }
            BundleFrameRowHolds::Element(index) => {
                row[BUNDLE_FRAME_ROW_ELEMENT_INDEX_OFFSET..BUNDLE_FRAME_ROW_MEMBER_NAME_OFFSET]
                    .copy_from_slice(&index.to_le_bytes());
                2
            }
        };
        row[BUNDLE_FRAME_ROW_HOLDS_OFFSET..BUNDLE_FRAME_ROW_ELEMENT_INDEX_OFFSET]
            .copy_from_slice(&holds_code.to_le_bytes());
        row[BUNDLE_FRAME_ROW_DESCRIPTOR_KIND_OFFSET..BUNDLE_FRAME_ROW_DESCRIPTOR_BODY_OFFSET]
            .copy_from_slice(&self.descriptor.kind.to_le_bytes());
        row[BUNDLE_FRAME_ROW_DESCRIPTOR_BODY_OFFSET..BUNDLE_FRAME_ROW_PAYLOAD_OFFSET_OFFSET]
            .copy_from_slice(&self.descriptor.body);
        row[BUNDLE_FRAME_ROW_PAYLOAD_OFFSET_OFFSET..BUNDLE_FRAME_ROW_PAYLOAD_BYTE_LEN_OFFSET]
            .copy_from_slice(&self.payload_byte_offset.to_le_bytes());
        row[BUNDLE_FRAME_ROW_PAYLOAD_BYTE_LEN_OFFSET..BUNDLE_FRAME_ROW_PAYLOAD_BYTE_LEN_OFFSET + 8]
            .copy_from_slice(&self.payload_byte_len.to_le_bytes());
        Ok(row)
    }

    /// The row `row_bytes` packs, refused when what it holds is out of range
    /// or its kind is not one it may travel as.
    pub fn from_row_bytes(
        row_bytes: &[u8; BUNDLE_FRAME_ROW_BYTES],
    ) -> Result<Self, BundleFrameRefusal> {
        let holds = match u32_le_at(row_bytes, BUNDLE_FRAME_ROW_HOLDS_OFFSET) {
            0 => BundleFrameRowHolds::SmallMembers,
            1 => {
                let name_field = &row_bytes[BUNDLE_FRAME_ROW_MEMBER_NAME_OFFSET
                    ..BUNDLE_FRAME_ROW_MEMBER_NAME_OFFSET + BUNDLE_FRAME_ROW_MEMBER_NAME_BYTES];
                let name_len = name_field
                    .iter()
                    .position(|&byte| byte == 0)
                    .unwrap_or(name_field.len());
                let name = std::str::from_utf8(&name_field[..name_len])
                    .ok()
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| {
                        refusal("a Bundle frame's member row names no member in UTF-8".to_owned())
                    })?;
                BundleFrameRowHolds::Member(name.to_owned())
            }
            2 => BundleFrameRowHolds::Element(u32_le_at(
                row_bytes,
                BUNDLE_FRAME_ROW_ELEMENT_INDEX_OFFSET,
            )),
            unknown => {
                return Err(refusal(format!(
                    "a Bundle frame's row states it holds {unknown}, which is none of the small \
                     members (0), a member (1) or an element (2)"
                )));
            }
        };
        let mut descriptor = DataChannelPayloadDescriptor {
            kind: u32_le_at(row_bytes, BUNDLE_FRAME_ROW_DESCRIPTOR_KIND_OFFSET),
            body: [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
        };
        descriptor.body.copy_from_slice(
            &row_bytes
                [BUNDLE_FRAME_ROW_DESCRIPTOR_BODY_OFFSET..BUNDLE_FRAME_ROW_PAYLOAD_OFFSET_OFFSET],
        );
        if let Some(why) = refusal_of_the_rows_kind(&holds, &descriptor) {
            return Err(why);
        }
        Ok(Self {
            holds,
            descriptor,
            payload_byte_offset: u64_le_at(row_bytes, BUNDLE_FRAME_ROW_PAYLOAD_OFFSET_OFFSET),
            payload_byte_len: u64_le_at(row_bytes, BUNDLE_FRAME_ROW_PAYLOAD_BYTE_LEN_OFFSET),
        })
    }
}

/// Why `rows` cannot be one Bundle frame's table: two rows hold the small
/// members, the same member or the same element, or member rows and element
/// rows are mixed, as a frame carries a bundle or a List and not both.
fn refusal_of_the_tables_rows(rows: &[BundleFrameRow]) -> Option<BundleFrameRefusal> {
    let holds_a_member = rows
        .iter()
        .any(|row| matches!(row.holds, BundleFrameRowHolds::Member(_)));
    let holds_an_element = rows
        .iter()
        .any(|row| matches!(row.holds, BundleFrameRowHolds::Element(_)));
    if holds_a_member && holds_an_element {
        return Some(refusal(
            "a Bundle frame carries a bundle's members or a List's elements, and its table holds \
             both"
                .to_owned(),
        ));
    }
    rows.iter().enumerate().find_map(|(place, row)| {
        rows[..place]
            .iter()
            .any(|earlier| earlier.holds == row.holds)
            .then(|| {
                refusal(format!(
                    "a Bundle frame's table holds {} more than once",
                    row.holds.words()
                ))
            })
    })
}

/// The row table a Bundle payload starts with, `row_count` rows read in
/// order; refused when the payload is shorter than the table, a row's bytes
/// run past the payload's end or into the table, or the rows are not one
/// bundle's or one List's.
pub fn bundle_frame_row_table_of(
    body: BundleFrameDescriptorBody,
    payload: &[u8],
) -> Result<Vec<BundleFrameRow>, BundleFrameRefusal> {
    let table_byte_len = (body.row_count as usize)
        .checked_mul(BUNDLE_FRAME_ROW_BYTES)
        .filter(|table_byte_len| *table_byte_len <= payload.len())
        .ok_or_else(|| {
            refusal(format!(
                "a Bundle frame states {} rows, whose table its {}-byte payload does not hold",
                body.row_count,
                payload.len()
            ))
        })?;
    let rows = payload[..table_byte_len]
        .as_chunks::<BUNDLE_FRAME_ROW_BYTES>()
        .0
        .iter()
        .map(|row_bytes| {
            let row = BundleFrameRow::from_row_bytes(row_bytes)?;
            if row
                .payload_byte_offset
                .checked_add(row.payload_byte_len)
                .is_none_or(|end| end > payload.len() as u64)
            {
                return Err(refusal(format!(
                    "{} of a Bundle frame states {} bytes at offset {}, past the end of its \
                     {}-byte payload",
                    row.holds.words(),
                    row.payload_byte_len,
                    row.payload_byte_offset,
                    payload.len()
                )));
            }
            if row.payload_byte_len != 0 && row.payload_byte_offset < table_byte_len as u64 {
                return Err(refusal(format!(
                    "{} of a Bundle frame states bytes at offset {}, inside the {table_byte_len}-\
                     byte row table",
                    row.holds.words(),
                    row.payload_byte_offset
                )));
            }
            Ok(row)
        })
        .collect::<Result<Vec<_>, _>>()?;
    match refusal_of_the_tables_rows(&rows) {
        Some(why) => Err(why),
        None => Ok(rows),
    }
}

/// One row a Bundle payload is packed from: what it holds, the descriptor its
/// bytes travel under, and those bytes.
#[derive(Clone, Copy, Debug)]
pub struct BundleFrameRowToPack<'data> {
    /// What the row holds.
    pub holds: &'data BundleFrameRowHolds,
    /// The descriptor the bytes would travel under on a wire of their own.
    pub descriptor: DataChannelPayloadDescriptor,
    /// The bytes; empty for a member, such as a Texture, whose descriptor names
    /// where it lives.
    pub payload: &'data [u8],
}

/// A Bundle frame's descriptor and payload holding `rows`, each row in order
/// and each row's bytes at its aligned offset; refused when a row does not
/// pack or the rows are not one bundle's or one List's.
pub fn bundle_frame_of_rows(
    rows: &[BundleFrameRowToPack<'_>],
) -> Result<(DataChannelPayloadDescriptor, Vec<u8>), BundleFrameRefusal> {
    let alignment = BUNDLE_FRAME_ROW_PAYLOAD_ALIGNMENT_BYTES as u64;
    let mut next_offset = (rows.len() * BUNDLE_FRAME_ROW_BYTES) as u64;
    let table: Vec<BundleFrameRow> = rows
        .iter()
        .map(|row| {
            let payload_byte_offset = next_offset.div_ceil(alignment) * alignment;
            next_offset = payload_byte_offset + row.payload.len() as u64;
            BundleFrameRow {
                holds: row.holds.clone(),
                descriptor: row.descriptor,
                payload_byte_offset,
                payload_byte_len: row.payload.len() as u64,
            }
        })
        .collect();
    if let Some(why) = refusal_of_the_tables_rows(&table) {
        return Err(why);
    }
    let mut payload = vec![0u8; next_offset as usize];
    for (row_index, (row, to_pack)) in table.iter().zip(rows).enumerate() {
        let row_start = row_index * BUNDLE_FRAME_ROW_BYTES;
        payload[row_start..row_start + BUNDLE_FRAME_ROW_BYTES]
            .copy_from_slice(&row.to_row_bytes()?);
        let data_start = row.payload_byte_offset as usize;
        payload[data_start..data_start + to_pack.payload.len()].copy_from_slice(to_pack.payload);
    }
    let body = BundleFrameDescriptorBody {
        row_count: rows.len() as u32,
    };
    Ok((DataChannelPayloadDescriptor::bundle_frame(body), payload))
}

#[cfg(test)]
mod tests;
