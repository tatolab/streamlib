// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The surface-share verbs that hand an interpreter a GPU pool's claim table
//! and its slots, once, so each later frame of the pool is claimed in shared
//! memory with no request.

/// The op that answers a pool's claim table memfd, the caller's claimant
/// column, and the pool slot key leased to each row.
pub const SURFACE_SHARE_CLAIM_TABLE_OF_GPU_POOL_OP: &str = "claim_table_of_gpu_pool";

/// The op that answers one pool slot's memory, under its pool slot key, with
/// no checkout lease.
pub const SURFACE_SHARE_IMPORT_POOL_SLOT_OP: &str = "import_pool_slot";

/// The request and reply field naming a pool by its claim table's UUID.
pub const SURFACE_SHARE_GPU_POOL_FIELD: &str = "pool";

/// The reply field stating the column the caller's process claims in.
pub const SURFACE_SHARE_CLAIMANT_COLUMN_FIELD: &str = "claimant_column";

/// The reply field listing each leased row and its pool slot key.
pub const SURFACE_SHARE_POOL_SLOTS_FIELD: &str = "pool_slots";

/// The field of one `pool_slots` entry naming its claim table row.
pub const SURFACE_SHARE_POOL_SLOT_ROW_FIELD: &str = "slot";

/// A pool's UUID as the wire spells it: 32 lowercase hex digits.
pub fn gpu_pool_uuid_wire_name(pool_uuid: &[u8; 16]) -> String {
    pool_uuid.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The UUID a wire name spells, or `None` for anything but 32 hex digits.
pub fn gpu_pool_uuid_of_its_wire_name(wire_name: &str) -> Option<[u8; 16]> {
    if wire_name.len() != 32 || !wire_name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut pool_uuid = [0u8; 16];
    for (byte, hex_pair) in pool_uuid.iter_mut().zip(wire_name.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(hex_pair).ok()?, 16).ok()?;
    }
    Some(pool_uuid)
}

/// A claim table hand-over, read: the column to claim in and each row's pool
/// slot key.
#[derive(Debug, PartialEq, Eq)]
pub struct GpuPoolClaimTableHandOver {
    pub claimant_column_index: usize,
    pub pool_slot_key_by_row: Vec<(u32, String)>,
}

impl GpuPoolClaimTableHandOver {
    /// Read a `claim_table_of_gpu_pool` reply.
    pub fn from_reply(reply: &serde_json::Value) -> Result<Self, String> {
        let claimant_column_index = reply
            .get(SURFACE_SHARE_CLAIMANT_COLUMN_FIELD)
            .and_then(|value| value.as_u64())
            .ok_or("a claim table hand-over states no claimant column")?
            as usize;
        let pool_slot_key_by_row = reply
            .get(SURFACE_SHARE_POOL_SLOTS_FIELD)
            .and_then(|value| value.as_array())
            .ok_or("a claim table hand-over lists no pool slots")?
            .iter()
            .map(|pool_slot| {
                let row = pool_slot
                    .get(SURFACE_SHARE_POOL_SLOT_ROW_FIELD)
                    .and_then(|value| value.as_u64())
                    .and_then(|row| u32::try_from(row).ok());
                let pool_slot_key = pool_slot.get("surface_id").and_then(|value| value.as_str());
                match (row, pool_slot_key) {
                    (Some(row), Some(pool_slot_key)) => Ok((row, pool_slot_key.to_string())),
                    _ => Err(format!(
                        "a claim table hand-over lists a pool slot without a row and a key: \
                         {pool_slot}"
                    )),
                }
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            claimant_column_index,
            pool_slot_key_by_row,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pool_uuid_round_trips_its_wire_name_and_anything_else_is_refused() {
        let pool_uuid = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let wire_name = gpu_pool_uuid_wire_name(&pool_uuid);

        assert_eq!(wire_name, "00112233445566778899aabbccddeeff");
        assert_eq!(gpu_pool_uuid_of_its_wire_name(&wire_name), Some(pool_uuid));
        assert_eq!(gpu_pool_uuid_of_its_wire_name("0011"), None);
        assert_eq!(
            gpu_pool_uuid_of_its_wire_name("zz112233445566778899aabbccddeeff"),
            None
        );
    }

    #[test]
    fn a_hand_over_reads_its_column_and_every_rows_key() {
        let reply = serde_json::json!({
            SURFACE_SHARE_CLAIMANT_COLUMN_FIELD: 3,
            SURFACE_SHARE_POOL_SLOTS_FIELD: [
                {SURFACE_SHARE_POOL_SLOT_ROW_FIELD: 0, "surface_id": "slot-a"},
                {SURFACE_SHARE_POOL_SLOT_ROW_FIELD: 2, "surface_id": "slot-c"},
            ],
        });

        assert_eq!(
            GpuPoolClaimTableHandOver::from_reply(&reply),
            Ok(GpuPoolClaimTableHandOver {
                claimant_column_index: 3,
                pool_slot_key_by_row: vec![(0, "slot-a".into()), (2, "slot-c".into())],
            })
        );
    }
}
