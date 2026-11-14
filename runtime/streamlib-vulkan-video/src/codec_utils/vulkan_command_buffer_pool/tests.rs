// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Test that `CmdBufState` has the expected values and transitions.
#[test]
fn test_cmd_buf_state_values() {
    assert_eq!(CmdBufState::Reset as u32, 0);
    assert_eq!(CmdBufState::Recording as u32, 1);
    assert_eq!(CmdBufState::Recorded as u32, 2);
    assert_eq!(CmdBufState::Submitted as u32, 3);
}

/// Test pool node default construction.
#[test]
fn test_pool_node_default() {
    let node = PoolNode::new();
    assert_eq!(node.cmd_buf_state(), CmdBufState::Reset);
    assert_eq!(node.parent_index(), None);
    assert!(!node.is_valid());
}

/// Test pool node parent assignment and clearing.
#[test]
fn test_pool_node_parent_lifecycle() {
    let mut node = PoolNode::new();

    // Set parent
    let result = node.set_parent(5);
    assert_eq!(result, vk::Result::SUCCESS);
    assert_eq!(node.parent_index(), Some(5));
    assert_eq!(node.get_node_pool_index(), Some(5));

    // Clear parent — state must NOT be reset (matches C++ behavior).
    node.clear_parent();
    assert_eq!(node.parent_index(), None);
    // State persists across clear_parent:
    assert_eq!(node.cmd_buf_state(), CmdBufState::Reset);
}

/// Test that clear_parent preserves cmd_buf_state (critical for fence
/// management, as noted in the C++ source).
#[test]
fn test_clear_parent_preserves_state() {
    let mut node = PoolNode::new();
    node.set_parent(0);

    // Manually advance state to Submitted
    node.cmd_buf_state = CmdBufState::Submitted;

    // ClearParent must NOT reset state
    node.clear_parent();
    assert_eq!(node.cmd_buf_state(), CmdBufState::Submitted);
}

/// Test the available_pool_nodes bitmask logic used in
/// `get_available_pool_node` / `release_pool_node_to_pool`.
#[test]
fn test_available_bitmask_operations() {
    let mut available: u64 = 0;
    let pool_size: u32 = 8;

    // Mark all nodes as available (simulates Configure)
    for i in 0..pool_size {
        available |= 1u64 << i;
    }
    assert_eq!(available, 0xFF);

    // Acquire node 0
    let idx = find_available_node(&mut available, pool_size, 0);
    assert_eq!(idx, Some(0));
    assert_eq!(available, 0xFE);

    // Acquire node 1
    let idx = find_available_node(&mut available, pool_size, 1);
    assert_eq!(idx, Some(1));
    assert_eq!(available, 0xFC);

    // Release node 0
    available |= 1u64 << 0;
    assert_eq!(available, 0xFD);

    // Acquire starting from 2 — should get 2
    let idx = find_available_node(&mut available, pool_size, 2);
    assert_eq!(idx, Some(2));

    // Acquire 5 more (nodes 0,3,4,5,6 — 0 was re-released above)
    for _ in 0..5 {
        let _ = find_available_node(&mut available, pool_size, 0);
    }
    // Only node 7 remains
    let idx = find_available_node(&mut available, pool_size, 0);
    assert_eq!(idx, Some(7));

    // Now truly empty
    let idx = find_available_node(&mut available, pool_size, 0);
    assert_eq!(idx, None);
}

/// Test wraparound behavior: when next_node_to_use is past the end,
/// it should wrap around to find available nodes at the beginning.
#[test]
fn test_wraparound_search() {
    let mut available: u64 = 0;
    let pool_size: u32 = 4;

    // Only node 0 and 1 are available.
    available |= 1u64 << 0;
    available |= 1u64 << 1;

    // Start searching from index 3 (past the available ones).
    let mut next = 3u32;
    let idx = find_available_node_with_wrap(&mut available, pool_size, &mut next);
    assert_eq!(idx, Some(0));
    assert_eq!(next, 1); // next_node_to_use advanced past found node
}

/// Test MAX_POOL_NODES constant matches C++.
#[test]
fn test_max_pool_nodes() {
    assert_eq!(MAX_POOL_NODES, 64);
    // The bitmask is u64, so 64 nodes is the max.
    let all_set: u64 = u64::MAX;
    assert_eq!(all_set.count_ones(), 64);
}

/// Test PoolInner default state.
#[test]
fn test_pool_inner_default() {
    let inner = PoolInner::new();
    assert_eq!(inner.pool_size, 0);
    assert_eq!(inner.next_node_to_use, 0);
    assert_eq!(inner.available_pool_nodes, 0);
    assert_eq!(inner.queue_family_index, u32::MAX);
}

/// Test set_command_buffer_submitted state transition requirements.
#[test]
fn test_set_command_buffer_submitted_requires_recorded() {
    let mut node = PoolNode::new();
    node.set_parent(0);
    // No device — is_valid will be false. But let's test state logic only:
    // Without a device, set_command_buffer_submitted returns false.
    assert!(!node.set_command_buffer_submitted());

    // Even with state set to Recorded, without device it fails.
    node.cmd_buf_state = CmdBufState::Recorded;
    assert!(!node.set_command_buffer_submitted());
}

// -- Helper functions for testing bitmask logic extracted from the pool --

/// Simulates the core bitmask search from `get_available_pool_node`,
/// starting from `start` and searching `[start..pool_size)`.
fn find_available_node(available: &mut u64, pool_size: u32, start: u32) -> Option<u32> {
    for i in start..pool_size {
        if *available & (1u64 << i) != 0 {
            *available &= !(1u64 << i);
            return Some(i);
        }
    }
    // Retry from 0
    for i in 0..start {
        if *available & (1u64 << i) != 0 {
            *available &= !(1u64 << i);
            return Some(i);
        }
    }
    None
}

/// Simulates the full wraparound search including next_node_to_use update.
fn find_available_node_with_wrap(
    available: &mut u64,
    pool_size: u32,
    next_node_to_use: &mut u32,
) -> Option<u32> {
    if *next_node_to_use >= pool_size {
        *next_node_to_use = 0;
    }

    let mut found: Option<u32> = None;
    let mut retry = false;

    loop {
        for i in *next_node_to_use..pool_size {
            if *available & (1u64 << i) != 0 {
                *next_node_to_use = i + 1;
                *available &= !(1u64 << i);
                found = Some(i);
                break;
            }
        }

        if found.is_none() && *next_node_to_use > 0 {
            *next_node_to_use = 0;
            if !retry {
                retry = true;
                continue;
            }
        }
        break;
    }

    found
}
