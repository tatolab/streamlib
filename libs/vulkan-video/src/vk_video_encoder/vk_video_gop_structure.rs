// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkVideoGopStructure.h + VkVideoGopStructure.cpp
//!
//! GOP (Group of Pictures) pattern generation: determines frame types (IDR, I, P, B),
//! encode order, and intra-refresh indices for each frame in a video sequence.

/// Maximum GOP size.
pub const MAX_GOP_SIZE: u32 = 64;

/// Frame types in the GOP structure.
///
/// Matches the C++ `VkVideoGopStructure::FrameType` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum FrameType {
    P = 0,
    B = 1,
    I = 2,
    Idr = 3,
    IntraRefresh = 6,
    Invalid = -1,
}

impl FrameType {
    /// Returns the human-readable name for a frame type.
    ///
    /// Equivalent to the C++ `GetFrameTypeName` static method.
    pub fn name(self) -> &'static str {
        match self {
            FrameType::P => "P",
            FrameType::B => "B",
            FrameType::I => "I",
            FrameType::Idr => "IDR",
            FrameType::IntraRefresh => "INTRA_REFRESH",
            FrameType::Invalid => "UNDEFINED",
        }
    }
}

/// Flags for GOP position entries.
///
/// Matches the C++ `VkVideoGopStructure::Flags` enum.
pub mod flags {
    /// Frame is a reference frame.
    pub const IS_REF: u32 = 1 << 0;
    /// Last reference in the GOP (closed GOP boundary).
    pub const CLOSE_GOP: u32 = 1 << 1;
    /// Non-uniform GOP part of sequence (usually used to terminate GOP).
    pub const NONUNIFORM_GOP: u32 = 1 << 2;
    /// This frame is part of an intra-refresh cycle.
    pub const INTRA_REFRESH: u32 = 1 << 3;
}

/// Mutable state tracked across frames within a GOP sequence.
///
/// Equivalent to the C++ `VkVideoGopStructure::GopState` struct.
#[derive(Debug, Clone)]
pub struct GopState {
    pub position_in_input_order: u32,
    pub last_ref_in_input_order: u32,
    pub last_ref_in_encode_order: u32,
    pub intra_refresh_counter: u32,
    pub intra_refresh_cycle_restarted: bool,
    pub intra_refresh_start_skipped: bool,
}

impl Default for GopState {
    fn default() -> Self {
        Self {
            position_in_input_order: 0,
            last_ref_in_input_order: 0,
            last_ref_in_encode_order: 0,
            intra_refresh_counter: 0,
            intra_refresh_cycle_restarted: false,
            intra_refresh_start_skipped: false,
        }
    }
}

/// Position information for a single frame within the GOP.
///
/// Equivalent to the C++ `VkVideoGopStructure::GopPosition` struct.
#[derive(Debug, Clone)]
pub struct GopPosition {
    /// Input order in the IDR sequence.
    pub input_order: u32,
    /// Encode order in the IDR sequence.
    pub encode_order: u32,
    /// Position in GOP in input order.
    pub in_gop: u32,
    /// Number of B frames in this part of the GOP, -1 if not a B frame.
    pub num_b_frames: i8,
    /// The B position in GOP, -1 if not a B frame.
    pub b_frame_pos: i8,
    /// The type of the picture.
    pub picture_type: FrameType,
    /// One or multiple flags from [`flags`].
    pub flags: u32,
    /// Index of the frame within the intra-refresh cycle.
    pub intra_refresh_index: u32,
}

impl GopPosition {
    /// Create a new `GopPosition` for the given input-order position.
    pub fn new(position_in_gop_in_input_order: u32) -> Self {
        Self {
            input_order: position_in_gop_in_input_order,
            encode_order: 0,
            in_gop: 0,
            num_b_frames: -1,
            b_frame_pos: -1,
            picture_type: FrameType::Invalid,
            flags: 0,
            intra_refresh_index: u32::MAX,
        }
    }
}

