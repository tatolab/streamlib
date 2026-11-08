// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The surface-share verbs that hand an interpreter a GPU pool's claim table
//! and its slots, once, so each later frame of the pool is claimed in shared
//! memory with no request.

/// The op that answers a pool's claim table (a memfd on Linux, a Mach memory
/// entry on macOS), the caller's claimant column, and the pool slot key
/// leased to each row.
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

/// The field of one `pool_slots` entry stating the row's lease number, which
/// its row in the table carries for as long as the slot holds it.
pub const SURFACE_SHARE_POOL_SLOT_ROW_LEASE_NUMBER_FIELD: &str = "row_lease_number";

/// The `check_out` request field a helper sets when it already holds its own
/// import of the published frame's pool slot; the service then answers the
/// frame's metadata and sends no handle.
pub const SURFACE_SHARE_HOLDS_POOL_SLOT_IMPORT_FIELD: &str = "holds_pool_slot_import";

/// The reply field listing the pool slot keys the engine unregistered since
/// this connection was last answered, among those it handed memory for.
pub const SURFACE_SHARE_RETIRED_POOL_SLOTS_FIELD: &str = "retired_pool_slots";

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

/// One claim table row leased to a slot, as a hand-over lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuPoolClaimTableRowLeasedToASlot {
    /// The row, the `slot` a `GpuPoolSlotFrame` descriptor carries.
    pub row_index: u32,
    /// The key of the slot holding the row.
    pub pool_slot_key: String,
    /// The lease's number, which the row carries while the slot holds it.
    pub row_lease_number: u64,
}

impl GpuPoolClaimTableRowLeasedToASlot {
    /// This row as one entry of a hand-over's `pool_slots` field.
    pub fn pool_slots_entry(&self) -> serde_json::Value {
        serde_json::json!({
            SURFACE_SHARE_POOL_SLOT_ROW_FIELD: self.row_index,
            "surface_id": self.pool_slot_key,
            SURFACE_SHARE_POOL_SLOT_ROW_LEASE_NUMBER_FIELD: self.row_lease_number,
        })
    }

    fn of_a_pool_slots_entry(pool_slot: &serde_json::Value) -> Result<Self, String> {
        let row_index = pool_slot
            .get(SURFACE_SHARE_POOL_SLOT_ROW_FIELD)
            .and_then(|value| value.as_u64())
            .and_then(|row| u32::try_from(row).ok());
        let pool_slot_key = pool_slot.get("surface_id").and_then(|value| value.as_str());
        let row_lease_number = pool_slot
            .get(SURFACE_SHARE_POOL_SLOT_ROW_LEASE_NUMBER_FIELD)
            .and_then(|value| value.as_u64());
        match (row_index, pool_slot_key, row_lease_number) {
            (Some(row_index), Some(pool_slot_key), Some(row_lease_number)) => Ok(Self {
                row_index,
                pool_slot_key: pool_slot_key.to_string(),
                row_lease_number,
            }),
            _ => Err(format!(
                "a claim table hand-over lists a pool slot without a row, a key and a lease \
                 number: {pool_slot}"
            )),
        }
    }
}

/// A claim table hand-over, read: the column to claim in and each leased row.
#[derive(Debug, PartialEq, Eq)]
pub struct GpuPoolClaimTableHandOver {
    pub claimant_column_index: usize,
    pub rows_leased_to_slots: Vec<GpuPoolClaimTableRowLeasedToASlot>,
}

impl GpuPoolClaimTableHandOver {
    /// Read a `claim_table_of_gpu_pool` reply.
    pub fn from_reply(reply: &serde_json::Value) -> Result<Self, String> {
        let claimant_column_index = reply
            .get(SURFACE_SHARE_CLAIMANT_COLUMN_FIELD)
            .and_then(|value| value.as_u64())
            .ok_or("a claim table hand-over states no claimant column")?
            as usize;
        let rows_leased_to_slots = reply
            .get(SURFACE_SHARE_POOL_SLOTS_FIELD)
            .and_then(|value| value.as_array())
            .ok_or("a claim table hand-over lists no pool slots")?
            .iter()
            .map(GpuPoolClaimTableRowLeasedToASlot::of_a_pool_slots_entry)
            .collect::<Result<_, _>>()?;
        Ok(Self {
            claimant_column_index,
            rows_leased_to_slots,
        })
    }
}

/// The refusal the service answered a request with, if it answered one.
pub fn refusal_the_surface_share_service_answered(response: &serde_json::Value) -> Option<&str> {
    response.get("error").and_then(|value| value.as_str())
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
    fn a_hand_over_reads_back_its_column_and_every_row_it_lists() {
        let rows_leased_to_slots = vec![
            GpuPoolClaimTableRowLeasedToASlot {
                row_index: 0,
                pool_slot_key: "slot-a".into(),
                row_lease_number: 1,
            },
            GpuPoolClaimTableRowLeasedToASlot {
                row_index: 2,
                pool_slot_key: "slot-c".into(),
                row_lease_number: 4,
            },
        ];
        let reply = serde_json::json!({
            SURFACE_SHARE_CLAIMANT_COLUMN_FIELD: 3,
            SURFACE_SHARE_POOL_SLOTS_FIELD: rows_leased_to_slots
                .iter()
                .map(GpuPoolClaimTableRowLeasedToASlot::pool_slots_entry)
                .collect::<Vec<_>>(),
        });

        assert_eq!(
            GpuPoolClaimTableHandOver::from_reply(&reply),
            Ok(GpuPoolClaimTableHandOver {
                claimant_column_index: 3,
                rows_leased_to_slots,
            })
        );
    }

    #[test]
    fn a_hand_over_row_without_a_lease_number_is_refused() {
        let reply = serde_json::json!({
            SURFACE_SHARE_CLAIMANT_COLUMN_FIELD: 3,
            SURFACE_SHARE_POOL_SLOTS_FIELD: [
                {SURFACE_SHARE_POOL_SLOT_ROW_FIELD: 0, "surface_id": "slot-a"},
            ],
        });

        assert!(GpuPoolClaimTableHandOver::from_reply(&reply).is_err());
    }
}