/// GOP structure generator.
///
/// Determines frame types, encode order, and intra-refresh indices for each
/// frame in a video sequence based on the configured GOP parameters.
///
/// Equivalent to the C++ `VkVideoGopStructure` class.
#[derive(Debug, Clone)]
pub struct VkVideoGopStructure {
    gop_frame_count: u32,
    consecutive_b_frame_count: u8,
    gop_frame_cycle: u8,
    temporal_layer_count: u8,
    idr_period: u32,
    last_frame_type: FrameType,
    pre_closed_gop_anchor_frame_type: FrameType,
    closed_gop: bool,
    intra_refresh_cycle_duration: u32,
    intra_refresh_cycle_restart_index: u32,
    intra_refresh_skipped_start_index: u32,
}

impl VkVideoGopStructure {
    /// Create a new GOP structure with the given parameters.
    ///
    /// Equivalent to the C++ constructor.
    pub fn new(
        gop_frame_count: u8,
        idr_period: i32,
        consecutive_b_frame_count: u8,
        temporal_layer_count: u8,
        last_frame_type: FrameType,
        pre_idr_anchor_frame_type: FrameType,
        closed_gop: bool,
        intra_refresh_cycle_duration: u32,
    ) -> Self {
        // C++ uses uint8_t cast which wraps on overflow; replicate with wrapping_add.
        let gop_frame_cycle = consecutive_b_frame_count.wrapping_add(1);
        Self {
            gop_frame_count: gop_frame_count as u32,
            consecutive_b_frame_count,
            gop_frame_cycle,
            temporal_layer_count,
            idr_period: idr_period as u32,
            last_frame_type,
            pre_closed_gop_anchor_frame_type: pre_idr_anchor_frame_type,
            closed_gop,
            intra_refresh_cycle_duration,
            intra_refresh_cycle_restart_index: 0,
            intra_refresh_skipped_start_index: 0,
        }
    }

    /// Initialize (recompute) the GOP frame cycle.
    ///
    /// Equivalent to the C++ `Init` method.
    pub fn init(&mut self, _max_num_frames: u64) -> bool {
        self.gop_frame_cycle = self.consecutive_b_frame_count.wrapping_add(1);
        true
    }

    // -- Getters / Setters --

    pub fn set_gop_frame_count(&mut self, count: u32) {
        self.gop_frame_count = count;
    }
    pub fn gop_frame_count(&self) -> u32 {
        self.gop_frame_count
    }

    pub fn set_idr_period(&mut self, period: u32) {
        self.idr_period = period;
    }
    pub fn idr_period(&self) -> u32 {
        self.idr_period
    }

    pub fn set_consecutive_b_frame_count(&mut self, count: u8) {
        self.consecutive_b_frame_count = count;
    }
    pub fn consecutive_b_frame_count(&self) -> u8 {
        self.consecutive_b_frame_count
    }

    pub fn set_intra_refresh_cycle_duration(&mut self, duration: u32) {
        self.intra_refresh_cycle_duration = duration;
    }

    pub fn set_intra_refresh_cycle_restart_index(&mut self, index: u32) {
        self.intra_refresh_cycle_restart_index = index;
    }

    pub fn set_intra_refresh_skipped_start_index(&mut self, index: u32) {
        self.intra_refresh_skipped_start_index = index;
    }

    pub fn set_temporal_layer_count(&mut self, count: u8) {
        self.temporal_layer_count = count;
    }
    pub fn temporal_layer_count(&self) -> u8 {
        self.temporal_layer_count
    }

    pub fn set_closed_gop(&mut self) {
        self.closed_gop = true;
    }
    pub fn is_closed_gop(&self) -> bool {
        self.closed_gop
    }

    pub fn set_last_frame_type(&mut self, frame_type: FrameType) -> bool {
        self.last_frame_type = frame_type;
        true
    }

    // -- Period / reference delta helpers --

    /// Compute the distance from the current position to the next period boundary.
    pub fn get_period_delta(&self, gop_state: &GopState, period: u32) -> u32 {
        if period > 0 {
            period - (gop_state.position_in_input_order % period)
        } else {
            i32::MAX as u32
        }
    }

    /// Compute the distance from the last reference to the next period boundary.
    pub fn get_ref_delta(&self, gop_state: &GopState, period_delta: u32) -> u32 {
        let period_position = period_delta + gop_state.position_in_input_order;
        period_position - gop_state.last_ref_in_input_order
    }

    /// Determine the position of the current frame in the GOP.
    ///
    /// Returns `true` if this frame starts a new IDR sequence.
    ///
    /// This is a faithful port of the C++ `GetPositionInGOP` method,
    /// preserving all logic paths including closed-GOP promotion,
    /// B-frame consecutive count adjustment, and intra-refresh handling.
    pub fn get_position_in_gop(
        &self,
        gop_state: &mut GopState,
        gop_pos: &mut GopPosition,
        first_frame: bool,
        frames_left: u32,
    ) -> bool {
        *gop_pos = GopPosition::new(gop_state.position_in_input_order);

        // IDR frame detection
        if first_frame
            || (self.idr_period > 0
                && (gop_state.position_in_input_order % self.idr_period) == 0)
        {
            gop_pos.picture_type = FrameType::Idr;
            gop_pos.input_order = 0;
            gop_pos.flags |= flags::IS_REF;
            gop_state.last_ref_in_input_order = 0;
            gop_state.last_ref_in_encode_order = 0;
            gop_state.position_in_input_order = 1;
            gop_state.intra_refresh_counter = 0;
            return true;
        }

        gop_pos.input_order = gop_state.position_in_input_order;

        let mut consecutive_b_frame_count = self.consecutive_b_frame_count;

        gop_pos.in_gop = gop_state.position_in_input_order % self.gop_frame_count;

        if gop_pos.in_gop == 0 {
            // Start of a new (open or closed) GOP
            gop_pos.picture_type = FrameType::I;
            consecutive_b_frame_count = (gop_state.position_in_input_order
                - gop_state.last_ref_in_input_order
                - 1) as u8;
            gop_state.intra_refresh_counter = 0;
        } else if (gop_pos.in_gop % self.gop_frame_cycle as u32) == 0 {
            // Start of min/sub-GOP
            gop_pos.picture_type = FrameType::P;
            consecutive_b_frame_count = (gop_state.position_in_input_order
                - gop_state.last_ref_in_input_order
                - 1) as u8;
        } else if consecutive_b_frame_count > 0 {
            // B-frame promotion checks
            if (frames_left == 1)
                || (self.idr_period > 0
                    && gop_state.position_in_input_order == self.idr_period - 1)
                || (self.closed_gop && gop_pos.in_gop == self.gop_frame_count - 1)
            {
                gop_pos.picture_type = self.pre_closed_gop_anchor_frame_type;
                gop_pos.flags |= flags::CLOSE_GOP;
                consecutive_b_frame_count = (gop_state.position_in_input_order
                    - gop_state.last_ref_in_input_order
                    - 1) as u8;
            } else {
                // This is a B picture
                gop_pos.picture_type = FrameType::B;
                gop_pos.b_frame_pos = (gop_state.position_in_input_order
                    - gop_state.last_ref_in_input_order
                    - 1) as i8;

                let mut next_ref_delta = frames_left - 1;
                if self.idr_period > 0 {
                    next_ref_delta = next_ref_delta
                        .min(self.get_period_delta(gop_state, self.idr_period) - 1);
                }
                next_ref_delta = next_ref_delta.min(
                    self.get_period_delta(gop_state, self.gop_frame_count)
                        - if self.closed_gop { 1 } else { 0 },
                );
                next_ref_delta = next_ref_delta.min(
                    gop_state.last_ref_in_input_order + self.gop_frame_cycle as u32
                        - gop_pos.input_order,
                );

                consecutive_b_frame_count =
                    (gop_pos.b_frame_pos as u32 + next_ref_delta) as u8;
                gop_pos.num_b_frames = consecutive_b_frame_count as i8;
            }
        }

        if gop_pos.picture_type == FrameType::B {
            gop_pos.encode_order = gop_state.position_in_input_order + 1;
        } else {
            if gop_state.position_in_input_order > consecutive_b_frame_count as u32 {
                gop_pos.encode_order =
                    gop_state.position_in_input_order - consecutive_b_frame_count as u32;
            } else {
                gop_pos.encode_order = gop_state.position_in_input_order;
            }

            gop_pos.flags |= flags::IS_REF;

            // Edge case: P-frame naturally before IDR boundary
            if self.idr_period > 0
                && ((gop_state.position_in_input_order + 1) % self.idr_period == 0)
            {
                gop_pos.flags |= flags::CLOSE_GOP;
            } else if self.closed_gop && (gop_pos.in_gop == self.gop_frame_count - 1) {
                gop_pos.flags |= flags::CLOSE_GOP;
            }

            gop_state.last_ref_in_input_order = gop_state.position_in_input_order;
            gop_state.last_ref_in_encode_order = gop_pos.encode_order;
        }

        // Intra-refresh handling
        if (gop_pos.picture_type == FrameType::P || gop_pos.picture_type == FrameType::B)
            && self.intra_refresh_cycle_duration > 0
        {
            // Mid-way intra-refresh restart
            if !gop_state.intra_refresh_cycle_restarted
                && gop_state.intra_refresh_counter >= self.intra_refresh_cycle_restart_index
            {
                gop_state.intra_refresh_counter = 0;
                gop_state.intra_refresh_cycle_restarted = true;
            }

            // Skipped-start handling
            if gop_state.intra_refresh_counter == 0 {
                if !gop_state.intra_refresh_start_skipped {
                    gop_state.intra_refresh_counter = self.intra_refresh_skipped_start_index;
                    gop_state.intra_refresh_start_skipped = true;
                } else {
                    gop_state.intra_refresh_start_skipped = false;
                }
            }

            gop_pos.intra_refresh_index = gop_state.intra_refresh_counter;
            gop_pos.flags |= flags::INTRA_REFRESH;

            gop_state.intra_refresh_counter =
                (gop_state.intra_refresh_counter + 1) % self.intra_refresh_cycle_duration;

            if gop_state.intra_refresh_counter == 0 {
                gop_state.intra_refresh_cycle_restarted = false;
            }
        }

        gop_state.position_in_input_order += 1;

        false
    }

    /// Check if a frame is a reference frame.
    pub fn is_frame_reference(&self, gop_pos: &GopPosition) -> bool {
        (gop_pos.flags & flags::IS_REF) != 0
    }

    /// Check if a frame is an intra-refresh frame.
    pub fn is_intra_refresh_frame(&self, gop_pos: &GopPosition) -> bool {
        (gop_pos.flags & flags::INTRA_REFRESH) != 0
    }

    /// Print the GOP structure for `num_frames` frames.
    ///
    /// Equivalent to the C++ `PrintGopStructure` virtual method.
    pub fn print_gop_structure(&self, num_frames: u64) {
        // Frame Index
        print!("\nFrame Index:   ");
        for frame_num in 0..num_frames {
            print!("{:3} ", frame_num);
        }

        // Frame Type
        print!("\nFrame Type:   ");
        let mut gop_state = GopState::default();
        let mut gop_pos = GopPosition::new(gop_state.position_in_input_order);
        for frame_num in 0..num_frames - 1 {
            let frames_left = (num_frames - frame_num) as u32;
            self.get_position_in_gop(
                &mut gop_state,
                &mut gop_pos,
                frame_num == 0,
                frames_left,
            );
            print!("{:>4}", gop_pos.picture_type.name());
        }
        self.get_position_in_gop(&mut gop_state, &mut gop_pos, false, 1);
        print!("{:>4}", gop_pos.picture_type.name());

        // Input order
        print!("\nInput  order:  ");
        gop_state = GopState::default();
        for frame_num in 0..num_frames - 1 {
            let frames_left = (num_frames - frame_num) as u32;
            self.get_position_in_gop(
                &mut gop_state,
                &mut gop_pos,
                frame_num == 0,
                frames_left,
            );
            print!("{:3} ", gop_pos.input_order);
        }
        self.get_position_in_gop(&mut gop_state, &mut gop_pos, false, 1);
        print!("{:3} ", gop_pos.input_order);

        // Encode order
        print!("\nEncode  order: ");
        gop_state = GopState::default();
        for frame_num in 0..num_frames - 1 {
            let frames_left = (num_frames - frame_num) as u32;
            self.get_position_in_gop(
                &mut gop_state,
                &mut gop_pos,
                frame_num == 0,
                frames_left,
            );
            print!("{:3} ", gop_pos.encode_order);
        }
        self.get_position_in_gop(&mut gop_state, &mut gop_pos, false, 1);
        print!("{:3} ", gop_pos.encode_order);

        // InGop order
        print!("\nInGop  order:  ");
        gop_state = GopState::default();
        for frame_num in 0..num_frames - 1 {
            let frames_left = (num_frames - frame_num) as u32;
            self.get_position_in_gop(
                &mut gop_state,
                &mut gop_pos,
                frame_num == 0,
                frames_left,
            );
            print!("{:3} ", gop_pos.in_gop);
        }
        self.get_position_in_gop(&mut gop_state, &mut gop_pos, false, 1);
        print!("{:3} ", gop_pos.in_gop);

        println!();
    }

    /// Dump a single frame's GOP structure info.
    ///
    /// Equivalent to the C++ `DumpFrameGopStructure` method.
    pub fn dump_frame_gop_structure(
        &self,
        gop_state: &mut GopState,
        _first_frame: bool,
        _last_frame: bool,
    ) {
        let mut gop_pos = GopPosition::new(gop_state.position_in_input_order);
        self.get_position_in_gop(gop_state, &mut gop_pos, false, u32::MAX);

        println!(
            "  {}, \t{}, \t{}, \t{}",
            gop_pos.input_order,
            gop_pos.encode_order,
            gop_pos.in_gop,
            gop_pos.picture_type.name()
        );
    }

    /// Dump GOP structure for a range of frames.
    ///
    /// Equivalent to the C++ `DumpFramesGopStructure` method.
    pub fn dump_frames_gop_structure(
        &self,
        first_frame_num_in_input_order: u64,
        num_frames: u64,
    ) {
        println!("Input Encode Position  Frame ");
        println!("order order   in GOP   type  ");
        let last = first_frame_num_in_input_order + num_frames - 1;
        let mut gop_state = GopState::default();
        for _ in first_frame_num_in_input_order..last {
            self.dump_frame_gop_structure(&mut gop_state, false, false);
        }
        self.dump_frame_gop_structure(&mut gop_state, true, false);
    }
}

impl Default for VkVideoGopStructure {
    fn default() -> Self {
        Self::new(8, 60, 2, 1, FrameType::P, FrameType::P, false, 0)
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_idr_first_frame() {
        let gop = VkVideoGopStructure::new(8, 60, 2, 1, FrameType::P, FrameType::P, false, 0);
        let mut state = GopState::default();
        let mut pos = GopPosition::new(0);
        let is_idr = gop.get_position_in_gop(&mut state, &mut pos, true, 100);
        assert!(is_idr);
        assert_eq!(pos.picture_type, FrameType::Idr);
        assert_eq!(pos.input_order, 0);
        assert!(pos.flags & flags::IS_REF != 0);
    }

    #[test]
    fn test_idr_period_boundary() {
        let gop = VkVideoGopStructure::new(8, 10, 0, 1, FrameType::P, FrameType::P, false, 0);
        let mut state = GopState::default();
        let mut pos = GopPosition::new(0);

        // First frame is IDR
        gop.get_position_in_gop(&mut state, &mut pos, true, 100);
        assert_eq!(pos.picture_type, FrameType::Idr);

        // Frames 1..9 are P or I
        for i in 1..10 {
            gop.get_position_in_gop(&mut state, &mut pos, false, 100 - i);
        }

        // Frame 10 should be IDR again (period=10)
        let is_idr = gop.get_position_in_gop(&mut state, &mut pos, false, 90);
        assert!(is_idr);
        assert_eq!(pos.picture_type, FrameType::Idr);
    }

    #[test]
    fn test_p_only_gop() {
        let gop = VkVideoGopStructure::new(4, 60, 0, 1, FrameType::P, FrameType::P, false, 0);
        let mut state = GopState::default();
        let mut pos = GopPosition::new(0);

        // IDR
        gop.get_position_in_gop(&mut state, &mut pos, true, 20);
        assert_eq!(pos.picture_type, FrameType::Idr);

        // P frames (no B frames since consecutive_b_frame_count=0)
        gop.get_position_in_gop(&mut state, &mut pos, false, 19);
        assert_eq!(pos.picture_type, FrameType::P);

        gop.get_position_in_gop(&mut state, &mut pos, false, 18);
        assert_eq!(pos.picture_type, FrameType::P);

        gop.get_position_in_gop(&mut state, &mut pos, false, 17);
        assert_eq!(pos.picture_type, FrameType::P);

        // Frame 4: in_gop == 0 => I frame (new GOP)
        gop.get_position_in_gop(&mut state, &mut pos, false, 16);
        assert_eq!(pos.picture_type, FrameType::I);
    }

    #[test]
    fn test_b_frames_present() {
        // GOP=8, IDR=60, 2 consecutive B frames
        let gop = VkVideoGopStructure::new(8, 60, 2, 1, FrameType::P, FrameType::P, false, 0);
        let mut state = GopState::default();
        let mut pos = GopPosition::new(0);

        // IDR
        gop.get_position_in_gop(&mut state, &mut pos, true, 30);
        assert_eq!(pos.picture_type, FrameType::Idr);

        // Frames 1,2 should be B
        gop.get_position_in_gop(&mut state, &mut pos, false, 29);
        assert_eq!(pos.picture_type, FrameType::B);

        gop.get_position_in_gop(&mut state, &mut pos, false, 28);
        assert_eq!(pos.picture_type, FrameType::B);

        // Frame 3: cycle boundary => P
        gop.get_position_in_gop(&mut state, &mut pos, false, 27);
        assert_eq!(pos.picture_type, FrameType::P);
    }

    #[test]
    fn test_frame_type_name() {
        assert_eq!(FrameType::P.name(), "P");
        assert_eq!(FrameType::B.name(), "B");
        assert_eq!(FrameType::I.name(), "I");
        assert_eq!(FrameType::Idr.name(), "IDR");
        assert_eq!(FrameType::IntraRefresh.name(), "INTRA_REFRESH");
        assert_eq!(FrameType::Invalid.name(), "UNDEFINED");
    }

    #[test]
    fn test_is_frame_reference() {
        let gop = VkVideoGopStructure::default();
        let mut pos = GopPosition::new(0);
        pos.flags = 0;
        assert!(!gop.is_frame_reference(&pos));
        pos.flags = flags::IS_REF;
        assert!(gop.is_frame_reference(&pos));
    }

    #[test]
    fn test_closed_gop() {
        // With closed GOP, last frame in GOP should get CLOSE_GOP flag
        let gop = VkVideoGopStructure::new(4, 60, 0, 1, FrameType::P, FrameType::P, true, 0);
        let mut state = GopState::default();
        let mut pos = GopPosition::new(0);

        // IDR
        gop.get_position_in_gop(&mut state, &mut pos, true, 20);
        assert_eq!(pos.picture_type, FrameType::Idr);

        // Frames 1,2 P
        gop.get_position_in_gop(&mut state, &mut pos, false, 19);
        gop.get_position_in_gop(&mut state, &mut pos, false, 18);

        // Frame 3: last in GOP (in_gop == 3 == gop_frame_count-1)
        gop.get_position_in_gop(&mut state, &mut pos, false, 17);
        assert!(pos.flags & flags::CLOSE_GOP != 0);
    }
}
