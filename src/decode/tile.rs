use crate::block::{
    PARTITION_CONTEXTS, PARTITION_PLOFFSET, PartitionType, SQUARE_SPLIT_CONTEXTS, TxSize,
};
use crate::cdf_context::FrameCdfContext;
use crate::cdf_tables::*;
use crate::decode::entropy::DaalaDecoder;
use crate::scan::{TxClass, get_scan_for_tx, get_tx_class};
use std::collections::{HashSet, VecDeque};

const FIRST_MODE_COUNT: usize = 13;
const SECOND_MODE_COUNT: usize = 16;
const NON_DIRECTIONAL_MODES_COUNT: usize = 5;
const CFL_SIGNS: usize = 3;

const ADAPTIVE_TABLE: [usize; 5] = [4, 8, 16, 32, 64];
const CDEF_UNIT_SIZE: usize = 64;
const CCSO_UNIT_SIZE: usize = 256;
const LUMA_MODE_COUNT: usize = 61;
const TOTAL_ANGLE_DELTA_COUNT: usize = 7;
const MAX_ANGLE_DELTA: i8 = 3;
const INTRABC_REFMV_BANK_SIZE: usize = 4;
const MAX_RMB_SB_HITS: usize = 64;
const BANK_1ST_UNIT_UPDATE_COUNT: usize = 4;
const BANK_UNIT_MAX_ALLOWED_LEFTOVER_UPDATES: usize = 16;
const AV2_EXT_TX_USED_FLAG: [u16; 10] = [
    0x0001, 0x0201, 0x0C37, 0xFE37, 0x020F, 0x0E0F, 0x0FFF, 0xFFFF, 0xFFFF, 0x0E01,
];

const REORDERED_Y_MODE: [u8; 13] = [0, 9, 10, 11, 12, 3, 8, 1, 5, 4, 6, 2, 7];

const DEFAULT_MODE_LIST_Y: [u8; 56] = [
    17, 45, 3, 10, 24, 31, 38, 52, 15, 19, 43, 47, 1, 5, 8, 12, 22, 26, 29, 33, 36, 40, 50, 54, 16,
    18, 44, 46, 2, 4, 9, 11, 23, 25, 30, 32, 37, 39, 51, 53, 14, 20, 42, 48, 0, 6, 7, 13, 21, 27,
    28, 34, 35, 41, 49, 55,
];

const DEFAULT_MODE_LIST_UV: [u8; 8] = [1, 2, 3, 4, 8, 5, 6, 7];

fn is_directional_mode(mode: u8) -> bool {
    mode >= 1 && mode <= 8
}

fn set_y_mode_and_delta_angle(joint_y_mode: u8) -> (u8, i8) {
    let joint = joint_y_mode as usize;
    if joint < NON_DIRECTIONAL_MODES_COUNT {
        (REORDERED_Y_MODE[joint], 0)
    } else {
        let base = (joint - NON_DIRECTIONAL_MODES_COUNT) / TOTAL_ANGLE_DELTA_COUNT
            + NON_DIRECTIONAL_MODES_COUNT;
        let delta = ((joint - NON_DIRECTIONAL_MODES_COUNT) % TOTAL_ANGLE_DELTA_COUNT) as i8
            - MAX_ANGLE_DELTA;
        (REORDERED_Y_MODE[base], delta)
    }
}

fn build_y_intra_mode_list(
    block_w: usize,
    block_h: usize,
    left_joint: u8,
    above_joint: u8,
) -> [u8; LUMA_MODE_COUNT] {
    let mut list = [0u8; LUMA_MODE_COUNT];
    let mut selected = [false; LUMA_MODE_COUNT];
    let mut idx = 0;

    for i in 0..NON_DIRECTIONAL_MODES_COUNT {
        list[idx] = i as u8;
        selected[i] = true;
        idx += 1;
    }

    let is_small_block = block_w * block_h < 64;

    if !is_small_block {
        let is_left_dir = (left_joint as usize) >= NON_DIRECTIONAL_MODES_COUNT;
        let is_above_dir = (above_joint as usize) >= NON_DIRECTIONAL_MODES_COUNT;

        let mut neighbor_joints = [left_joint, above_joint];
        let mut dir_cnt = (is_left_dir as usize) + (is_above_dir as usize);

        if dir_cnt == 2 && neighbor_joints[0] == neighbor_joints[1] {
            dir_cnt = 1;
        }
        if dir_cnt == 1 && !is_left_dir {
            neighbor_joints[0] = neighbor_joints[1];
        }

        for i in 0..dir_cnt {
            let joint = neighbor_joints[i] as usize;
            if !selected[joint] {
                list[idx] = joint as u8;
                selected[joint] = true;
                idx += 1;
            }
        }

        let is_large_block = block_w * block_h > 64;
        if is_large_block {
            for offset in 0..4usize {
                for j in 0..dir_cnt {
                    let nj = neighbor_joints[j] as usize;
                    let left_derived = (nj - offset + 50) % 56 + NON_DIRECTIONAL_MODES_COUNT;
                    let right_derived = (nj + offset - 4) % 56 + NON_DIRECTIONAL_MODES_COUNT;

                    if !selected[left_derived] {
                        list[idx] = left_derived as u8;
                        selected[left_derived] = true;
                        idx += 1;
                    }
                    if !selected[right_derived] {
                        list[idx] = right_derived as u8;
                        selected[right_derived] = true;
                        idx += 1;
                    }
                }
            }
        }
    }

    for i in 0..(LUMA_MODE_COUNT - NON_DIRECTIONAL_MODES_COUNT) {
        if idx >= LUMA_MODE_COUNT {
            break;
        }
        let joint = DEFAULT_MODE_LIST_Y[i] as usize + NON_DIRECTIONAL_MODES_COUNT;
        if !selected[joint] {
            list[idx] = joint as u8;
            selected[joint] = true;
            idx += 1;
        }
    }

    list
}

fn build_uv_intra_mode_list(y_mode: u8) -> [u8; 13] {
    let mut list = [0u8; 13];
    let mut selected = [false; 13];
    let mut idx = 0usize;

    if is_directional_mode(y_mode) {
        list[idx] = y_mode;
        selected[y_mode as usize] = true;
        idx += 1;
    }

    for &mode in &[0u8, 9, 10, 11, 12] {
        if !selected[mode as usize] {
            list[idx] = mode;
            selected[mode as usize] = true;
            idx += 1;
        }
    }

    for &mode in &DEFAULT_MODE_LIST_UV {
        if !selected[mode as usize] {
            list[idx] = mode;
            selected[mode as usize] = true;
            idx += 1;
        }
    }

    list
}

fn intra_mode_to_tx_type(pred_mode: u8) -> usize {
    match pred_mode {
        0 | 3 => DCT_DCT,
        1 | 5 | 8 | 10 => ADST_DCT,
        2 | 6 | 7 | 11 => DCT_ADST,
        4 | 9 | 12 => ADST_ADST,
        13 => DCT_DCT,
        _ => DCT_DCT,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeType {
    Shared,
    Luma,
    Chroma,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChromaRefInfo {
    is_chroma_ref: bool,
    offset_started: bool,
    x_base: usize,
    y_base: usize,
    w_base: usize,
    h_base: usize,
}

impl ChromaRefInfo {
    fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self {
            is_chroma_ref: true,
            offset_started: false,
            x_base: x,
            y_base: y,
            w_base: w,
            h_base: h,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChromaRefContext {
    info: ChromaRefInfo,
    partition: PartitionType,
    width: usize,
    height: usize,
}

const SKIP_CONTEXTS_Y: [[u8; 5]; 5] = [
    [1, 2, 2, 3, 3],
    [2, 2, 3, 3, 4],
    [2, 3, 3, 4, 4],
    [3, 3, 4, 4, 5],
    [3, 4, 4, 5, 5],
];

struct TileState {
    cdef_read: HashSet<(usize, usize)>,
    ccso_read: [HashSet<(usize, usize)>; 3],
    above_ctx: [Vec<u8>; 3],
    left_ctx: [Vec<u8>; 3],
    above_part_ctx: [Vec<u8>; 2],
    left_part_ctx: [Vec<u8>; 2],
    sb_row_y: usize,
    sdp_luma_partition_64x64: Option<PartitionType>,
    sdp_luma_sub_partitions: Vec<PartitionType>,
    sdp_luma_tracking: bool,
    sdp_luma_depth: usize,
    cfl_allowed_in_sdp: bool,
    sdp_luma_partition_for_cfl: Option<PartitionType>,
}

struct IntrabcRefMvBank {
    entries: VecDeque<(i32, i32)>,
    sb_hits: usize,
    remain_hits: usize,
    unit_hits: usize,
}

impl IntrabcRefMvBank {
    fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(INTRABC_REFMV_BANK_SIZE),
            sb_hits: 0,
            remain_hits: 0,
            unit_hits: 0,
        }
    }

    fn reset_for_row(&mut self) {
        self.entries.clear();
        self.reset_hits_for_sb();
    }

    fn reset_hits_for_sb(&mut self) {
        self.sb_hits = 0;
        self.remain_hits = 0;
        self.unit_hits = 0;
    }

    fn newest_first(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        self.entries.iter().rev().copied()
    }

    fn update_lru(&mut self, mv: (i32, i32)) {
        if let Some(idx) = self.entries.iter().position(|&entry| entry == mv) {
            self.entries.remove(idx);
        } else if self.entries.len() == INTRABC_REFMV_BANK_SIZE {
            self.entries.pop_front();
        }
        self.entries.push_back(mv);
    }
}

fn get_txb_skip_ctx_y(
    above: &[u8],
    left: &[u8],
    block_w: usize,
    block_h: usize,
    tx_w: usize,
    tx_h: usize,
) -> usize {
    if block_w == tx_w && block_h == tx_h {
        return 0;
    }
    let mut top = 0u8;
    for &a in above {
        top |= a;
    }
    top &= COEFF_CONTEXT_MASK;
    let top_idx = (top as usize).min(4);

    let mut left_val = 0u8;
    for &l in left {
        left_val |= l;
    }
    left_val &= COEFF_CONTEXT_MASK;
    let left_idx = (left_val as usize).min(4);

    SKIP_CONTEXTS_Y[top_idx][left_idx] as usize
}

pub struct TileDecodeConfig {
    pub enable_cfl_intra: bool,
    pub enable_mhccp: bool,
    pub enable_fsc: bool,
    pub enable_idtx_intra: bool,
    pub enable_mrls: bool,
    pub enable_ist: bool,
    pub enable_cctx: bool,
    pub tx_mode_select: bool,
    pub coded_lossless: bool,
    pub reduced_tx_set_used: u8,
    pub reduced_tx_part_set: bool,
    pub allow_intrabc: bool,
    pub allow_screen_content_tools: bool,
    pub enable_sdp: bool,
    pub cdef_frame_enable: bool,
    pub nb_cdef_strengths: u32,
    pub ccso_frame_enable: bool,
    pub ccso_enable: [bool; 3],
    pub sb_size: usize,
    pub tcq_mode: bool,
    pub allow_parity_hiding: bool,
    pub enable_ext_partitions: bool,
    pub enable_uneven_4way_partitions: bool,
    pub enable_refmvbank: bool,
    pub max_bvp_drl_bits: u32,
    pub cur_frame_force_integer_mv: bool,
    pub allow_intrabc_morph_pred: bool,
}

const TCQ_NEXT_STATE: [[u8; 2]; 8] = [
    [0, 4],
    [4, 0],
    [1, 5],
    [5, 1],
    [6, 2],
    [2, 6],
    [7, 3],
    [3, 7],
];

fn tcq_quant(state: i32) -> usize {
    ((state & 2) != 0) as usize
}

fn tcq_next_state(state: i32, level: usize) -> i32 {
    let tcq_mode = state >> 8;
    let s = (state & 0xFF) as usize;
    if tcq_mode != 1 {
        return tcq_mode << 8;
    }
    let parity = level & 1;
    let s = if s >= 8 { 0 } else { s };
    (tcq_mode << 8) | TCQ_NEXT_STATE[s][parity] as i32
}

fn tcq_init_state(tcq_mode: bool) -> i32 {
    if tcq_mode { 1 << 8 } else { 0 }
}

impl Default for TileDecodeConfig {
    fn default() -> Self {
        Self {
            enable_cfl_intra: false,
            enable_mhccp: false,
            enable_fsc: false,
            enable_idtx_intra: false,
            enable_mrls: false,
            enable_ist: false,
            enable_cctx: false,
            tx_mode_select: false,
            coded_lossless: false,
            reduced_tx_set_used: 0,
            reduced_tx_part_set: false,
            allow_intrabc: false,
            allow_screen_content_tools: false,
            enable_sdp: false,
            cdef_frame_enable: false,
            nb_cdef_strengths: 0,
            ccso_frame_enable: false,
            ccso_enable: [false; 3],
            sb_size: 128,
            tcq_mode: false,
            allow_parity_hiding: false,
            enable_ext_partitions: false,
            enable_uneven_4way_partitions: false,
            enable_refmvbank: false,
            max_bvp_drl_bits: 3,
            cur_frame_force_integer_mv: false,
            allow_intrabc_morph_pred: false,
        }
    }
}

const COEFF_CONTEXT_BITS: u32 = 3;
const COEFF_CONTEXT_MASK: u8 = (1 << COEFF_CONTEXT_BITS) - 1;

pub struct DecodedSbCoeffs {
    pub y_levels: [i32; 1024],
    pub y_eob: u16,
    pub u_levels: [i32; 1024],
    pub u_eob: u16,
    pub v_levels: [i32; 1024],
    pub v_eob: u16,
    pub y_mode: u8,
    pub uv_mode: u8,
}

fn ceil_log2(n: u32) -> u32 {
    if n < 2 {
        return 0;
    }
    32 - (n - 1).leading_zeros()
}

fn get_adaptive_param(ctx: usize) -> u32 {
    let mut m = 0;
    while m < ADAPTIVE_TABLE.len() && ctx >= ADAPTIVE_TABLE[m] {
        m += 1;
    }
    (m + 1) as u32
}

fn get_padded_idx(idx: usize, bwl: u32) -> usize {
    idx + ((idx >> bwl) << TX_PAD_HOR_LOG2)
}


const LF_RC_LIM: usize = 2;
const LF_RC_LIM_UV: usize = 1;

fn is_lf(row: usize, col: usize, tx_class: TxClass, plane: usize) -> bool {
    match tx_class {
        TxClass::TwoD => {
            if plane == 0 {
                (row + col) < LF_2D_LIM
            } else {
                (row + col) < LF_2D_LIM_UV
            }
        }
        TxClass::Horiz => {
            if plane == 0 {
                col < LF_RC_LIM
            } else {
                col < LF_RC_LIM_UV
            }
        }
        TxClass::Vert => {
            if plane == 0 {
                row < LF_RC_LIM
            } else {
                row < LF_RC_LIM_UV
            }
        }
    }
}

fn get_lower_levels_ctx_eob(bwl: u32, height: usize, scan_idx: usize) -> usize {
    if scan_idx == 0 {
        return 0;
    }
    if scan_idx <= (height << bwl) / 8 {
        return 1;
    }
    if scan_idx <= (height << bwl) / 4 {
        return 2;
    }
    3
}





fn get_br_ctx_lf_eob(coeff_idx: usize, tx_class: TxClass) -> usize {
    if tx_class == TxClass::TwoD && coeff_idx == 0 {
        0
    } else {
        7
    }
}


fn get_br_lf_ctx_dc_2d(levels: &[u8], coeff_idx: usize, bwl: u32) -> usize {
    let stride = (1 << bwl) + TX_PAD_HOR;
    let pos = (coeff_idx >> bwl) * stride + (coeff_idx - ((coeff_idx >> bwl) << bwl));
    let mag = (levels[pos + 1] as usize).min(MAX_VAL_BR_CTX as usize)
        + (levels[pos + stride] as usize).min(MAX_VAL_BR_CTX as usize)
        + (levels[pos + stride + 1] as usize).min(MAX_VAL_BR_CTX as usize);
    ((mag + 1) >> 1).min(6)
}



const LF_SIG_COEF_CONTEXTS_2D: usize = 21;

fn get_nz_mag_lf(levels: &[u8], pos: usize, stride: usize, tx_class: TxClass) -> usize {
    let clip = 5usize;
    match tx_class {
        TxClass::TwoD => {
            (levels[pos + 1] as usize).min(clip)
                + (levels[pos + stride] as usize).min(clip)
                + (levels[pos + stride + 1] as usize).min(clip)
                + (levels[pos + 2] as usize).min(clip)
                + (levels[pos + 2 * stride] as usize).min(clip)
        }
        TxClass::Vert => {
            (levels[pos + 1] as usize).min(clip)
                + (levels[pos + stride] as usize).min(clip)
                + (levels[pos + 2 * stride] as usize).min(3)
                + (levels[pos + 3 * stride] as usize).min(3)
                + (levels[pos + 4 * stride] as usize).min(3)
        }
        TxClass::Horiz => {
            (levels[pos + 1] as usize).min(clip)
                + (levels[pos + stride] as usize).min(clip)
                + (levels[pos + 2] as usize).min(3)
                + (levels[pos + 3] as usize).min(3)
                + (levels[pos + 4] as usize).min(3)
        }
    }
}

fn get_nz_mag(levels: &[u8], pos: usize, stride: usize, tx_class: TxClass) -> usize {
    let clip = 3usize;
    match tx_class {
        TxClass::TwoD => {
            (levels[pos + 1] as usize).min(clip)
                + (levels[pos + stride] as usize).min(clip)
                + (levels[pos + stride + 1] as usize).min(clip)
                + (levels[pos + 2] as usize).min(clip)
                + (levels[pos + 2 * stride] as usize).min(clip)
        }
        TxClass::Vert => {
            (levels[pos + 1] as usize).min(clip)
                + (levels[pos + stride] as usize).min(clip)
                + (levels[pos + 2 * stride] as usize).min(clip)
                + (levels[pos + 3 * stride] as usize).min(clip)
                + (levels[pos + 4 * stride] as usize).min(clip)
        }
        TxClass::Horiz => {
            (levels[pos + 1] as usize).min(clip)
                + (levels[pos + stride] as usize).min(clip)
                + (levels[pos + 2] as usize).min(clip)
                + (levels[pos + 3] as usize).min(clip)
                + (levels[pos + 4] as usize).min(clip)
        }
    }
}

fn get_lower_levels_ctx_lf(levels: &[u8], coeff_idx: usize, bwl: u32, tx_class: TxClass) -> usize {
    let pos = get_padded_idx(coeff_idx, bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let mag = get_nz_mag_lf(levels, pos, stride, tx_class);
    let ctx = (mag + 1) >> 1;
    let row = coeff_idx >> bwl;
    let col = coeff_idx - (row << bwl);
    match tx_class {
        TxClass::TwoD => {
            if coeff_idx == 0 {
                ctx.min(8)
            } else if row + col < 2 {
                ctx.min(6) + 9
            } else {
                ctx.min(4) + 16
            }
        }
        TxClass::Vert => {
            if row == 0 {
                ctx.min(6) + LF_SIG_COEF_CONTEXTS_2D
            } else if row == 1 {
                ctx.min(4) + LF_SIG_COEF_CONTEXTS_2D + 7
            } else {
                ctx.min(4) + 16
            }
        }
        TxClass::Horiz => {
            if col == 0 {
                ctx.min(6) + LF_SIG_COEF_CONTEXTS_2D
            } else if col == 1 {
                ctx.min(4) + LF_SIG_COEF_CONTEXTS_2D + 7
            } else {
                ctx.min(4) + 16
            }
        }
    }
}

fn get_lower_levels_ctx(
    levels: &[u8],
    coeff_idx: usize,
    bwl: u32,
    tx_class: TxClass,
    plane: usize,
) -> usize {
    let pos = get_padded_idx(coeff_idx, bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let mag = get_nz_mag(levels, pos, stride, tx_class);
    let ctx = ((mag + 1) >> 1).min(4);
    if plane > 0 {
        return ctx;
    }
    match tx_class {
        TxClass::TwoD => {
            let row = coeff_idx >> bwl;
            let col = coeff_idx - (row << bwl);
            if row + col < 6 {
                ctx
            } else if row + col < 8 {
                ctx + 5
            } else {
                ctx + 10
            }
        }
        TxClass::Vert | TxClass::Horiz => ctx + 15,
    }
}

fn get_lower_levels_ctx_lf_chroma(
    levels: &[u8],
    coeff_idx: usize,
    bwl: u32,
    tx_class: TxClass,
    plane: usize,
) -> usize {
    let pos = get_padded_idx(coeff_idx, bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let mag = match tx_class {
        TxClass::TwoD => {
            (levels[pos + 1] as usize).min(5)
                + (levels[pos + stride] as usize).min(5)
                + (levels[pos + stride + 1] as usize).min(5)
        }
        TxClass::Vert | TxClass::Horiz => {
            (levels[pos + 1] as usize).min(5) + (levels[pos + stride] as usize).min(5)
        }
    };
    let ctx = ((mag + 1) >> 1).min(3);
    if plane == 1 { ctx } else { ctx + 4 }
}

fn get_lower_levels_ctx_chroma(
    levels: &[u8],
    coeff_idx: usize,
    bwl: u32,
    tx_class: TxClass,
    plane: usize,
) -> usize {
    let pos = get_padded_idx(coeff_idx, bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let mag = match tx_class {
        TxClass::TwoD => {
            (levels[pos + 1] as usize).min(3)
                + (levels[pos + stride] as usize).min(3)
                + (levels[pos + stride + 1] as usize).min(3)
        }
        TxClass::Vert | TxClass::Horiz => {
            (levels[pos + 1] as usize).min(3) + (levels[pos + stride] as usize).min(3)
        }
    };
    let ctx = ((mag + 1) >> 1).min(3);
    if plane == 1 { ctx } else { ctx + 4 }
}

fn get_br_lf_ctx(levels: &[u8], coeff_idx: usize, bwl: u32, tx_class: TxClass) -> usize {
    let row = coeff_idx >> bwl;
    let col = coeff_idx - (row << bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let pos = row * stride + col;
    let mag = match tx_class {
        TxClass::TwoD => {
            (levels[pos + 1] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + stride] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + stride + 1] as usize).min(MAX_VAL_BR_CTX as usize)
        }
        TxClass::Vert => {
            (levels[pos + 1] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + stride] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + 2 * stride] as usize).min(MAX_VAL_BR_CTX as usize)
        }
        TxClass::Horiz => {
            (levels[pos + 1] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + stride] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + 2] as usize).min(MAX_VAL_BR_CTX as usize)
        }
    };
    let mag = ((mag + 1) >> 1).min(6);
    match tx_class {
        TxClass::TwoD => {
            if coeff_idx == 0 {
                mag
            } else {
                mag + 7
            }
        }
        TxClass::Vert => {
            if row == 0 {
                mag + 7
            } else {
                mag + 7
            }
        }
        TxClass::Horiz => {
            if col == 0 {
                mag + 7
            } else {
                mag + 7
            }
        }
    }
}

fn get_br_ctx(levels: &[u8], coeff_idx: usize, bwl: u32, tx_class: TxClass) -> usize {
    let row = coeff_idx >> bwl;
    let col = coeff_idx - (row << bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let pos = row * stride + col;
    let mag = match tx_class {
        TxClass::TwoD => {
            (levels[pos + 1] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + stride] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + stride + 1] as usize).min(MAX_VAL_BR_CTX as usize)
        }
        TxClass::Vert => {
            (levels[pos + 1] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + stride] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + 2 * stride] as usize).min(MAX_VAL_BR_CTX as usize)
        }
        TxClass::Horiz => {
            (levels[pos + 1] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + stride] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + 2] as usize).min(MAX_VAL_BR_CTX as usize)
        }
    };
    ((mag + 1) >> 1).min(6)
}

fn get_br_ctx_chroma(levels: &[u8], coeff_idx: usize, bwl: u32, tx_class: TxClass) -> usize {
    let row = coeff_idx >> bwl;
    let col = coeff_idx - (row << bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let pos = row * stride + col;
    let mag = match tx_class {
        TxClass::TwoD => {
            (levels[pos + 1] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + stride] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + stride + 1] as usize).min(MAX_VAL_BR_CTX as usize)
        }
        TxClass::Vert | TxClass::Horiz => {
            (levels[pos + 1] as usize).min(MAX_VAL_BR_CTX as usize)
                + (levels[pos + stride] as usize).min(MAX_VAL_BR_CTX as usize)
        }
    };
    ((mag + 1) >> 1).min(3)
}

const PHTHRESH: usize = 4;

fn parity_hiding_allowed_tx_type(tx_type: usize) -> bool {
    tx_type != IDTX
}

fn get_base_ctx_ph(levels: &[u8], coeff_idx: usize, bwl: u32, tx_class: TxClass) -> usize {
    let pos = get_padded_idx(coeff_idx, bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let mag = get_nz_mag(levels, pos, stride, tx_class);
    ((mag + 1) >> 1).min(4)
}

fn is_dc_coeff(c: usize, row: usize, col: usize, tx_class: TxClass) -> bool {
    match tx_class {
        TxClass::TwoD => c == 0,
        TxClass::Vert => row == 0,
        TxClass::Horiz => col == 0,
    }
}

fn stored_level(abs_level: usize, lf: bool, plane: usize) -> u8 {
    let max = if lf {
        if plane == 0 {
            LF_MAX_BASE_BR_RANGE
        } else {
            LF_BASE_SYMBOLS - 1
        }
    } else {
        MAX_BASE_BR_RANGE
    };
    abs_level.min(max) as u8
}

fn compute_cul_level(levels: &[i32], eob: u16) -> u8 {
    if eob == 0 {
        return 0;
    }
    let mut sum = 0u32;
    for item in levels.iter().take(eob as usize) {
        sum += item.unsigned_abs();
    }
    let mut cul = sum.min(COEFF_CONTEXT_MASK as u32) as u8;
    let dc = levels[0];
    if dc < 0 {
        cul |= 1 << COEFF_CONTEXT_BITS;
    } else if dc > 0 {
        cul += 2 << COEFF_CONTEXT_BITS;
    }
    cul
}

fn compute_entropy_ctx(above: &[u8], left: &[u8]) -> usize {
    let above_nz = above.iter().any(|&x| x != 0) as usize;
    let left_nz = left.iter().any(|&x| x != 0) as usize;
    above_nz + left_nz
}

fn compute_dc_sign_ctx(above: &[u8], left: &[u8]) -> usize {
    const SIGNS: [i32; 3] = [0, -1, 1];
    let mut dc_sign = 0i32;
    for &a in above {
        let sign = (a >> COEFF_CONTEXT_BITS) as usize;
        dc_sign += SIGNS[sign.min(2)];
    }
    for &l in left {
        let sign = (l >> COEFF_CONTEXT_BITS) as usize;
        dc_sign += SIGNS[sign.min(2)];
    }
    if dc_sign < 0 {
        1
    } else if dc_sign > 0 {
        2
    } else {
        0
    }
}

fn decode_exp_golomb(ec: &mut DaalaDecoder, k: u32) -> u32 {
    let length = ec.decode_unary(21);
    let total_length = length + k;
    let x = 1u32 << total_length;
    let literal = ec.decode_literal(total_length);
    x + literal - (1u32 << k)
}

fn decode_truncated_rice(ec: &mut DaalaDecoder, m: u32, k: u32, cmax: u32) -> u32 {
    let q = ec.decode_unary(cmax);
    if q == cmax {
        let eg = decode_exp_golomb(ec, k);
        (cmax << m) + eg
    } else {
        let r = if m > 0 { ec.decode_literal(m) } else { 0 };
        (q << m) | r
    }
}

fn decode_adaptive_hr(ec: &mut DaalaDecoder, hr_ctx: usize) -> usize {
    let m = get_adaptive_param(hr_ctx);
    let k = m + 1;
    let cmax = (m + 4).min(6);
    decode_truncated_rice(ec, m, k, cmax) as usize
}


fn decode_low_range(
    ec: &mut DaalaDecoder,
    base_levels: usize,
    static_cdf: &[u16],
    mutable_cdf: Option<&mut Vec<u16>>,
) -> usize {
    let br_sym = if let Some(cdf) = mutable_cdf {
        ec.decode_symbol_mut(cdf)
    } else {
        ec.decode_symbol(static_cdf)
    };
    base_levels + 1 + br_sym
}



pub struct TuData {
    pub levels: Vec<i32>,
    pub eob: u16,
    pub tx_size: TxSize,
    pub tx_type: usize,
    pub row_offset: usize,
    pub col_offset: usize,
    pub cctx_type: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxPartitionType {
    None,
    Split,
    Horz,
    Vert,
    Horz4,
    Vert4,
}

pub struct DecodedBlock {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub is_chroma_ref: bool,
    pub chroma_x: usize,
    pub chroma_y: usize,
    pub chroma_width: usize,
    pub chroma_height: usize,
    pub tree_type: TreeType,
    pub is_intrabc: bool,
    pub morph_pred: bool,
    pub bv_row: i32,
    pub bv_col: i32,
    pub y_mode: u8,
    pub joint_y_mode: u8,
    pub uv_mode: u8,
    pub mrl_index: u8,
    pub cfl_alpha_u: i8,
    pub cfl_alpha_v: i8,
    pub cfl_idx: u8,
    pub mhccp_dir: u8,
    pub y_tu: Vec<TuData>,
    pub u_tu: Vec<TuData>,
    pub v_tu: Vec<TuData>,
    pub palette_y_colors: Vec<u16>,
    pub palette_y_map: Vec<u8>,
}

fn compute_eob_from_token(
    ec: &mut DaalaDecoder,
    eob_token: usize,
    cdf: &mut FrameCdfContext,
) -> u16 {
    let eob_pt = eob_token + 1;
    let bits = EOB_OFFSET_BITS[eob_pt] as u32;
    let base = EOB_GROUP_START[eob_pt];
    if bits > 0 {
        let msb = ec.decode_symbol_mut(&mut cdf.eob_extra) as u16;
        let rest = if bits > 1 {
            ec.decode_literal(bits - 1) as u16
        } else {
            0
        };
        let extra = (msb << (bits - 1)) | rest;
        base + extra
    } else {
        base
    }
}

fn decode_eob_var(
    ec: &mut DaalaDecoder,
    tx_size: TxSize,
    pl_ctx: usize,
    cdf: &mut FrameCdfContext,
) -> u16 {
    let num_coeffs = tx_size.num_coeffs();
    let (raw, extra_bits) = match num_coeffs {
        16 => (ec.decode_symbol_mut(&mut cdf.eob_multi_16[pl_ctx]), 0u32),
        32 => (ec.decode_symbol_mut(&mut cdf.eob_multi_32[pl_ctx]), 0),
        64 => (ec.decode_symbol_mut(&mut cdf.eob_multi_64[pl_ctx]), 0),
        128 => (ec.decode_symbol_mut(&mut cdf.eob_multi_128[pl_ctx]), 0),
        256 => (ec.decode_symbol_mut(&mut cdf.eob_multi_256[pl_ctx]), 1),
        512 => (ec.decode_symbol_mut(&mut cdf.eob_multi_512[pl_ctx]), 2),
        1024 => (ec.decode_symbol_mut(&mut cdf.eob_multi_1024[pl_ctx]), 2),
        _ => unreachable!("unexpected num_coeffs: {num_coeffs}"),
    };

    let eob_token = if extra_bits > 0 && raw >= EOB_PT_INDEX_COUNT - 1 {
        let extra = ec.decode_literal(extra_bits) as usize;
        (EOB_PT_INDEX_COUNT - 1) + extra
    } else {
        raw
    };

    compute_eob_from_token(ec, eob_token, cdf)
}

fn get_inter_eob_tx_ctx(tx_size: TxSize, eob: u16) -> usize {
    let bwl = tx_size.bwl() as usize;
    let eob_m1 = (eob - 1) as usize;
    let eoby = eob_m1 >> bwl;
    let eobx = eob_m1 - (eoby << bwl);
    let diag = eobx + eoby;
    let max_diag = tx_size.coded_width() + tx_size.coded_height() - 2;
    if diag < 2 {
        1
    } else if diag > max_diag.saturating_sub(2) {
        2
    } else {
        0
    }
}

fn get_tx_set_type(tx_size: TxSize, inter_block: bool, reduced_tx_set_used: u8) -> usize {
    let tx_size_sqr_up = tx_size.sqr_up();
    let tx_size_sqr = tx_size.sqr();
    if tx_size_sqr_up > 3 {
        if tx_size_sqr >= 3 {
            EXT_TX_SET_DCTONLY
        } else {
            EXT_TX_SET_LONG_SIDE_64
        }
    } else if tx_size_sqr_up == 3 {
        if tx_size_sqr == 3 {
            EXT_TX_SET_DCT_IDTX
        } else {
            EXT_TX_SET_LONG_SIDE_32
        }
    } else if reduced_tx_set_used != 0 {
        const REDUCED_SET_MAP: [[usize; 3]; 2] = [
            [EXT_NEW_TX_SET, EXT_TX_SET_DCT_IDTX, EXT_NEW_TX_SET],
            [
                EXT_TX_SET_DCT_IDTX,
                EXT_TX_SET_DCT_IDTX,
                EXT_TX_SET_DCT_IDTX_IDDCT,
            ],
        ];
        let reduced_idx = (reduced_tx_set_used.saturating_sub(1) as usize).min(2);
        REDUCED_SET_MAP[inter_block as usize][reduced_idx]
    } else if inter_block {
        if tx_size_sqr == 2 {
            EXT_TX_SET_DTT9_IDTX_1DDCT
        } else {
            EXT_TX_SET_ALL16
        }
    } else {
        EXT_NEW_TX_SET
    }
}

fn adjust_ext_tx_used_flag(tx_size: TxSize, tx_set_type: usize, ext_tx_used_flag: &mut u16) {
    let is_rect_horz = tx_size.width() > tx_size.height();
    if tx_set_type == EXT_TX_SET_LONG_SIDE_64 {
        if is_rect_horz {
            *ext_tx_used_flag &= !((1 << DCT_ADST) | (1 << DCT_FLIPADST) | (1 << V_DCT));
        } else {
            *ext_tx_used_flag &= !((1 << ADST_DCT) | (1 << FLIPADST_DCT) | (1 << H_DCT));
        }
    } else if tx_set_type == EXT_TX_SET_LONG_SIDE_32 {
        if is_rect_horz {
            *ext_tx_used_flag &=
                !((1 << DCT_ADST) | (1 << DCT_FLIPADST) | (1 << H_ADST) | (1 << H_FLIPADST));
        } else {
            *ext_tx_used_flag &=
                !((1 << ADST_DCT) | (1 << FLIPADST_DCT) | (1 << V_ADST) | (1 << V_FLIPADST));
        }
    }
}

fn get_chroma_tx_type(
    tx_size: TxSize,
    pred_mode: u8,
    inter_block: bool,
    reduced_tx_set_used: u8,
) -> usize {
    if inter_block {
        return DCT_DCT;
    }

    let tx_type = intra_mode_to_tx_type(pred_mode);
    let tx_set_type = get_tx_set_type(tx_size, inter_block, reduced_tx_set_used);
    let mut ext_tx_used_flag = AV2_EXT_TX_USED_FLAG[tx_set_type];
    if tx_set_type == EXT_TX_SET_LONG_SIDE_64 || tx_set_type == EXT_TX_SET_LONG_SIDE_32 {
        adjust_ext_tx_used_flag(tx_size, tx_set_type, &mut ext_tx_used_flag);
    }

    if (ext_tx_used_flag & (1 << tx_type)) == 0 {
        DCT_DCT
    } else {
        tx_type
    }
}

fn read_tx_type(
    ec: &mut DaalaDecoder,
    tx_size: TxSize,
    y_mode: u8,
    fsc_mode: usize,
    inter_block: bool,
    reduced_tx_set_used: u8,
    eob: u16,
    cdf: &mut FrameCdfContext,
) -> usize {
    if fsc_mode != 0 {
        return IDTX;
    }

    if !inter_block && eob == 1 {
        return DCT_DCT;
    }

    if inter_block {
        let tx_set_type = get_tx_set_type(tx_size, true, reduced_tx_set_used);
        if AV2_NUM_EXT_TX_SET[tx_set_type] <= 1 {
            return DCT_DCT;
        }
        if tx_set_type == EXT_TX_SET_LONG_SIDE_64 || tx_set_type == EXT_TX_SET_LONG_SIDE_32 {
            return DCT_DCT;
        }

        let eset = EXT_TX_SET_INDEX_INTER[tx_set_type];
        if eset <= 0 {
            return DCT_DCT;
        }

        let eob_tx_ctx = get_inter_eob_tx_ctx(tx_size, eob);
        let square_tx_size = tx_size.sqr().min(3);
        let tx_type_idx = if eset == 1 || eset == 2 {
            let set_idx = (eset - 1) as usize;
            let tx_set = ec
                .decode_symbol_mut(&mut cdf.inter_tx_type_set[set_idx][eob_tx_ctx][square_tx_size]);
            if tx_set == 0 {
                ec.decode_symbol_mut(&mut cdf.inter_tx_type_idx[set_idx][eob_tx_ctx])
            } else {
                INTER_TX_TYPE_INDEX_COUNT
                    + if eset == 1 {
                        ec.decode_symbol_mut(&mut cdf.inter_tx_type_offset_1[eob_tx_ctx])
                    } else {
                        ec.decode_symbol_mut(&mut cdf.inter_tx_type_offset_2[eob_tx_ctx])
                    }
            }
        } else {
            return DCT_DCT;
        };

        ec.trace_state(&format!(
            "after_tx_type_inter={} set_type={} eset={} sqr={} eob_ctx={}",
            tx_type_idx, tx_set_type, eset, square_tx_size, eob_tx_ctx
        ));
        return AV2_EXT_TX_INV[tx_set_type][tx_type_idx.min(15)];
    }

    if reduced_tx_set_used == 2 {
        return DCT_DCT;
    }

    let tx_size_sqr_up = tx_size.sqr_up();
    let tx_size_sqr = tx_size.sqr();

    if tx_size_sqr_up > 3 {
        if tx_size_sqr >= 3 {
            return DCT_DCT;
        }
        let short_side_idx =
            ec.decode_symbol_mut(&mut cdf.intra_ext_tx_short_side[tx_size_sqr.min(3)]);
        let is_rect_horz = tx_size.width() > tx_size.height();
        return EXT_INDEX_TO_TX_TYPE_LARGE_64[if is_rect_horz { 0 } else { 1 }][short_side_idx];
    }

    if tx_size_sqr_up == 3 {
        if tx_size_sqr == 3 {
            if reduced_tx_set_used == 0 {
                return DCT_DCT;
            }
            let tx_type_idx = ec.decode_symbol_mut(&mut cdf.intra_ext_tx_reduced);
            return if tx_type_idx == 0 { DCT_DCT } else { IDTX };
        }
        let is_long_side_dct = ec.decode_symbol_mut(&mut cdf.tx_ext_32_intra);
        let short_side_idx =
            ec.decode_symbol_mut(&mut cdf.intra_ext_tx_short_side[tx_size_sqr.min(3)]);
        let is_rect_horz = tx_size.width() > tx_size.height();
        return EXT_INDEX_TO_TX_TYPE_LARGE_32[1 - is_long_side_dct]
            [if is_rect_horz { 0 } else { 1 }][short_side_idx];
    }

    if reduced_tx_set_used == 1 {
        let tx_type_idx = ec.decode_symbol_mut(&mut cdf.intra_ext_tx_reduced);
        return if tx_type_idx == 0 { DCT_DCT } else { ADST_ADST };
    }

    let tx_type_idx = ec.decode_symbol_mut(&mut cdf.intra_ext_tx[tx_size_sqr.min(3)]);
    ec.trace_state(&format!(
        "after_tx_type={} sqr={} mode={}",
        tx_type_idx, tx_size_sqr, y_mode
    ));

    let size_info = tx_size.size_class();
    let mode = y_mode as usize;
    if mode < 13 && tx_type_idx < 7 {
        AV2_MD_IDX2TYPE[size_info][mode][tx_type_idx] as usize
    } else {
        DCT_DCT
    }
}

fn read_sec_tx_type(
    ec: &mut DaalaDecoder,
    tx_size: TxSize,
    primary_tx_type: usize,
    y_mode: u8,
    eob: usize,
    cdf: &mut FrameCdfContext,
) {
    if primary_tx_type != DCT_DCT && primary_tx_type != ADST_ADST {
        return;
    }

    if y_mode >= PAETH_PRED {
        return;
    }

    let width = tx_size.width();
    let height = tx_size.height();
    let square_tx_size = tx_size.sqr();

    let (st_size_class, eob_limit) = if width == 4 || height == 4 {
        (0, IST_4X4_HEIGHT)
    } else if width == 8 && height == 8 && primary_tx_type == DCT_DCT {
        (1, IST_8X8_HEIGHT_RED)
    } else if width >= 8 && height >= 8 && primary_tx_type == DCT_DCT {
        (2, IST_8X8_HEIGHT)
    } else if width >= 8 && height >= 8 {
        (3, IST_ADST_NZ_CNT)
    } else {
        return;
    };

    let _ = st_size_class;

    if eob > eob_limit {
        return;
    }

    let stx_idx = square_tx_size.min(4);
    ec.trace_state(&format!(
        "before_stx idx={} cdf={:?}",
        stx_idx, &cdf.stx[stx_idx]
    ));
    let stx_flag = ec.decode_symbol_mut(&mut cdf.stx[stx_idx]);
    ec.trace_state(&format!(
        "after_stx_flag={} txs={}",
        stx_flag, square_tx_size
    ));

    if stx_flag > 0 {
        if primary_tx_type == ADST_ADST && width >= 8 && height >= 8 {
            let _stx_set = ec.decode_symbol_mut(&mut cdf.most_probable_stx_set_adst_adst);
            ec.trace_state(&format!("after_stx_set_adst={}", _stx_set));
        } else {
            let _stx_set = ec.decode_symbol_mut(&mut cdf.most_probable_stx_set);
            ec.trace_state(&format!("after_stx_set={}", _stx_set));
        }
    }
}

fn get_padded_idx_left(idx: usize, bwl: u32, top_off: usize) -> usize {
    top_off + TX_PAD_LEFT + idx + ((idx >> bwl) << TX_PAD_HOR_LOG2)
}

fn get_upper_levels_ctx_2d(levels: &[u8], coeff_idx: usize, bwl: u32, top_off: usize) -> usize {
    let pos = get_padded_idx_left(coeff_idx, bwl, top_off);
    let stride = (1usize << bwl) + TX_PAD_LEFT;
    let left = (levels[pos - 1] as usize).min(3);
    let above = (levels[pos - stride] as usize).min(3);
    (left + above).min(6)
}

fn get_br_ctx_skip(levels: &[u8], coeff_idx: usize, bwl: u32, top_off: usize) -> usize {
    let row = coeff_idx >> bwl;
    let col = coeff_idx - (row << bwl);
    let stride = (1usize << bwl) + TX_PAD_LEFT;
    let pos = top_off + row * stride + col + TX_PAD_LEFT;
    let left = (levels[pos - 1] as usize).min(MAX_VAL_BR_CTX as usize);
    let above = (levels[pos - stride] as usize).min(MAX_VAL_BR_CTX as usize);
    (left + above).min(6)
}

fn get_sign_skip(signs: &[i8], levels: &[u8], pos: usize, stride: usize) -> usize {
    let mut signc: i32 = 0;
    if levels[pos - 1] != 0 {
        signc += signs[pos - 1] as i32;
    }
    if levels[pos - stride] != 0 {
        signc += signs[pos - stride] as i32;
    }
    if levels[pos - stride - 1] != 0 {
        signc += signs[pos - stride - 1] as i32;
    }
    if signc > 2 {
        return 5;
    }
    if signc < -2 {
        return 6;
    }
    if signc > 0 {
        return 1;
    }
    if signc < 0 {
        return 2;
    }
    0
}

fn get_sign_ctx_skip(
    signs: &[i8],
    levels: &[u8],
    coeff_idx: usize,
    bwl: u32,
    top_off: usize,
) -> usize {
    let pos = get_padded_idx_left(coeff_idx, bwl, top_off);
    let stride = (1usize << bwl) + TX_PAD_LEFT;
    let sign_ctx = get_sign_skip(signs, levels, pos, stride);
    if levels[pos] > COEFF_BASE_RANGE as u8 && sign_ctx != 0 {
        sign_ctx + 2
    } else {
        sign_ctx
    }
}

fn get_lower_levels_ctx_bob(bwl: u32, height: usize, scan_idx: usize) -> usize {
    let total = height << bwl;
    if scan_idx <= total / 8 {
        return 0;
    }
    if scan_idx <= total / 4 {
        return 1;
    }
    2
}

fn decode_low_range_fsc(ec: &mut DaalaDecoder, cdf: &mut Vec<u16>) -> usize {
    let br_sym = ec.decode_symbol_mut(cdf);
    NUM_BASE_LEVELS + 1 + br_sym
}

fn decode_idtx_skip_coeffs(
    ec: &mut DaalaDecoder,
    tx_size: TxSize,
    eob: u16,
    cdf: &mut FrameCdfContext,
) -> (Vec<i32>, u16) {
    let bwl = tx_size.bwl();
    let height = tx_size.coded_height();
    let txs_idx = tx_size.txs_ctx();
    let size_ctx = txs_idx.min(2);
    let scan = get_scan_for_tx(tx_size, IDTX);
    let max_eob = tx_size.num_coeffs();
    let eob_clamped = eob.min(max_eob as u16).max(1);
    let bob = max_eob - eob_clamped as usize;

    let stride = (1usize << bwl) + TX_PAD_LEFT;
    let buf_height = height + TX_PAD_TOP + TX_PAD_LEFT;
    let top_off = TX_PAD_TOP * stride;
    let mut levels = vec![0u8; stride * buf_height];
    let mut signs = vec![0i8; stride * buf_height];
    let mut abs_levels = vec![0usize; max_eob];

    let bob_pos = scan[bob] as usize;
    let bob_ctx = get_lower_levels_ctx_bob(bwl, height, bob);
    let bob_sym = ec.decode_symbol_mut(&mut cdf.coeff_base_bob[size_ctx][bob_ctx]);
    let mut bob_level = bob_sym + 1;
    if bob_level > NUM_BASE_LEVELS {
        let br_ctx = get_br_ctx_skip(&levels, bob_pos, bwl, top_off);
        bob_level = decode_low_range_fsc(ec, &mut cdf.coeff_br_idtx[size_ctx][br_ctx]);
    }
    abs_levels[bob] = bob_level;
    let padded_bob = get_padded_idx_left(bob_pos, bwl, top_off);
    levels[padded_bob] = bob_level.min(127) as u8;

    for c in (bob + 1)..max_eob {
        let pos = scan[c] as usize;
        let ctx = get_upper_levels_ctx_2d(&levels, pos, bwl, top_off);
        let sym = ec.decode_symbol_mut(&mut cdf.coeff_base_idtx[size_ctx][ctx]);
        if sym > NUM_BASE_LEVELS {
            let br_ctx = get_br_ctx_skip(&levels, pos, bwl, top_off);
            abs_levels[c] = decode_low_range_fsc(ec, &mut cdf.coeff_br_idtx[size_ctx][br_ctx]);
        } else {
            abs_levels[c] = sym;
        }
        let padded = get_padded_idx_left(pos, bwl, top_off);
        levels[padded] = abs_levels[c].min(127) as u8;
    }

    let mut final_levels = vec![0i32; max_eob];
    let mut hr_avg = 0usize;
    for c in bob..max_eob {
        let pos = scan[c] as usize;
        let abs_level = abs_levels[c];
        if abs_level == 0 {
            continue;
        }
        let sign_ctx = get_sign_ctx_skip(&signs, &levels, pos, bwl, top_off);
        let sign = ec.decode_symbol_mut(&mut cdf.idtx_sign[size_ctx][sign_ctx]);
        let padded = get_padded_idx_left(pos, bwl, top_off);
        signs[padded] = if sign > 0 { -1 } else { 1 };

        let final_level = if abs_level >= MAX_BASE_BR_RANGE {
            let hr = decode_adaptive_hr(ec, hr_avg);
            hr_avg = (hr_avg + hr) >> 1;
            abs_level + hr
        } else {
            abs_level
        };

        final_levels[c] = if sign == 1 {
            -(final_level as i32)
        } else {
            final_level as i32
        };
    }

    (final_levels, max_eob as u16)
}

fn decode_coeff_tu_fsc(
    ec: &mut DaalaDecoder,
    tx_size: TxSize,
    cfg: &TileDecodeConfig,
    cdf: &mut FrameCdfContext,
) -> TuData {
    let bwl = tx_size.bwl();
    let width = tx_size.coded_width();
    let height = tx_size.coded_height();
    let num_coeffs = tx_size.num_coeffs();
    let txs_idx = tx_size.txs_ctx();

    let effective_ctx = 9usize;
    ec.trace_state(&format!(
        "before_txb_skip_y txs_ctx={} skip_ctx={}",
        txs_idx, effective_ctx
    ));
    eprintln!("[CDF_DBG] txb_skip_y[{}][{}] fsc=1", txs_idx, effective_ctx);

    let skip = ec.decode_symbol_mut(&mut cdf.txb_skip_y_fsc[txs_idx][effective_ctx]);
    ec.trace_state(&format!("after_txb_skip p=0 skip={}", skip));

    if skip == 1 {
        return TuData {
            levels: vec![0; num_coeffs],
            eob: 0,
            tx_size,
            tx_type: IDTX,
            row_offset: 0,
            col_offset: 0,
            cctx_type: 0,
        };
    }

    let eob = decode_eob_var(ec, tx_size, 0, cdf);
    ec.trace_state(&format!("after_eob_y eob={} txs={}", eob, txs_idx));

    ec.trace_state(&format!("after_tx_type_y type={}", IDTX));

    let _ = bwl;
    let _ = width;
    let _ = height;
    let _ = txs_idx;
    let _ = cfg;

    let (final_levels, final_eob) = decode_idtx_skip_coeffs(ec, tx_size, eob, cdf);

    TuData {
        levels: final_levels,
        eob: final_eob,
        tx_size,
        tx_type: IDTX,
        row_offset: 0,
        col_offset: 0,
        cctx_type: 0,
    }
}

fn decode_coeff_tu(
    ec: &mut DaalaDecoder,
    plane: usize,
    tx_size: TxSize,
    dc_sign_ctx: usize,
    entropy_ctx: usize,
    u_has_coeffs: bool,
    inter_block: bool,
    pred_mode: u8,
    fsc_mode: usize,
    cfg: &TileDecodeConfig,
    cdf: &mut FrameCdfContext,
) -> TuData {
    if plane == 0 && fsc_mode != 0 {
        return decode_coeff_tu_fsc(ec, tx_size, cfg, cdf);
    }

    let bwl = tx_size.bwl();
    let width = tx_size.coded_width();
    let height = tx_size.coded_height();
    let num_coeffs = tx_size.num_coeffs();
    let txs_idx = tx_size.txs_ctx();

    let plane_name = match plane {
        0 => "y",
        1 => "u",
        _ => "v",
    };
    let effective_ctx = if plane == 0 && fsc_mode != 0 {
        9
    } else {
        entropy_ctx
    };
    ec.trace_state(&format!(
        "before_txb_skip_{} txs_ctx={} skip_ctx={}",
        plane_name, txs_idx, effective_ctx
    ));

    if plane == 0 {
        eprintln!(
            "[CDF_DBG] txb_skip_y[{}][{}] fsc={}",
            txs_idx, effective_ctx, fsc_mode
        );
    }
    let skip = match plane {
        0 => {
            if fsc_mode != 0 || inter_block {
                ec.decode_symbol_mut(&mut cdf.txb_skip_y_fsc[txs_idx][effective_ctx])
            } else {
                ec.decode_symbol_mut(&mut cdf.txb_skip_y[txs_idx][effective_ctx])
            }
        }
        1 => {
            if inter_block {
                ec.decode_symbol_mut(&mut cdf.txb_skip_y_fsc[txs_idx][entropy_ctx])
            } else {
                ec.decode_symbol_mut(&mut cdf.txb_skip_y[txs_idx][entropy_ctx])
            }
        }
        _ => {
            if u_has_coeffs {
                ec.decode_symbol_mut(&mut cdf.txb_skip_v_u_nz[entropy_ctx])
            } else {
                ec.decode_symbol_mut(&mut cdf.txb_skip_v[entropy_ctx])
            }
        }
    };
    ec.trace_state(&format!("after_txb_skip p={} skip={}", plane, skip));

    if skip == 1 {
        return TuData {
            levels: vec![0; num_coeffs],
            eob: 0,
            tx_size,
            tx_type: 0,
            row_offset: 0,
            col_offset: 0,
            cctx_type: 0,
        };
    }

    let pl_ctx = if plane == 0 {
        if inter_block { 1 } else { 0 }
    } else {
        2
    };
    let mut eob = decode_eob_var(ec, tx_size, pl_ctx, cdf);
    ec.trace_state(&format!(
        "after_eob_{} eob={} txs={}",
        plane_name, eob, txs_idx
    ));

    let tx_type = if plane == 0 && cfg.coded_lossless {
        DCT_DCT
    } else if plane == 0 {
        read_tx_type(
            ec,
            tx_size,
            pred_mode,
            fsc_mode,
            inter_block,
            cfg.reduced_tx_set_used,
            eob,
            cdf,
        )
    } else if cfg.coded_lossless {
        DCT_DCT
    } else {
        get_chroma_tx_type(tx_size, pred_mode, inter_block, cfg.reduced_tx_set_used)
    };
    ec.trace_state(&format!("after_tx_type_{} type={}", plane_name, tx_type));

    if plane == 0 && inter_block && cfg.enable_fsc && tx_type == IDTX {
        let (final_levels, final_eob) = decode_idtx_skip_coeffs(ec, tx_size, eob, cdf);
        return TuData {
            levels: final_levels,
            eob: final_eob,
            tx_size,
            tx_type,
            row_offset: 0,
            col_offset: 0,
            cctx_type: 0,
        };
    }

    let tx_class = get_tx_class(tx_type);
    let scan = get_scan_for_tx(tx_size, tx_type);
    let max_eob = scan.len() as u16;
    if eob > max_eob {
        ec.trace_state(&format!(
            "clamp_eob p={} tx={:?} tx_type={} eob={} max={}",
            plane, tx_size, tx_type, eob, max_eob
        ));
        eob = max_eob;
    }

    let mut cctx_type_val: u8 = 0;
    if plane == 1 && cfg.enable_cctx && eob > 1 {
        cctx_type_val = ec.decode_symbol_mut(&mut cdf.cctx_type) as u8;
        ec.trace_state(&format!("after_cctx_type={}", cctx_type_val));
    }

    let padded_stride = width + TX_PAD_HOR;
    let padded_height = height + 4;
    if plane == 0 && cfg.enable_ist && eob > 1 && fsc_mode == 0 && !inter_block {
        read_sec_tx_type(ec, tx_size, tx_type, pred_mode, eob as usize, cdf);
    }

    let tcq_active = cfg.tcq_mode && plane == 0 && tx_class == TxClass::TwoD;
    let mut tcq_state = tcq_init_state(tcq_active);

    let mut levels_buf = vec![0u8; padded_stride * padded_height];
    let mut abs_levels = vec![0usize; eob as usize];

    let eob_scan_idx = (eob - 1) as usize;
    let eob_pos = scan[eob_scan_idx] as usize;
    let eob_row = eob_pos >> bwl;
    let eob_col = eob_pos - (eob_row << bwl);
    let eob_lf = is_lf(eob_row, eob_col, tx_class, plane);

    let coeff_ctx = get_lower_levels_ctx_eob(bwl, height, eob_scan_idx);

    if plane == 0 {
        if eob_lf {
            let sym = ec.decode_symbol_mut(&mut cdf.coeff_base_lf_eob_y[txs_idx][coeff_ctx]);
            let base_value = sym + 1;
            if base_value > LF_NUM_BASE_LEVELS {
                let br_ctx = get_br_ctx_lf_eob(eob_pos, tx_class);
                abs_levels[eob_scan_idx] = decode_low_range(
                    ec,
                    LF_NUM_BASE_LEVELS,
                    &COEFF_BR_LF_Y_ICDF[br_ctx],
                    Some(&mut cdf.coeff_br_lf_y[br_ctx]),
                );
            } else {
                abs_levels[eob_scan_idx] = base_value;
            }
        } else {
            let sym = ec.decode_symbol_mut(&mut cdf.coeff_base_eob_y[txs_idx][coeff_ctx]);
            let base_value = sym + 1;
            if base_value > NUM_BASE_LEVELS {
                abs_levels[eob_scan_idx] = decode_low_range(
                    ec,
                    NUM_BASE_LEVELS,
                    &COEFF_BR_Y_ICDF[0],
                    Some(&mut cdf.coeff_br_y[0]),
                );
            } else {
                abs_levels[eob_scan_idx] = base_value;
            }
        }
    } else if eob_lf {
        let sym = ec.decode_symbol_mut(&mut cdf.coeff_base_lf_eob_uv[coeff_ctx]);
        abs_levels[eob_scan_idx] = sym + 1;
    } else {
        let sym = ec.decode_symbol_mut(&mut cdf.coeff_base_eob_uv[coeff_ctx]);
        let base_value = sym + 1;
        if base_value > NUM_BASE_LEVELS {
            abs_levels[eob_scan_idx] = decode_low_range(
                ec,
                NUM_BASE_LEVELS,
                &COEFF_BR_UV_ICDF[0],
                Some(&mut cdf.coeff_br_uv[0]),
            );
        } else {
            abs_levels[eob_scan_idx] = base_value;
        }
    }

    levels_buf[get_padded_idx(eob_pos, bwl)] =
        stored_level(abs_levels[eob_scan_idx], eob_lf, plane);
    tcq_state = tcq_next_state(tcq_state, abs_levels[eob_scan_idx]);

    if plane == 0 {
        ec.trace_state(&format!(
            "after_eob_pos_coeff eob={} lvl={} lf={} pos={}",
            eob, abs_levels[eob_scan_idx], eob_lf, eob_pos
        ));
    }
    if plane > 0 {
        ec.trace_state(&format!(
            "COEFF_UV si={} pos={} row={} col={} lf={} p={} ctx={} level={}",
            eob_scan_idx,
            eob_pos,
            eob_row,
            eob_col,
            eob_lf as u8,
            plane,
            coeff_ctx,
            abs_levels[eob_scan_idx]
        ));
    }

    let mut is_hidden = false;
    if eob > 1 {
        for c in (1..eob_scan_idx).rev() {
            let pos = scan[c] as usize;
            let row = pos >> bwl;
            let col = pos - (row << bwl);
            let lf = is_lf(row, col, tx_class, plane);
            let q_i = tcq_quant(tcq_state);

            if plane == 0 {
                if lf {
                    let lctx = get_lower_levels_ctx_lf(&levels_buf, pos, bwl, tx_class);
                    let sym = ec.decode_symbol_mut(&mut cdf.coeff_base_lf_y[txs_idx][lctx][q_i]);
                    if sym > LF_NUM_BASE_LEVELS {
                        let br_ctx = get_br_lf_ctx(&levels_buf, pos, bwl, tx_class);
                        abs_levels[c] = decode_low_range(
                            ec,
                            LF_NUM_BASE_LEVELS,
                            &COEFF_BR_LF_Y_ICDF[br_ctx],
                            Some(&mut cdf.coeff_br_lf_y[br_ctx]),
                        );
                    } else {
                        abs_levels[c] = sym;
                    }
                } else {
                    let lctx = get_lower_levels_ctx(&levels_buf, pos, bwl, tx_class, plane);
                    let sym = ec.decode_symbol_mut(&mut cdf.coeff_base_y[txs_idx][lctx][q_i]);
                    if sym > NUM_BASE_LEVELS {
                        let br_ctx = get_br_ctx(&levels_buf, pos, bwl, tx_class);
                        abs_levels[c] = decode_low_range(
                            ec,
                            NUM_BASE_LEVELS,
                            &COEFF_BR_Y_ICDF[br_ctx],
                            Some(&mut cdf.coeff_br_y[br_ctx]),
                        );
                    } else {
                        abs_levels[c] = sym;
                    }
                }
            } else if lf {
                let lctx = get_lower_levels_ctx_lf_chroma(&levels_buf, pos, bwl, tx_class, plane);
                let sym = ec.decode_symbol_mut(&mut cdf.coeff_base_lf_uv[lctx][q_i]);
                abs_levels[c] = sym;
            } else {
                let lctx = get_lower_levels_ctx_chroma(&levels_buf, pos, bwl, tx_class, plane);
                let sym = ec.decode_symbol_mut(&mut cdf.coeff_base_uv[lctx][q_i]);
                if sym > NUM_BASE_LEVELS {
                    let br_ctx = get_br_ctx_chroma(&levels_buf, pos, bwl, tx_class);
                    abs_levels[c] = decode_low_range(
                        ec,
                        NUM_BASE_LEVELS,
                        &COEFF_BR_UV_ICDF[br_ctx],
                        Some(&mut cdf.coeff_br_uv[br_ctx]),
                    );
                } else {
                    abs_levels[c] = sym;
                }
            }

            levels_buf[get_padded_idx(pos, bwl)] = stored_level(abs_levels[c], lf, plane);
            tcq_state = tcq_next_state(tcq_state, abs_levels[c]);

            if plane == 0 && eob <= 10 {
                ec.trace_state(&format!(
                    "after_scan_c={} pos={} lf={} lvl={} q_i={}",
                    c, pos, lf, abs_levels[c], q_i
                ));
            }
            if plane > 0 {
                let lctx_val = if lf {
                    get_lower_levels_ctx_lf_chroma(&levels_buf, pos, bwl, tx_class, plane)
                } else {
                    get_lower_levels_ctx_chroma(&levels_buf, pos, bwl, tx_class, plane)
                };
                let br_ctx_val: i32 = if !lf && abs_levels[c] > NUM_BASE_LEVELS {
                    get_br_ctx_chroma(&levels_buf, pos, bwl, tx_class) as i32
                } else {
                    -1
                };
                ec.trace_state(&format!("COEFF_UV si={} pos={} row={} col={} lf={} p={} ctx={} q_i={} level={} br_ctx={}", c, pos, row, col, lf as u8, plane, lctx_val, q_i, abs_levels[c], br_ctx_val));
            }
        }

        let dc_pos = scan[0] as usize;
        let dc_row = dc_pos >> bwl;
        let dc_col = dc_pos - (dc_row << bwl);
        let dc_lf = is_lf(dc_row, dc_col, tx_class, plane);
        let dc_q_i = tcq_quant(tcq_state);

        let enable_parity_hiding = plane == 0
            && cfg.allow_parity_hiding
            && !cfg.tcq_mode
            && parity_hiding_allowed_tx_type(tx_type)
            && (eob as usize) > PHTHRESH;
        let mut sum_abs1 = 0usize;
        if enable_parity_hiding {
            let mut num_nz = 0usize;
            for c in 1..(eob as usize) {
                let level = abs_levels[c].min(MAX_BASE_BR_RANGE);
                if level != 0 {
                    num_nz += 1;
                    sum_abs1 += level;
                }
            }
            is_hidden = num_nz >= PHTHRESH;
        }

        if plane == 0 && is_hidden {
            let ctx_idx = get_base_ctx_ph(&levels_buf, dc_pos, bwl, tx_class);
            let q_index = ec.decode_symbol_mut(&mut cdf.coeff_base_ph[ctx_idx]);
            abs_levels[0] = (q_index << 1) + (sum_abs1 & 1);
            if eob <= 32 {
                ec.trace_state(&format!(
                    "after_hidden_dc q={} parity={} ctx={} level={}",
                    q_index,
                    sum_abs1 & 1,
                    ctx_idx,
                    abs_levels[0],
                ));
            }
        } else if plane == 0 {
            if dc_lf {
                let lctx = get_lower_levels_ctx_lf(&levels_buf, dc_pos, bwl, tx_class);
                let sym = ec.decode_symbol_mut(&mut cdf.coeff_base_lf_y[txs_idx][lctx][dc_q_i]);
                if sym > LF_NUM_BASE_LEVELS {
                    let br_ctx = if tx_class == TxClass::TwoD && dc_pos == 0 {
                        get_br_lf_ctx_dc_2d(&levels_buf, dc_pos, bwl)
                    } else {
                        get_br_lf_ctx(&levels_buf, dc_pos, bwl, tx_class)
                    };
                    abs_levels[0] = decode_low_range(
                        ec,
                        LF_NUM_BASE_LEVELS,
                        &COEFF_BR_LF_Y_ICDF[br_ctx],
                        Some(&mut cdf.coeff_br_lf_y[br_ctx]),
                    );
                } else {
                    abs_levels[0] = sym;
                }
            } else {
                let lctx = get_lower_levels_ctx(&levels_buf, dc_pos, bwl, tx_class, plane);
                let sym = ec.decode_symbol_mut(&mut cdf.coeff_base_y[txs_idx][lctx][dc_q_i]);
                if sym > NUM_BASE_LEVELS {
                    let br_ctx = get_br_ctx(&levels_buf, dc_pos, bwl, tx_class);
                    abs_levels[0] = decode_low_range(
                        ec,
                        NUM_BASE_LEVELS,
                        &COEFF_BR_Y_ICDF[br_ctx],
                        Some(&mut cdf.coeff_br_y[br_ctx]),
                    );
                } else {
                    abs_levels[0] = sym;
                }
            }
        } else if dc_lf {
            let lctx = get_lower_levels_ctx_lf_chroma(&levels_buf, dc_pos, bwl, tx_class, plane);
            let sym = ec.decode_symbol_mut(&mut cdf.coeff_base_lf_uv[lctx][dc_q_i]);
            abs_levels[0] = sym;
        } else {
            let lctx = get_lower_levels_ctx_chroma(&levels_buf, dc_pos, bwl, tx_class, plane);
            let sym = ec.decode_symbol_mut(&mut cdf.coeff_base_uv[lctx][dc_q_i]);
            if sym > NUM_BASE_LEVELS {
                let br_ctx = get_br_ctx_chroma(&levels_buf, dc_pos, bwl, tx_class);
                abs_levels[0] = decode_low_range(
                    ec,
                    NUM_BASE_LEVELS,
                    &COEFF_BR_UV_ICDF[br_ctx],
                    Some(&mut cdf.coeff_br_uv[br_ctx]),
                );
            } else {
                abs_levels[0] = sym;
            }
        }

        levels_buf[get_padded_idx(dc_pos, bwl)] = stored_level(abs_levels[0], dc_lf, plane);

        if plane == 0 && eob <= 10 {
            ec.trace_state(&format!(
                "after_dc_coeff lvl={} lf={} q_i={}",
                abs_levels[0], dc_lf, dc_q_i
            ));
        }
    }

    if plane == 0 {
        ec.trace_state(&format!(
            "after_all_levels eob={} levels={:?}",
            eob,
            &abs_levels[..eob as usize]
        ));
    }

    let mut levels = vec![0i32; num_coeffs];
    let mut hr_avg = 0usize;
    for c in (0..eob as usize).rev() {
        let pos = scan[c] as usize;
        let abs_level = abs_levels[c];
        if abs_level == 0 {
            continue;
        }

        let row = pos >> bwl;
        let col = pos - (row << bwl);
        let is_dc = is_dc_coeff(c, row, col, tx_class);

        let sign = if is_dc {
            if plane == 0 {
                if is_hidden {
                    ec.decode_symbol(&DC_SIGN_UV_ICDF)
                } else {
                    let ctx = if c == 0 { dc_sign_ctx } else { 0 };
                    ec.decode_symbol_mut(&mut cdf.dc_sign_y[ctx])
                }
            } else {
                ec.decode_literal(1) as usize
            }
        } else {
            ec.decode_literal(1) as usize
        };

        let lf = is_lf(row, col, tx_class, plane);
        let max_br = if lf {
            if plane == 0 {
                LF_MAX_BASE_BR_RANGE
            } else {
                LF_NUM_BASE_LEVELS + 1
            }
        } else {
            MAX_BASE_BR_RANGE
        };

        let final_level = if abs_level >= max_br {
            let hr = decode_adaptive_hr(ec, hr_avg);
            hr_avg = (hr_avg + hr) >> 1;
            abs_level + hr
        } else {
            abs_level
        };

        if plane > 0 {
            ec.trace_state(&format!(
                "SIGN_HR si={} pos={} sign={} final={} is_dc={} abs={} max_br={} hr_avg={}",
                c, pos, sign, final_level, is_dc as u8, abs_level, max_br, hr_avg
            ));
        }

        levels[c] = if sign == 1 {
            -(final_level as i32)
        } else {
            final_level as i32
        };
    }

    TuData {
        levels,
        eob,
        tx_size,
        tx_type,
        row_offset: 0,
        col_offset: 0,
        cctx_type: cctx_type_val,
    }
}

fn decode_block_tus(
    ec: &mut DaalaDecoder,
    plane: usize,
    block_x: usize,
    block_y: usize,
    block_w: usize,
    block_h: usize,
    u_has_coeffs: bool,
    inter_block: bool,
    pred_mode: u8,
    fsc_mode: usize,
    cfg: &TileDecodeConfig,
    cdf: &mut FrameCdfContext,
    state: &mut TileState,
) -> Vec<TuData> {
    let tx_partition = if cfg.tx_mode_select && !(block_w == 4 && block_h == 4) && plane == 0 {
        read_tx_partition(ec, block_w, block_h, fsc_mode, cfg.reduced_tx_part_set, cdf)
    } else {
        TxPartitionType::None
    };

    let max_tx_w = block_w.min(64);
    let max_tx_h = block_h.min(64);
    ec.trace_state(&format!(
        "after_tx_part p={} txp={:?} blk=({},{}) {}x{}",
        plane, tx_partition, block_x, block_y, block_w, block_h
    ));
    let sub_tus = if cfg.coded_lossless {
        let mut tus = Vec::new();
        let mut row_off = 0;
        while row_off < block_h {
            let mut col_off = 0;
            while col_off < block_w {
                tus.push((TxSize::Tx4x4, row_off, col_off));
                col_off += 4;
            }
            row_off += 4;
        }
        tus
    } else {
        get_tx_partition_sub_tus(tx_partition, max_tx_w, max_tx_h)
    };

    let (ctx_plane, mi_scale) = if plane == 0 { (0, 4) } else { (plane, 8) };
    let base_mi_x = block_x / mi_scale;
    let base_mi_y = (block_y - state.sb_row_y) / mi_scale;

    let mut tus = Vec::with_capacity(sub_tus.len());
    for (sub_tx_size, row_off, col_off) in &sub_tus {
        let tx_w = sub_tx_size.width();
        let tx_h = sub_tx_size.height();
        let tx_w_units = tx_w / 4;
        let tx_h_units = tx_h / 4;

        let tu_mi_x = base_mi_x + col_off / 4;
        let tu_mi_y = base_mi_y + row_off / 4;

        let above = &state.above_ctx[ctx_plane][tu_mi_x..tu_mi_x + tx_w_units];
        let left = &state.left_ctx[ctx_plane][tu_mi_y..tu_mi_y + tx_h_units];

        let (txb_skip_ctx, dc_sign_ctx) = if plane == 0 && fsc_mode != 0 {
            (9, 0)
        } else if plane == 0 {
            let skip_ctx = get_txb_skip_ctx_y(above, left, block_w, block_h, tx_w, tx_h);
            let dc_ctx = compute_dc_sign_ctx(above, left);
            (skip_ctx, dc_ctx)
        } else if plane == 1 {
            let ctx_base = compute_entropy_ctx(above, left);
            let dc_ctx = compute_dc_sign_ctx(above, left);
            (ctx_base + 6, dc_ctx)
        } else {
            let ctx_base = compute_entropy_ctx(above, left);
            let dc_ctx = compute_dc_sign_ctx(above, left);
            let block_pels_log2 = ((block_w * block_h) as f64).log2() as usize;
            let tx_pels_log2 = ((tx_w * tx_h) as f64).log2() as usize;
            let v_offset = if block_pels_log2 > tx_pels_log2 { 3 } else { 0 };
            (ctx_base + v_offset, dc_ctx)
        };

        let mut tu = decode_coeff_tu(
            ec,
            plane,
            *sub_tx_size,
            dc_sign_ctx,
            txb_skip_ctx,
            u_has_coeffs,
            inter_block,
            pred_mode,
            fsc_mode,
            cfg,
            cdf,
        );
        tu.row_offset = *row_off;
        tu.col_offset = *col_off;
        ec.trace_state(&format!(
            "after_tu p={} tx={:?} eob={} skip_ctx={} blk=({},{})",
            plane, sub_tx_size, tu.eob, txb_skip_ctx, block_x, block_y
        ));

        let cul = compute_cul_level(&tu.levels, tu.eob);
        for i in 0..tx_w_units {
            state.above_ctx[ctx_plane][tu_mi_x + i] = cul;
        }
        for i in 0..tx_h_units {
            state.left_ctx[ctx_plane][tu_mi_y + i] = cul;
        }

        tus.push(tu);
    }
    tus
}

fn read_y_mode(
    ec: &mut DaalaDecoder,
    y_mode_ctx: usize,
    lossless: bool,
    cdf: &mut FrameCdfContext,
) -> (usize, Option<u8>) {
    if lossless {
        let use_dpcm = ec.decode_symbol_mut(&mut cdf.dpcm_y);
        if use_dpcm == 1 {
            let direction = ec.decode_symbol_mut(&mut cdf.dpcm_vert_horz);
            let joint = if direction == 0 { 22 } else { 50 };
            return (0, Some(joint));
        }
    }
    ec.trace_state("before_mode_set");
    let mode_set = ec.decode_symbol_mut(&mut cdf.y_mode_set);
    ec.trace_state(&format!("after_mode_set={}", mode_set));
    if mode_set == 0 {
        let mut mode_idx = ec.decode_symbol_mut(&mut cdf.y_mode_idx[y_mode_ctx]);
        ec.trace_state(&format!("after_mode_idx={} ctx={}", mode_idx, y_mode_ctx));
        if mode_idx == 7 {
            let offset = ec.decode_symbol_mut(&mut cdf.y_mode_idx_offset[y_mode_ctx]);
            ec.trace_state(&format!("after_mode_offset={}", offset));
            mode_idx += offset;
        }
        (mode_idx as usize, None)
    } else {
        let literal = ec.decode_literal(4) as usize;
        (
            FIRST_MODE_COUNT + (mode_set - 1) * SECOND_MODE_COUNT + literal,
            None,
        )
    }
}

fn is_cfl_allowed(block_w: usize, block_h: usize) -> bool {
    let uv_w = block_w / 2;
    let uv_h = block_h / 2;
    uv_w >= 4 && uv_h >= 4 && block_w <= 128 && block_h <= 128
}

fn is_mhccp_allowed(enable_mhccp: bool, block_w: usize, block_h: usize) -> bool {
    if !enable_mhccp {
        return false;
    }
    let uv_w = block_w / 2;
    let uv_h = block_h / 2;
    if uv_w == 4 && uv_h == 4 {
        return false;
    }
    if block_w > 64 || block_h > 64 {
        return false;
    }
    true
}

fn mhccp_size_group(block_w: usize, block_h: usize) -> usize {
    let mi_w_log2 = (block_w / 4).trailing_zeros() as usize;
    let mi_h_log2 = (block_h / 4).trailing_zeros() as usize;
    mi_w_log2.min(mi_h_log2).min(3)
}

fn read_uv_mode(
    ec: &mut DaalaDecoder,
    y_mode: u8,
    lossless: bool,
    block_w: usize,
    block_h: usize,
    cfl_ctx: usize,
    enable_cfl_intra: bool,
    enable_mhccp: bool,
    cfl_allowed_in_sdp: bool,
    cdf: &mut FrameCdfContext,
) -> (u8, i8, i8, u8, u8) {
    if lossless {
        let use_dpcm = ec.decode_symbol_mut(&mut cdf.dpcm_uv);
        if use_dpcm == 1 {
            let direction = ec.decode_symbol_mut(&mut cdf.dpcm_uv_vert_horz);
            let uv_mode = if direction == 0 { 1 } else { 2 };
            return (uv_mode, 0, 0, 0, 0);
        }
    }
    let lossless_allows_cfl = !lossless || (block_w == 4 && block_h == 4);
    let cfl_or_mhccp_allowed = lossless_allows_cfl
        && (enable_cfl_intra || enable_mhccp)
        && is_cfl_allowed(block_w, block_h);
    let mhccp_only_allowed = lossless_allows_cfl
        && !enable_cfl_intra
        && enable_mhccp
        && is_mhccp_allowed(enable_mhccp, block_w, block_h);
    if cfl_allowed_in_sdp && (cfl_or_mhccp_allowed || mhccp_only_allowed) {
        ec.trace_state(&format!("before_is_cfl ctx={}", cfl_ctx));
        let is_cfl = ec.decode_symbol_mut(&mut cdf.cfl[cfl_ctx]);
        ec.trace_state(&format!("after_is_cfl={}", is_cfl));
        if is_cfl == 1 {
            let mut cfl_idx = 0u8;
            let mut mh_dir = 0u8;
            let mhccp_allowed =
                lossless_allows_cfl && is_mhccp_allowed(enable_mhccp, block_w, block_h);

            if mhccp_allowed {
                let cfl_mhccp_switch = if enable_cfl_intra {
                    let s = ec.decode_symbol_mut(&mut cdf.cfl_mhccp);
                    ec.trace_state(&format!("after_cfl_mhccp_switch={}", s));
                    s
                } else {
                    1
                };
                if cfl_mhccp_switch != 0 {
                    cfl_idx = 2;
                    let sg = mhccp_size_group(block_w, block_h);
                    mh_dir = ec.decode_symbol_mut(&mut cdf.mhccp_filter_dir[sg]) as u8;
                    ec.trace_state(&format!("after_mhccp_filter_dir={} sg={}", mh_dir, sg));
                } else {
                    cfl_idx = ec.decode_symbol_mut(&mut cdf.cfl_index) as u8;
                    ec.trace_state(&format!("after_cfl_index={}", cfl_idx));
                }
            } else if enable_cfl_intra {
                cfl_idx = ec.decode_symbol_mut(&mut cdf.cfl_index) as u8;
                ec.trace_state(&format!("after_cfl_index={}", cfl_idx));
            }

            if cfl_idx != 0 {
                return (13, 0, 0, cfl_idx, mh_dir);
            }

            let joint_sign = ec.decode_symbol_mut(&mut cdf.cfl_sign);
            ec.trace_state(&format!("after_cfl_sign={}", joint_sign));
            let u_sign = (joint_sign + 1) / CFL_SIGNS;
            let v_sign = (joint_sign + 1) % CFL_SIGNS;
            let mut alpha_u_raw: i8 = 0;
            let mut alpha_v_raw: i8 = 0;
            if u_sign != 0 {
                let alpha_u_ctx = joint_sign - 2;
                let alpha_u_idx = ec.decode_symbol_mut(&mut cdf.cfl_alpha[alpha_u_ctx]);
                ec.trace_state(&format!(
                    "after_cfl_alpha_u={} ctx={}",
                    alpha_u_idx, alpha_u_ctx
                ));
                let abs_val = (alpha_u_idx + 1) as i8;
                alpha_u_raw = if u_sign == 2 { abs_val } else { -abs_val };
            }
            if v_sign != 0 {
                let alpha_v_ctx = v_sign * CFL_SIGNS + u_sign - CFL_SIGNS;
                let alpha_v_idx = ec.decode_symbol_mut(&mut cdf.cfl_alpha[alpha_v_ctx]);
                ec.trace_state(&format!(
                    "after_cfl_alpha_v={} ctx={}",
                    alpha_v_idx, alpha_v_ctx
                ));
                let abs_val = (alpha_v_idx + 1) as i8;
                alpha_v_raw = if v_sign == 2 { abs_val } else { -abs_val };
            }
            return (13, alpha_u_raw, alpha_v_raw, cfl_idx, mh_dir);
        }
    }

    let uv_ctx = if is_directional_mode(y_mode) { 1 } else { 0 };
    ec.trace_state(&format!("before_uv_mode ctx={}", uv_ctx));
    let mut uv_mode_idx = ec.decode_symbol_mut(&mut cdf.uv_mode[uv_ctx]);
    if uv_mode_idx == 7 {
        uv_mode_idx += ec.decode_literal(3) as usize;
    }
    let uv_mode_list = build_uv_intra_mode_list(y_mode);
    let uv_mode = uv_mode_list[uv_mode_idx.min(12)];
    ec.trace_state(&format!(
        "after_uv_mode_idx={} mapped_uv_mode={}",
        uv_mode_idx, uv_mode
    ));
    (uv_mode, 0, 0, 0, 0)
}

const PALETTE_MAX_SIZE: usize = 8;

struct ModeInfoGrid {
    frame_w: usize,
    frame_h: usize,
    mi_cols: usize,
    mi_rows: usize,
    sb_size: usize,
    joint_y_mode: Vec<u8>,
    is_cfl: Vec<bool>,
    fsc_mode_y: Vec<u8>,
    palette_size_y: Vec<u8>,
    palette_colors_y: Vec<[u16; PALETTE_MAX_SIZE]>,
    mrl_index: Vec<u8>,
    multi_line_mrl: Vec<u8>,
    is_intrabc: Vec<bool>,
    skip_txfm: Vec<bool>,
    morph_pred: Vec<bool>,
    intrabc_bv_row: Vec<i32>,
    intrabc_bv_col: Vec<i32>,
    intrabc_refmv_bank: IntrabcRefMvBank,
}

impl ModeInfoGrid {
    fn new(frame_w: usize, frame_h: usize, sb_size: usize) -> Self {
        let mi_cols = (frame_w + 3) / 4;
        let mi_rows = (frame_h + 3) / 4;
        let n = mi_cols * mi_rows;
        Self {
            frame_w,
            frame_h,
            mi_cols,
            mi_rows,
            sb_size,
            joint_y_mode: vec![0; n],
            is_cfl: vec![false; n],
            fsc_mode_y: vec![0; n],
            palette_size_y: vec![0; n],
            palette_colors_y: vec![[0u16; PALETTE_MAX_SIZE]; n],
            mrl_index: vec![0; n],
            multi_line_mrl: vec![0; n],
            is_intrabc: vec![false; n],
            skip_txfm: vec![false; n],
            morph_pred: vec![false; n],
            intrabc_bv_row: vec![0; n],
            intrabc_bv_col: vec![0; n],
            intrabc_refmv_bank: IntrabcRefMvBank::new(),
        }
    }

    fn reset_intrabc_refmv_bank_for_row(&mut self) {
        self.intrabc_refmv_bank.reset_for_row();
    }

    fn reset_intrabc_refmv_bank_for_sb(&mut self) {
        self.intrabc_refmv_bank.reset_hits_for_sb();
    }

    fn fill(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        joint_mode: u8,
        cfl: bool,
        fsc: u8,
        mrl: u8,
        multi_mrl: u8,
    ) {
        let mi_x = x / 4;
        let mi_y = y / 4;
        let mi_w = w / 4;
        let mi_h = h / 4;
        if cfl {
            eprintln!(
                "[MI] fill is_cfl=true at ({},{}) {}x{} mi=({},{}) mi_size=({},{}) mi_cols={}",
                x, y, w, h, mi_x, mi_y, mi_w, mi_h, self.mi_cols
            );
        }
        for r in mi_y..(mi_y + mi_h).min(self.mi_rows) {
            for c in mi_x..(mi_x + mi_w).min(self.mi_cols) {
                let idx = r * self.mi_cols + c;
                self.joint_y_mode[idx] = joint_mode;
                self.is_cfl[idx] = cfl;
                self.fsc_mode_y[idx] = fsc;
                self.mrl_index[idx] = mrl;
                self.multi_line_mrl[idx] = multi_mrl;
            }
        }
    }

    fn fill_cfl(&mut self, x: usize, y: usize, w: usize, h: usize, cfl: bool) {
        let mi_x = x / 4;
        let mi_y = y / 4;
        let mi_w = w / 4;
        let mi_h = h / 4;
        for r in mi_y..(mi_y + mi_h).min(self.mi_rows) {
            for c in mi_x..(mi_x + mi_w).min(self.mi_cols) {
                let idx = r * self.mi_cols + c;
                self.is_cfl[idx] = cfl;
            }
        }
    }

    fn get_neighbor_joints(&self, x: usize, y: usize, w: usize, h: usize) -> (u8, u8) {
        let mi_x = x / 4;
        let mi_y = y / 4;
        let mi_w = w / 4;
        let mi_h = h / 4;

        let above_joint = if mi_y > 0 {
            let above_col = (mi_x + mi_w - 1).min(self.mi_cols - 1);
            self.joint_y_mode[(mi_y - 1) * self.mi_cols + above_col]
        } else {
            0
        };

        let left_joint = if mi_x > 0 {
            let left_row = (mi_y + mi_h - 1).min(self.mi_rows - 1);
            self.joint_y_mode[left_row * self.mi_cols + (mi_x - 1)]
        } else {
            0
        };

        (left_joint, above_joint)
    }

    fn fsc_mode_ctx(&self, x: usize, y: usize, w: usize, h: usize, sb_size: usize) -> usize {
        let mi_x = x / 4;
        let mi_y = y / 4;
        let mi_w = w / 4;
        let mi_h = h / 4;
        let mib_size = sb_size / 4;
        let at_sb_top = (mi_y % mib_size) == 0;

        let mut ctx = 0usize;
        let mut count = 0;

        let bottom_left = if mi_x > 0 {
            let r = (mi_y + mi_h - 1).min(self.mi_rows - 1);
            Some(self.fsc_mode_y[r * self.mi_cols + (mi_x - 1)] as usize)
        } else {
            None
        };
        if let Some(v) = bottom_left {
            ctx += v;
            count += 1;
        }

        if count < 2 && !at_sb_top && mi_y > 0 {
            let c = (mi_x + mi_w - 1).min(self.mi_cols - 1);
            let v = self.fsc_mode_y[(mi_y - 1) * self.mi_cols + c] as usize;
            ctx += v;
            count += 1;
        }

        if count < 2 && mi_x > 0 {
            let v = self.fsc_mode_y[mi_y * self.mi_cols + (mi_x - 1)] as usize;
            ctx += v;
            count += 1;
        }

        if count < 2 && !at_sb_top && mi_y > 0 {
            let v = self.fsc_mode_y[(mi_y - 1) * self.mi_cols + mi_x] as usize;
            ctx += v;
            // count += 1;
        }

        ctx
    }

    fn y_mode_ctx(&self, x: usize, y: usize, w: usize, h: usize) -> usize {
        let (left_joint, above_joint) = self.get_neighbor_joints(x, y, w, h);
        let is_above_dir = (above_joint as usize) >= NON_DIRECTIONAL_MODES_COUNT;
        let is_left_dir = (left_joint as usize) >= NON_DIRECTIONAL_MODES_COUNT;
        (is_above_dir as usize) + (is_left_dir as usize)
    }

    fn cfl_ctx(&self, x: usize, y: usize) -> usize {
        let mi_x = x / 4;
        let mi_y = y / 4;
        let mut cnt = 0;
        if mi_y > 0 {
            let above = (mi_y - 1) * self.mi_cols + mi_x;
            eprintln!(
                "[MI] cfl_ctx({},{}) mi=({},{}) above_idx={} is_cfl={}",
                x, y, mi_x, mi_y, above, self.is_cfl[above]
            );
            if self.is_cfl[above] {
                cnt += 1;
            }
        }
        if mi_x > 0 {
            let left = mi_y * self.mi_cols + (mi_x - 1);
            eprintln!(
                "[MI] cfl_ctx({},{}) mi=({},{}) left_idx={} is_cfl={}",
                x, y, mi_x, mi_y, left, self.is_cfl[left]
            );
            if self.is_cfl[left] {
                cnt += 1;
            }
        }
        eprintln!("[MI] cfl_ctx({},{}) => {}", x, y, cnt);
        cnt
    }

    fn mrl_index_ctx(&self, x: usize, y: usize, w: usize, h: usize, sb_size: usize) -> usize {
        self.gather_neighbor_indices(x, y, w, h, sb_size, false)
            .iter()
            .flatten()
            .filter(|&&idx| !self.is_intrabc[idx] && self.mrl_index[idx] != 0)
            .count()
    }

    fn multi_line_mrl_ctx(&self, x: usize, y: usize, w: usize, h: usize, sb_size: usize) -> usize {
        self.gather_neighbor_indices(x, y, w, h, sb_size, false)
            .iter()
            .flatten()
            .filter(|&&idx| !self.is_intrabc[idx] && self.multi_line_mrl[idx] != 0)
            .count()
    }

    fn fill_palette_y(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        n_colors: usize,
        colors: &[u16],
    ) {
        let mi_x = x / 4;
        let mi_y = y / 4;
        let mi_w = w / 4;
        let mi_h = h / 4;
        let mi_rows = self.palette_size_y.len() / self.mi_cols;
        for r in mi_y..(mi_y + mi_h).min(mi_rows) {
            for c in mi_x..(mi_x + mi_w).min(self.mi_cols) {
                let idx = r * self.mi_cols + c;
                self.palette_size_y[idx] = n_colors as u8;
                let mut pal = [0u16; PALETTE_MAX_SIZE];
                for i in 0..n_colors.min(PALETTE_MAX_SIZE) {
                    pal[i] = colors[i];
                }
                self.palette_colors_y[idx] = pal;
            }
        }
    }

    fn get_palette_cache_y(&self, x: usize, y: usize) -> Vec<u16> {
        let mi_x = x / 4;
        let mi_y = y / 4;

        let above_available = mi_y > 0 && (y % 64) != 0;
        let left_available = mi_x > 0;

        let (above_n, above_colors) = if above_available {
            let idx = (mi_y - 1) * self.mi_cols + mi_x;
            let n = self.palette_size_y[idx] as usize;
            (n, self.palette_colors_y[idx])
        } else {
            (0, [0u16; PALETTE_MAX_SIZE])
        };

        let (left_n, left_colors) = if left_available {
            let idx = mi_y * self.mi_cols + (mi_x - 1);
            let n = self.palette_size_y[idx] as usize;
            (n, self.palette_colors_y[idx])
        } else {
            (0, [0u16; PALETTE_MAX_SIZE])
        };

        if above_n == 0 && left_n == 0 {
            return Vec::new();
        }

        let mut cache = Vec::with_capacity(2 * PALETTE_MAX_SIZE);
        let mut ai = 0;
        let mut li = 0;
        let mut an = above_n;
        let mut ln = left_n;

        while an > 0 && ln > 0 {
            cache.push(above_colors[ai]);
            ai += 1;
            an -= 1;
            cache.push(left_colors[li]);
            li += 1;
            ln -= 1;
        }
        while an > 0 {
            cache.push(above_colors[ai]);
            ai += 1;
            an -= 1;
        }
        while ln > 0 {
            cache.push(left_colors[li]);
            li += 1;
            ln -= 1;
        }

        cache
    }

    fn mi_idx(&self, row: usize, col: usize) -> Option<usize> {
        if row < self.mi_rows && col < self.mi_cols {
            Some(row * self.mi_cols + col)
        } else {
            None
        }
    }

    fn gather_neighbor_indices(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        sb_size: usize,
        use_line_buffer_neighbors: bool,
    ) -> [Option<usize>; 2] {
        let mi_x = x / 4;
        let mi_y = y / 4;
        let mi_w = w / 4;
        let mi_h = h / 4;
        let mib_size = (sb_size / 4).max(1);
        let not_at_sb_top_boundary = (mi_y % mib_size) != 0;

        let mut neighbors = [None, None];
        let mut push_idx = 0usize;
        let push = |slot: Option<usize>, out: &mut [Option<usize>; 2], idx: &mut usize| {
            if slot.is_some() && *idx < 2 {
                out[*idx] = slot;
                *idx += 1;
            }
        };

        let bottom_left = if mi_x > 0 {
            let row = (mi_y + mi_h - 1).min(self.mi_rows.saturating_sub(1));
            self.mi_idx(row, mi_x - 1)
        } else {
            None
        };
        push(bottom_left, &mut neighbors, &mut push_idx);

        let allow_above_for_spatial = use_line_buffer_neighbors || not_at_sb_top_boundary;

        let above_right = if allow_above_for_spatial && mi_y > 0 {
            let col = (mi_x + mi_w - 1).min(self.mi_cols.saturating_sub(1));
            self.mi_idx(mi_y - 1, col)
        } else {
            None
        };
        push(above_right, &mut neighbors, &mut push_idx);

        let left = if mi_x > 0 {
            self.mi_idx(mi_y, mi_x - 1)
        } else {
            None
        };
        push(left, &mut neighbors, &mut push_idx);

        let above = if allow_above_for_spatial && mi_y > 0 {
            self.mi_idx(mi_y - 1, mi_x)
        } else {
            None
        };
        push(above, &mut neighbors, &mut push_idx);

        neighbors
    }

    fn intrabc_ctx(&self, x: usize, y: usize, w: usize, h: usize, sb_size: usize) -> usize {
        self.gather_neighbor_indices(x, y, w, h, sb_size, false)
            .iter()
            .flatten()
            .filter(|&&idx| self.is_intrabc[idx])
            .count()
    }

    fn skip_txfm_ctx(&self, x: usize, y: usize, w: usize, h: usize, sb_size: usize) -> usize {
        self.gather_neighbor_indices(x, y, w, h, sb_size, true)
            .iter()
            .flatten()
            .filter(|&&idx| self.skip_txfm[idx])
            .count()
    }

    fn morph_pred_ctx(&self, x: usize, y: usize, w: usize, h: usize, sb_size: usize) -> usize {
        self.gather_neighbor_indices(x, y, w, h, sb_size, false)
            .iter()
            .flatten()
            .filter(|&&idx| self.is_intrabc[idx] && self.morph_pred[idx])
            .count()
    }

    fn intrabc_bv_from_mi(&self, mi_row: isize, mi_col: isize) -> Option<(i32, i32)> {
        if mi_row < 0 || mi_col < 0 {
            return None;
        }
        let row = mi_row as usize;
        let col = mi_col as usize;
        if row >= self.mi_rows || col >= self.mi_cols {
            return None;
        }
        let idx = row * self.mi_cols + col;
        if !self.is_intrabc[idx] {
            return None;
        }
        Some((self.intrabc_bv_row[idx], self.intrabc_bv_col[idx]))
    }

    fn push_intrabc_ref_bv(cands: &mut Vec<(i32, i32)>, mv: (i32, i32), max_cands: usize) {
        if cands.len() >= max_cands {
            return;
        }
        if cands.iter().any(|&v| v == mv) {
            return;
        }
        cands.push(mv);
    }

    fn push_intrabc_ref_bv_default(cands: &mut Vec<(i32, i32)>, mv: (i32, i32), max_cands: usize) {
        if cands.len() >= max_cands {
            return;
        }
        cands.push(mv);
    }

    fn intrabc_ref_bv_valid_for_block(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        mv: (i32, i32),
    ) -> bool {
        let ref_x = x as i32 + mv.1 / 8;
        let ref_y = y as i32 + mv.0 / 8;
        ref_x > -(w as i32)
            && ref_y > -(h as i32)
            && ref_x < self.frame_w as i32
            && ref_y < self.frame_h as i32
    }

    fn intrabc_rmb_unit_mi_size(&self) -> usize {
        ((self.sb_size / 4) >> 3).max(1)
    }

    fn intrabc_rmb_units_count(&self, w: usize, h: usize) -> usize {
        let unit_mi_size = self.intrabc_rmb_unit_mi_size();
        let mi_bw = (w / 4).max(1);
        let mi_bh = (h / 4).max(1);
        (mi_bw / unit_mi_size).max(1) * (mi_bh / unit_mi_size).max(1)
    }

    fn decide_intrabc_refmv_bank_hits(&mut self, x: usize, y: usize, w: usize, h: usize) {
        let mi_sb_size = (self.sb_size / 4).max(1);
        let unit_mi_size = self.intrabc_rmb_unit_mi_size();
        let mi_row_in_sb = (y / 4) % mi_sb_size;
        let mi_col_in_sb = (x / 4) % mi_sb_size;
        let units_count = self.intrabc_rmb_units_count(w, h);
        if mi_row_in_sb == 0 && mi_col_in_sb == 0 {
            self.intrabc_refmv_bank.remain_hits = units_count.max(BANK_1ST_UNIT_UPDATE_COUNT);
            self.intrabc_refmv_bank.unit_hits = 0;
        } else if mi_row_in_sb % unit_mi_size == 0 && mi_col_in_sb % unit_mi_size == 0 {
            self.intrabc_refmv_bank.remain_hits += units_count;
            self.intrabc_refmv_bank.unit_hits = 0;
        }
    }

    fn note_non_intrabc_refmv_bank_block(&mut self, x: usize, y: usize, w: usize, h: usize) {
        self.decide_intrabc_refmv_bank_hits(x, y, w, h);
    }

    fn update_intrabc_refmv_bank(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        mv: (i32, i32),
    ) {
        self.decide_intrabc_refmv_bank_hits(x, y, w, h);
        if self.intrabc_refmv_bank.remain_hits == 0
            || self.intrabc_refmv_bank.unit_hits >= BANK_UNIT_MAX_ALLOWED_LEFTOVER_UPDATES
            || self.intrabc_refmv_bank.sb_hits >= MAX_RMB_SB_HITS
        {
            return;
        }
        self.intrabc_refmv_bank.remain_hits -= 1;
        self.intrabc_refmv_bank.unit_hits += 1;
        self.intrabc_refmv_bank.sb_hits += 1;
        self.intrabc_refmv_bank.update_lru(mv);
    }

    fn intrabc_ref_bv_candidates(
        &self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        enable_refmvbank: bool,
        max_bvp_drl_bits: u32,
    ) -> Vec<(i32, i32)> {
        let mi_x = (x / 4) as isize;
        let mi_y = (y / 4) as isize;
        let mi_w = (w / 4) as isize;
        let mi_h = (h / 4) as isize;
        let max_cands = max_bvp_drl_bits as usize + 1;
        let mut cands = Vec::with_capacity(max_cands);

        let left_available = mi_x > 0;
        let up_available = mi_y > 0;
        let sb_mi_size = (self.sb_size / 4) as isize;
        let mask_row = mi_y & (sb_mi_size - 1);
        let mask_col = mi_x & (sb_mi_size - 1);

        let has_tr = if mi_w > 16 {
            false
        } else {
            let tr_mask_row = mask_row - 1;
            let tr_mask_col = mask_col + mi_w;
            if tr_mask_row < 0 {
                true
            } else {
                tr_mask_col < sb_mi_size
            }
        };

        let has_bl = if mi_h > 16 {
            false
        } else {
            let bl_mask_row = mask_row + mi_h;
            let bl_mask_col = mask_col - 1;
            if bl_mask_row >= sb_mi_size {
                false
            } else if bl_mask_col < 0 {
                true
            } else {
                true
            }
        };

        let aligned_offset = |col_offset: isize| -> isize {
            let aligned_mi_col = (mi_x >> 1) << 1;
            aligned_mi_col + col_offset - mi_x
        };

        let above_smvp_available = |col_offset: isize| -> bool {
            if !up_available {
                return false;
            }
            let aligned_smvp_mi_col = ((mi_x >> 1) << 1) + col_offset;
            aligned_smvp_mi_col >= 0 && aligned_smvp_mi_col < self.mi_cols as isize
        };

        let is_sb_boundary = (mi_y % sb_mi_size) == 0;
        let block_width_type = if mi_w == 1 {
            0
        } else if mi_w == 2 {
            1
        } else {
            2
        };

        let (state0, state1, state2, state3): (
            (bool, isize),
            (bool, isize),
            (bool, isize),
            (bool, isize),
        ) = if !is_sb_boundary {
            if block_width_type == 0 {
                (
                    (up_available, mi_w - 1),
                    (false, 0),
                    (has_tr, mi_w),
                    (up_available && left_available, -1),
                )
            } else {
                (
                    (up_available, mi_w - 1),
                    (up_available, 0),
                    (has_tr, mi_w),
                    (up_available && left_available, -1),
                )
            }
        } else if block_width_type <= 1 {
            (
                (above_smvp_available(0), aligned_offset(0)),
                (false, 0),
                (has_tr && above_smvp_available(2), aligned_offset(2)),
                (above_smvp_available(-2), aligned_offset(-2)),
            )
        } else {
            (
                (above_smvp_available(mi_w - 2), aligned_offset(mi_w - 2)),
                (above_smvp_available(0), aligned_offset(0)),
                (has_tr && above_smvp_available(mi_w), aligned_offset(mi_w)),
                (above_smvp_available(-2), aligned_offset(-2)),
            )
        };

        let mut add_spatial = |row_off: isize, col_off: isize| {
            if let Some(mv) = self.intrabc_bv_from_mi(mi_y + row_off, mi_x + col_off) {
                Self::push_intrabc_ref_bv(&mut cands, mv, max_cands);
            }
        };

        if left_available {
            add_spatial(mi_h - 1, -1);
        }
        if state0.0 {
            add_spatial(-1, state0.1);
        }
        let height_at_least_two = left_available && mi_h > 1;
        if height_at_least_two {
            add_spatial(0, -1);
        }
        if state1.0 {
            add_spatial(-1, state1.1);
        }
        if has_bl {
            add_spatial(mi_h, -1);
        }
        if state2.0 {
            add_spatial(-1, state2.1);
        }
        if state3.0 {
            add_spatial(-1, state3.1);
        }

        if left_available {
            let col_adj = if mi_w < 2 && (mi_x & 1) != 0 { 1 } else { 0 };
            let col_off = -3 + col_adj;
            add_spatial(mi_h - 1, col_off);
            if mi_h > 1 {
                add_spatial(0, col_off);
            }
        }

        if enable_refmvbank {
            for mv in self.intrabc_refmv_bank.newest_first() {
                if self.intrabc_ref_bv_valid_for_block(x, y, w, h, mv) {
                    Self::push_intrabc_ref_bv(&mut cands, mv, max_cands);
                }
            }
        }

        let default_ref_bv_list = [
            (-(self.sb_size as i32) * 8, 0),
            (0, (-(self.sb_size as i32) - 256) * 8),
            (-(h as i32) * 8, 0),
            (0, -(w as i32) * 8),
        ];

        for &mv in &default_ref_bv_list {
            Self::push_intrabc_ref_bv_default(&mut cands, mv, max_cands);
        }

        cands
    }

    fn fill_intrabc(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        skip_txfm: bool,
        morph: bool,
        bv_row: i32,
        bv_col: i32,
    ) {
        let mi_x = x / 4;
        let mi_y = y / 4;
        let mi_w = w / 4;
        let mi_h = h / 4;
        for r in mi_y..(mi_y + mi_h).min(self.mi_rows) {
            for c in mi_x..(mi_x + mi_w).min(self.mi_cols) {
                let idx = r * self.mi_cols + c;
                self.is_intrabc[idx] = true;
                self.skip_txfm[idx] = skip_txfm;
                self.morph_pred[idx] = morph;
                self.intrabc_bv_row[idx] = bv_row;
                self.intrabc_bv_col[idx] = bv_col;
            }
        }
    }
}

fn get_palette_color_index_context(
    color_map: &[u8],
    stride: usize,
    r: usize,
    c: usize,
    color_order: &mut [u8],
    _n_colors: usize,
) -> usize {
    for i in 0..PALETTE_MAX_SIZE {
        color_order[i] = i as u8;
    }
    let mut status = [0u8; PALETTE_MAX_SIZE];
    let mut cnt = 0usize;

    let swap = |order: &mut [u8], stat: &mut [u8], pos: usize, val: usize, cnt: &mut usize| {
        order[pos] = val as u8;
        stat[val] = 1;
        *cnt += 1;
    };

    let ctx;
    if r > 0 && c > 0 {
        let left = color_map[r * stride + c - 1] as usize;
        let tl = color_map[(r - 1) * stride + c - 1] as usize;
        let top = color_map[(r - 1) * stride + c] as usize;

        if left == tl && left == top {
            ctx = 4;
            swap(color_order, &mut status, 0, left, &mut cnt);
        } else if left == top {
            ctx = 3;
            swap(color_order, &mut status, 0, left, &mut cnt);
            swap(color_order, &mut status, 1, tl, &mut cnt);
        } else if left == tl {
            ctx = 2;
            swap(color_order, &mut status, 0, left, &mut cnt);
            swap(color_order, &mut status, 1, top, &mut cnt);
        } else if tl == top {
            ctx = 2;
            swap(color_order, &mut status, 0, top, &mut cnt);
            swap(color_order, &mut status, 1, left, &mut cnt);
        } else {
            ctx = 1;
            swap(color_order, &mut status, 0, left, &mut cnt);
            swap(color_order, &mut status, 1, top, &mut cnt);
            swap(color_order, &mut status, 2, tl, &mut cnt);
        }
    } else if c == 0 && r > 0 {
        ctx = 0;
        let top = color_map[(r - 1) * stride + c] as usize;
        swap(color_order, &mut status, 0, top, &mut cnt);
    } else if c > 0 && r == 0 {
        ctx = 0;
        let left = color_map[r * stride + c - 1] as usize;
        swap(color_order, &mut status, 0, left, &mut cnt);
    } else {
        ctx = 0;
    }

    let mut write_idx = cnt;
    for read_idx in 0..PALETTE_MAX_SIZE {
        if status[read_idx] == 0 {
            color_order[write_idx] = read_idx as u8;
            write_idx += 1;
        }
    }

    ctx
}

fn read_palette_colors_y(ec: &mut DaalaDecoder, n: usize, palette_cache: &[u16]) -> Vec<u16> {
    let bit_depth = 8u32;
    let n_cache = palette_cache.len();
    let mut colors = Vec::with_capacity(n);

    let mut idx = 0;
    for i in 0..n_cache {
        if idx >= n {
            break;
        }
        let use_cache = ec.decode_bit();
        eprintln!(
            "[TRACE] cache_bit[{}]={} cache_val={}",
            i, use_cache, palette_cache[i]
        );
        ec.trace_state(&format!("after_cache_bit[{}]", i));
        if use_cache != 0 {
            colors.push(palette_cache[i]);
            idx += 1;
        }
    }

    if idx < n {
        colors.push(ec.decode_literal(bit_depth) as u16);
        idx += 1;
    }

    if idx < n {
        let min_bits = bit_depth - 3;
        let mut bits = min_bits + ec.decode_literal(2);
        let mut range =
            ((1i32 << bit_depth) - colors.last().copied().unwrap_or(0) as i32 - 1).max(0);

        for _ in idx..n {
            let delta = if bits > 0 {
                ec.decode_literal(bits) + 1
            } else {
                1
            };
            let prev = *colors.last().unwrap() as u32;
            let new_val = (prev + delta).min((1u32 << bit_depth) - 1) as u16;
            colors.push(new_val);
            range -= new_val as i32 - prev as i32;
            bits = bits.min(ceil_log2(range.max(0) as u32));
        }
    }

    for i in 0..colors.len() {
        for j in 1..colors.len() - i {
            if colors[j - 1] > colors[j] {
                colors.swap(j - 1, j);
            }
        }
    }

    colors
}

fn decode_palette_map(
    ec: &mut DaalaDecoder,
    width: usize,
    height: usize,
    n_colors: usize,
    cdf: &mut FrameCdfContext,
) -> Vec<u8> {
    let transverse_allowed = width < 64 && height < 64;
    let direction = if transverse_allowed {
        ec.decode_bit() as usize
    } else {
        0
    };

    let (axis2_limit, axis1_limit) = if direction == 0 {
        (height, width)
    } else {
        (width, height)
    };

    let plane_block_width = width;
    let mut color_map = vec![0u8; width * height];
    let ps = n_colors - 2;

    let mut prev_identity_row_flag = 0usize;

    for ax2 in 0..axis2_limit {
        let ctx = if ax2 == 0 { 3 } else { prev_identity_row_flag };
        let identity_row_flag = ec.decode_symbol_mut(&mut cdf.identity_row_y[ctx]);

        if identity_row_flag == 2 && ax2 > 0 {
            for ax1 in 0..axis1_limit {
                let (r, c) = if direction == 0 {
                    (ax2, ax1)
                } else {
                    (ax1, ax2)
                };
                let (pr, pc) = if direction == 0 {
                    (ax2 - 1, ax1)
                } else {
                    (ax1, ax2 - 1)
                };
                color_map[r * plane_block_width + c] = color_map[pr * plane_block_width + pc];
            }
        } else if identity_row_flag == 1 {
            for ax1 in 0..axis1_limit {
                let (r, c) = if direction == 0 {
                    (ax2, ax1)
                } else {
                    (ax1, ax2)
                };
                if ax1 == 0 {
                    if ax2 == 0 {
                        let val = ec.decode_uniform(n_colors as u32) as u8;
                        color_map[r * plane_block_width + c] = val;
                    } else {
                        let mut color_order = [0u8; 8];
                        let color_ctx = get_palette_color_index_context(
                            &color_map,
                            plane_block_width,
                            r,
                            c,
                            &mut color_order,
                            n_colors,
                        );
                        let color_idx =
                            ec.decode_symbol_mut(&mut cdf.palette_y_color_index[ps][color_ctx]);
                        color_map[r * plane_block_width + c] = color_order[color_idx];
                    }
                } else {
                    let (nr, nc) = if direction == 0 {
                        (r, c.wrapping_sub(1))
                    } else {
                        (r.wrapping_sub(1), c)
                    };
                    if nr < height && nc < width {
                        color_map[r * plane_block_width + c] =
                            color_map[nr * plane_block_width + nc];
                    }
                }
            }
        } else {
            for ax1 in 0..axis1_limit {
                let (r, c) = if direction == 0 {
                    (ax2, ax1)
                } else {
                    (ax1, ax2)
                };
                if ax2 == 0 && ax1 == 0 {
                    let val = ec.decode_uniform(n_colors as u32) as u8;
                    color_map[r * plane_block_width + c] = val;
                } else {
                    let mut color_order = [0u8; 8];
                    let color_ctx = get_palette_color_index_context(
                        &color_map,
                        plane_block_width,
                        r,
                        c,
                        &mut color_order,
                        n_colors,
                    );
                    let color_idx =
                        ec.decode_symbol_mut(&mut cdf.palette_y_color_index[ps][color_ctx]);
                    color_map[r * plane_block_width + c] = color_order[color_idx];
                }
            }
        }

        prev_identity_row_flag = identity_row_flag;
    }
    color_map
}

fn read_truncated_unary_mvd(ec: &mut DaalaDecoder, cdf: &mut FrameCdfContext) -> usize {
    let mut result = 0;
    for bit_idx in 0..3usize {
        let this_bit = if bit_idx == 0 {
            ec.decode_symbol_mut(&mut cdf.ndvc_shell_offset_class2) as usize
        } else {
            ec.decode_literal(1) as usize
        };
        result = bit_idx + this_bit;
        if this_bit == 0 {
            break;
        }
    }
    result
}

fn read_tu_quasi_uniform(
    ec: &mut DaalaDecoder,
    cdf: &mut FrameCdfContext,
    max_coded_value: usize,
) -> usize {
    let max_trunc_unary_value = 2;
    let max_idx_bits = max_coded_value.min(max_trunc_unary_value);
    let mut col = 0usize;
    for bit_idx in 0..max_idx_bits {
        let ctx = bit_idx.min(1);
        let this_bit = ec.decode_symbol_mut(&mut cdf.ndvc_col_mv_greater_flags[ctx]) as usize;
        col = bit_idx + this_bit;
        if this_bit == 0 {
            break;
        }
    }
    if max_coded_value > max_trunc_unary_value && col == max_trunc_unary_value {
        let remainder_max = max_coded_value - max_trunc_unary_value;
        let remainder = ec.decode_uniform((remainder_max + 1) as u32) as usize;
        col = remainder + max_trunc_unary_value;
    }
    col
}

fn read_mv_delta(ec: &mut DaalaDecoder, precision: usize, cdf: &mut FrameCdfContext) -> (i32, i32) {
    let shell_set = ec.decode_symbol_mut(&mut cdf.ndvc_joint_shell_set) as usize;

    let num_mv_class = 11 + precision;
    let n0 = num_mv_class >> 1;

    let shell_class = if shell_set == 0 {
        ec.decode_symbol_mut(&mut cdf.ndvc_joint_shell_class_0[precision]) as usize
    } else {
        let c = ec.decode_symbol_mut(&mut cdf.ndvc_joint_shell_class_1[precision]) as usize;
        n0 + c
    };

    let shell_cls_offset = if shell_class == 0 {
        ec.decode_symbol_mut(&mut cdf.ndvc_shell_offset_low_class[0]) as usize
    } else if shell_class == 1 {
        ec.decode_symbol_mut(&mut cdf.ndvc_shell_offset_low_class[1]) as usize
    } else if shell_class == 2 {
        read_truncated_unary_mvd(ec, cdf)
    } else {
        let mut offset = 0usize;
        for i in 0..shell_class {
            let bit = ec.decode_symbol_mut(&mut cdf.ndvc_shell_offset_other_class[i]) as usize;
            offset |= bit << i;
        }
        offset
    };

    let shell_class_base = if shell_class == 0 {
        0
    } else {
        1usize << shell_class
    };
    let shell_index = shell_cls_offset + shell_class_base;

    if shell_index == 0 {
        return (0, 0);
    }

    let maximum_pair_index = shell_index >> 1;
    let this_pair_index = if maximum_pair_index == 0 {
        0
    } else {
        read_tu_quasi_uniform(ec, cdf, maximum_pair_index)
    };

    let skip_coding_col_bit = (this_pair_index == maximum_pair_index) && (shell_index % 2 == 0);
    let scaled_mv_col = if skip_coding_col_bit {
        maximum_pair_index
    } else {
        let ctx = shell_class.min(3);
        let bit = ec.decode_symbol_mut(&mut cdf.ndvc_col_mv_index[ctx]) as usize;
        if bit == 0 {
            this_pair_index
        } else {
            shell_index - this_pair_index
        }
    };

    let scaled_row = (shell_index - scaled_mv_col) as i32;
    let scaled_col = scaled_mv_col as i32;

    let start_lsb = (6 - precision as i32).max(0);
    (scaled_row << start_lsb, scaled_col << start_lsb)
}

fn lower_mv_precision_component(value: i32, precision: usize) -> i32 {
    let shift = (6usize).saturating_sub(precision);
    let radix = 1i32 << shift;
    if radix == 1 {
        return value;
    }
    let mut reduced = value;
    let rem = reduced % radix;
    if rem != 0 {
        reduced -= rem;
        if rem.abs() > (radix >> 1) {
            if rem > 0 {
                reduced += radix;
            } else {
                reduced -= radix;
            }
        }
    }
    reduced
}

fn lower_mv_precision_pair(row: i32, col: i32, precision: usize) -> (i32, i32) {
    (
        lower_mv_precision_component(row, precision),
        lower_mv_precision_component(col, precision),
    )
}

fn read_intrabc_rest(
    ec: &mut DaalaDecoder,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    skip_txfm: bool,
    cfg: &TileDecodeConfig,
    cdf: &mut FrameCdfContext,
    mi: &mut ModeInfoGrid,
) -> (i32, i32, bool) {
    let intrabc_mode = ec.decode_symbol_mut(&mut cdf.intrabc_mode) as usize;
    ec.trace_state(&format!("ibc_mode={} block=({},{})", intrabc_mode, x, y));

    let mut drl_idx = 0u32;
    for idx in 0..cfg.max_bvp_drl_bits {
        let bit = ec.decode_literal(1);
        drl_idx = idx + bit;
        if bit == 0 {
            break;
        }
    }
    ec.trace_state(&format!("ibc_drl_idx={} block=({},{})", drl_idx, x, y));

    let ref_bv_cands =
        mi.intrabc_ref_bv_candidates(x, y, w, h, cfg.enable_refmvbank, cfg.max_bvp_drl_bits);
    for (i, cand) in ref_bv_cands.iter().enumerate() {
        ec.trace_state(&format!(
            "ibc_cand[{}]=({},{}) block=({},{})",
            i, cand.0, cand.1, x, y
        ));
    }
    let drl_idx_usize = drl_idx as usize;
    let ref_bv = ref_bv_cands
        .get(drl_idx_usize)
        .copied()
        .or_else(|| ref_bv_cands.last().copied())
        .unwrap_or((0, -(w as i32) * 8));
    ec.trace_state(&format!(
        "ibc_ref_bv=({},{}) cands={} drl_idx={} block=({},{})",
        ref_bv.0,
        ref_bv.1,
        ref_bv_cands.len(),
        drl_idx,
        x,
        y
    ));

    let bv_precision = if !cfg.cur_frame_force_integer_mv && intrabc_mode == 0 {
        let prec_idx = ec.decode_symbol_mut(&mut cdf.intrabc_bv_precision) as usize;
        ec.trace_state(&format!("ibc_bv_prec_idx={} block=({},{})", prec_idx, x, y));
        if prec_idx == 0 { 3 } else { 5 }
    } else {
        3
    };

    let (bv_row, bv_col) = if intrabc_mode == 0 {
        let (row_mag, col_mag) = read_mv_delta(ec, bv_precision, cdf);
        ec.trace_state(&format!(
            "ibc_mv_mag=({},{}) block=({},{})",
            row_mag, col_mag, x, y
        ));

        let row_signed = if row_mag != 0 {
            let sign = ec.decode_literal(1);
            if sign != 0 { -row_mag } else { row_mag }
        } else {
            0
        };
        let col_signed = if col_mag != 0 {
            let sign = ec.decode_literal(1);
            if sign != 0 { -col_mag } else { col_mag }
        } else {
            0
        };
        ec.trace_state(&format!(
            "ibc_mv_delta=({},{}) block=({},{})",
            row_signed, col_signed, x, y
        ));

        let (ref_row, ref_col) = if bv_precision < 4 {
            lower_mv_precision_pair(ref_bv.0, ref_bv.1, bv_precision)
        } else {
            ref_bv
        };
        (ref_row + row_signed, ref_col + col_signed)
    } else {
        ref_bv
    };

    ec.trace_state(&format!(
        "ibc_mv_final=({},{}) block=({},{})",
        bv_row, bv_col, x, y
    ));

    let morph_pred = if cfg.allow_intrabc_morph_pred {
        let ctx = mi.morph_pred_ctx(x, y, w, h, cfg.sb_size);
        let pred = ec.decode_symbol_mut(&mut cdf.morph_pred[ctx]) != 0;
        ec.trace_state(&format!(
            "ibc_morph_pred={} ctx={} block=({},{})",
            pred, ctx, x, y
        ));
        pred
    } else {
        false
    };

    mi.fill_intrabc(x, y, w, h, skip_txfm, morph_pred, bv_row, bv_col);
    (bv_row, bv_col, morph_pred)
}

fn decode_leaf_block(
    ec: &mut DaalaDecoder,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    tree_type: TreeType,
    chroma_ref_info: ChromaRefInfo,
    cfg: &TileDecodeConfig,
    cdf: &mut FrameCdfContext,
    mi: &mut ModeInfoGrid,
    blocks: &mut Vec<DecodedBlock>,
    state: &mut TileState,
) {
    if tree_type == TreeType::Chroma {
        ec.trace_state(&format!(
            "CHROMA_BLOCK mi=({},{}) {}x{}",
            y / 4,
            x / 4,
            width / 2,
            height / 2
        ));
    }
    let mut use_intrabc = false;
    let mut ibc_skip_txfm = false;
    let chroma_x = chroma_ref_info.x_base;
    let chroma_y = chroma_ref_info.y_base;
    let chroma_width = chroma_ref_info.w_base;
    let chroma_height = chroma_ref_info.h_base;
    let uv_w = chroma_width / 2;
    let uv_h = chroma_height / 2;
    let decode_chroma =
        tree_type != TreeType::Luma && chroma_ref_info.is_chroma_ref && uv_w >= 4 && uv_h >= 4;

    if tree_type != TreeType::Chroma {
        ec.trace_state(&format!(
            "before_block_syntax block=({},{}) {}x{} ibc={} cdef={}/{} ccso={}",
            x,
            y,
            width,
            height,
            cfg.allow_intrabc,
            cfg.cdef_frame_enable,
            cfg.nb_cdef_strengths,
            cfg.ccso_frame_enable
        ));

        if cfg.allow_intrabc && !(width == 64 && height == 64) && width <= 64 && height <= 64 {
            let ibc_ctx = mi.intrabc_ctx(x, y, width, height, cfg.sb_size);
            let ibc = ec.decode_symbol_mut(&mut cdf.intrabc[ibc_ctx]);
            ec.trace_state(&format!(
                "after_use_intrabc={} ctx={} block=({},{})",
                ibc, ibc_ctx, x, y
            ));
            use_intrabc = ibc != 0;
        }

        if use_intrabc {
            let skip_ctx = mi.skip_txfm_ctx(x, y, width, height, cfg.sb_size);
            let s = ec.decode_symbol_mut(&mut cdf.skip_txfm[skip_ctx]) != 0;
            ec.trace_state(&format!(
                "ibc_skip_txfm={} ctx={} block=({},{})",
                s, skip_ctx, x, y
            ));
            ibc_skip_txfm = s;
        }

        if cfg.cdef_frame_enable && cfg.nb_cdef_strengths >= 2 {
            let cdef_unit = (x / CDEF_UNIT_SIZE, y / CDEF_UNIT_SIZE);
            if !state.cdef_read.contains(&cdef_unit) {
                let ctx = 0;
                let is_strength0 = ec.decode_symbol_mut(&mut cdf.cdef_strength_index0[ctx]);
                ec.trace_state(&format!(
                    "after_cdef_idx0={} block=({},{})",
                    is_strength0, x, y
                ));
                if is_strength0 == 0 && cfg.nb_cdef_strengths > 2 {
                    let cdf_idx = cfg.nb_cdef_strengths as usize - 3;
                    let _strength = ec.decode_symbol_mut(&mut cdf.cdef_cdf[cdf_idx]);
                    ec.trace_state(&format!(
                        "after_cdef_strength={} block=({},{})",
                        _strength, x, y
                    ));
                }
                state.cdef_read.insert(cdef_unit);
            }
        }

        if cfg.ccso_frame_enable {
            let ccso_unit = (x / CCSO_UNIT_SIZE, y / CCSO_UNIT_SIZE);
            for plane in 0..3 {
                if cfg.ccso_enable[plane] && !state.ccso_read[plane].contains(&ccso_unit) {
                    let ctx = 0;
                    let _ccso_blk = ec.decode_symbol_mut(&mut cdf.ccso[plane][ctx]);
                    ec.trace_state(&format!(
                        "after_ccso_p{}={} block=({},{})",
                        plane, _ccso_blk, x, y
                    ));
                    state.ccso_read[plane].insert(ccso_unit);
                }
            }
        }
    }

    if tree_type != TreeType::Chroma && use_intrabc {
        let (bv_row, bv_col, morph_pred) =
            read_intrabc_rest(ec, x, y, width, height, ibc_skip_txfm, cfg, cdf, mi);
        if cfg.enable_refmvbank {
            mi.update_intrabc_refmv_bank(x, y, width, height, (bv_row, bv_col));
        }

        let (y_tu, u_tu, v_tu) = if ibc_skip_txfm {
            (vec![], vec![], vec![])
        } else {
            let y_tu = decode_block_tus(
                ec, 0, x, y, width, height, false, true, 0, 0, cfg, cdf, state,
            );
            let (u_tu, v_tu) = if decode_chroma {
                let u_tu = decode_block_tus(
                    ec, 1, chroma_x, chroma_y, uv_w, uv_h, false, true, 0, 0, cfg, cdf, state,
                );
                let v_u_has = u_tu.last().map(|t| t.eob > 0).unwrap_or(false);
                let v_tu = decode_block_tus(
                    ec, 2, chroma_x, chroma_y, uv_w, uv_h, v_u_has, true, 0, 0, cfg, cdf, state,
                );
                (u_tu, v_tu)
            } else {
                (vec![], vec![])
            };
            (y_tu, u_tu, v_tu)
        };

        ec.trace_state(&format!(
            "after_ibc_block ({},{}) {}x{}",
            x, y, width, height
        ));
        blocks.push(DecodedBlock {
            x,
            y,
            width,
            height,
            is_chroma_ref: chroma_ref_info.is_chroma_ref,
            chroma_x,
            chroma_y,
            chroma_width,
            chroma_height,
            tree_type,
            is_intrabc: true,
            morph_pred,
            bv_row,
            bv_col,
            y_mode: 0,
            joint_y_mode: 0,
            uv_mode: 0,
            mrl_index: 0,
            cfl_alpha_u: 0,
            cfl_alpha_v: 0,
            cfl_idx: 0,
            mhccp_dir: 0,
            y_tu,
            u_tu,
            v_tu,
            palette_y_colors: vec![],
            palette_y_map: vec![],
        });
        return;
    }

    let mut y_mode = 0u8;
    let mut fsc_mode = 0usize;
    let is_directional;
    let mut joint_y_mode = 0u8;
    let mut mrl_index = 0u8;
    let mut multi_line_mrl = 0u8;

    if tree_type != TreeType::Chroma {
        let y_mode_ctx = mi.y_mode_ctx(x, y, width, height);
        let (left_joint, above_joint) = mi.get_neighbor_joints(x, y, width, height);
        eprintln!(
            "[MODE_CTX] block=({},{}) size={}x{} y_mode_idx_ctx={} left_joint={} above_joint={}",
            x / 4,
            y / 4,
            width / 4,
            height / 4,
            y_mode_ctx,
            left_joint,
            above_joint
        );
        if x / 4 == 16 && y / 4 == 4 {
            let mi_x = x / 4;
            let mi_y = y / 4;
            let _mi_w = width / 4;
            let mi_h = height / 4;
            let left_row = (mi_y + mi_h - 1).min(mi.mi_rows - 1);
            let left_col = mi_x - 1;
            let idx = left_row * mi.mi_cols + left_col;
            eprintln!(
                "[DEBUG] block(16,4) left_row={} left_col={} idx={} mi_cols={} value={}",
                left_row, left_col, idx, mi.mi_cols, mi.joint_y_mode[idx]
            );
            for r in 0..8 {
                eprintln!(
                    "[DEBUG] MI row {} cols 12-15: [{}, {}, {}, {}]",
                    r,
                    mi.joint_y_mode[r * mi.mi_cols + 12],
                    mi.joint_y_mode[r * mi.mi_cols + 13],
                    mi.joint_y_mode[r * mi.mi_cols + 14],
                    mi.joint_y_mode[r * mi.mi_cols + 15]
                );
            }
        }
        {
            let ms = &cdf.y_mode_set;
            eprintln!(
                "[MODE_CDF] y_mode_set_cdf=[{}, {}, {}, {}]",
                ms[0], ms[1], ms[2], ms[3]
            );
            let mi_cdf = &cdf.y_mode_idx[y_mode_ctx];
            eprintln!(
                "[MODE_CDF] y_mode_idx_cdf[{}]=[{}, {}, {}, {}, {}, {}, {}, {}]",
                y_mode_ctx,
                mi_cdf[0],
                mi_cdf[1],
                mi_cdf[2],
                mi_cdf[3],
                mi_cdf[4],
                mi_cdf[5],
                mi_cdf[6],
                mi_cdf[7]
            );
        }
        let (mode_idx, dpcm_joint) = read_y_mode(ec, y_mode_ctx, cfg.coded_lossless, cdf);

        let y_intra_mode_list = build_y_intra_mode_list(width, height, left_joint, above_joint);
        joint_y_mode = dpcm_joint.unwrap_or_else(|| y_intra_mode_list[mode_idx]);
        let (actual_mode, _delta_angle) = set_y_mode_and_delta_angle(joint_y_mode);
        y_mode = actual_mode;
        eprintln!(
            "[MODE_CTX] block=({},{}) mode_idx={} joint_y_mode={} actual_mode={}",
            x / 4,
            y / 4,
            mode_idx,
            joint_y_mode,
            y_mode
        );
        ec.trace_state(&format!(
            "after_y_mode_mapping mode_idx={} joint={} actual={} block=({},{})",
            mode_idx, joint_y_mode, y_mode, x, y
        ));

        fsc_mode = if cfg.enable_idtx_intra && allow_fsc_intra(width, height) {
            let bsize_group = fsc_bsize_group(width, height);
            let ctx = mi.fsc_mode_ctx(x, y, width, height, cfg.sb_size);
            eprintln!(
                "[TRACE] before_fsc_mode cdf={:?} ctx={} block=({},{}) bsg={}",
                &cdf.fsc_mode[ctx][bsize_group], ctx, x, y, bsize_group
            );
            let fsc = ec.decode_symbol_mut(&mut cdf.fsc_mode[ctx][bsize_group]);
            ec.trace_state(&format!(
                "after_fsc_mode={} bsg={} block=({},{})",
                fsc, bsize_group, x, y
            ));
            fsc
        } else {
            0
        };

        is_directional = is_directional_mode(y_mode);
        mrl_index = if cfg.enable_mrls && is_directional {
            let ctx = mi.mrl_index_ctx(x, y, width, height, cfg.sb_size);
            let mrl = ec.decode_symbol_mut(&mut cdf.mrl_index[ctx]);
            ec.trace_state(&format!(
                "after_mrl_index={} ctx={} block=({},{})",
                mrl, ctx, x, y
            ));
            if mrl > 0 {
                let multi_ctx = mi.multi_line_mrl_ctx(x, y, width, height, cfg.sb_size);
                multi_line_mrl = ec.decode_symbol_mut(&mut cdf.multi_line_mrl[multi_ctx]) as u8;
                ec.trace_state(&format!(
                    "after_multi_line_mrl={} ctx={} block=({},{})",
                    multi_line_mrl, multi_ctx, x, y
                ));
            }
            mrl as u8
        } else {
            0
        };
    }

    let is_sb2c_block = tree_type == TreeType::Chroma && x < 64 && y >= 64 && y < 128;
    if tree_type == TreeType::Chroma {
        let mi_row = y / 4;
        let mi_col = x / 4;
        let joint = mi.joint_y_mode[mi_row * mi.mi_cols + mi_col];
        joint_y_mode = joint;
        let (actual_mode, _delta_angle) = set_y_mode_and_delta_angle(joint);
        y_mode = actual_mode;
        if is_sb2c_block {
            eprintln!(
                "[SB2C_BLOCK] mi=({},{}) {}x{} y_mode={} joint={}",
                mi_row, mi_col, width, height, y_mode, joint
            );
        }
    }

    let mut uv_mode = 0u8;
    let mut cfl_alpha_u = 0i8;
    let mut cfl_alpha_v = 0i8;
    let mut cfl_idx = 0u8;
    let mut mhccp_dir = 0u8;
    if tree_type != TreeType::Luma && chroma_ref_info.is_chroma_ref {
        if is_sb2c_block {
            ec.trace_state(&format!(
                "[SB2C_BLOCK] before_uv_mode mi=({},{}) {}x{} y_mode={}",
                y / 4,
                x / 4,
                width,
                height,
                y_mode
            ));
        }
        let cfl_ctx = mi.cfl_ctx(chroma_x, chroma_y);
        let (mode, au, av, ci, md) = read_uv_mode(
            ec,
            y_mode,
            cfg.coded_lossless,
            chroma_width,
            chroma_height,
            cfl_ctx,
            cfg.enable_cfl_intra,
            cfg.enable_mhccp,
            state.cfl_allowed_in_sdp,
            cdf,
        );
        uv_mode = mode;
        cfl_alpha_u = au;
        cfl_alpha_v = av;
        cfl_idx = ci;
        mhccp_dir = md;
        if is_sb2c_block {
            ec.trace_state(&format!(
                "[SB2C_BLOCK] after_uv_mode={} mi=({},{}) {}x{} cfl_idx={} mh_dir={}",
                uv_mode,
                y / 4,
                x / 4,
                width,
                height,
                cfl_idx,
                mhccp_dir
            ));
        }
    }

    let mut palette_y_colors: Vec<u16> = Vec::new();
    let mut palette_y_map: Vec<u8> = Vec::new();
    if tree_type != TreeType::Chroma
        && cfg.allow_screen_content_tools
        && y_mode == 0
        && width * height >= 64
        && width <= 64
        && height <= 64
    {
        ec.trace_state(&format!(
            "before_palette_y block=({},{}) {}x{}",
            x, y, width, height
        ));
        let palette_y = ec.decode_symbol_mut(&mut cdf.palette_y_mode);
        ec.trace_state(&format!(
            "after_palette_y={} block=({},{})",
            palette_y, x, y
        ));

        if palette_y == 1 {
            let palette_size_raw = ec.decode_symbol_mut(&mut cdf.palette_y_size);
            let n_colors = palette_size_raw + 2;
            ec.trace_state(&format!(
                "after_palette_size={} n_colors={} block=({},{})",
                palette_size_raw, n_colors, x, y
            ));

            let palette_cache = mi.get_palette_cache_y(x, y);
            eprintln!(
                "[TRACE] palette_cache block=({},{}) n_cache={} cache={:?}",
                x,
                y,
                palette_cache.len(),
                palette_cache
            );
            let colors = read_palette_colors_y(ec, n_colors, &palette_cache);
            eprintln!(
                "[TRACE] palette_colors block=({},{}) colors={:?}",
                x, y, colors
            );
            ec.trace_state(&format!("after_palette_colors block=({},{})", x, y));

            mi.fill_palette_y(x, y, width, height, n_colors, &colors);

            palette_y_map = decode_palette_map(ec, width, height, n_colors, cdf);
            palette_y_colors = colors;
            ec.trace_state(&format!("after_palette_map block=({},{})", x, y));
        }
    }

    let is_cfl = uv_mode == 13;
    if tree_type != TreeType::Chroma {
        mi.fill(
            x,
            y,
            width,
            height,
            joint_y_mode,
            false,
            fsc_mode as u8,
            mrl_index,
            multi_line_mrl,
        );
        if cfg.enable_refmvbank {
            mi.note_non_intrabc_refmv_bank_block(x, y, width, height);
        }
    }
    if tree_type != TreeType::Luma && chroma_ref_info.is_chroma_ref {
        mi.fill_cfl(chroma_x, chroma_y, chroma_width, chroma_height, is_cfl);
    }

    let y_tu = if tree_type != TreeType::Chroma {
        decode_block_tus(
            ec, 0, x, y, width, height, false, false, y_mode, fsc_mode, cfg, cdf, state,
        )
    } else {
        vec![]
    };

    let (u_tu, v_tu) = if decode_chroma {
        if is_sb2c_block {
            ec.trace_state(&format!(
                "[SB2C_BLOCK] before_u_coeffs mi=({},{}) uv={}x{}",
                y / 4,
                x / 4,
                uv_w,
                uv_h
            ));
        }
        let u_tu = decode_block_tus(
            ec, 1, chroma_x, chroma_y, uv_w, uv_h, false, false, uv_mode, 0, cfg, cdf, state,
        );
        let v_u_has = u_tu.last().map(|t| t.eob > 0).unwrap_or(false);
        if is_sb2c_block {
            ec.trace_state(&format!(
                "[SB2C_BLOCK] after_u_coeffs mi=({},{}) v_u_has={}",
                y / 4,
                x / 4,
                v_u_has
            ));
        }
        let v_tu = decode_block_tus(
            ec, 2, chroma_x, chroma_y, uv_w, uv_h, v_u_has, false, uv_mode, 0, cfg, cdf, state,
        );
        if is_sb2c_block {
            ec.trace_state(&format!(
                "[SB2C_BLOCK] after_v_coeffs mi=({},{})",
                y / 4,
                x / 4
            ));
        }
        ec.trace_state(&format!("after_v_coeffs block=({},{})", x, y));
        (u_tu, v_tu)
    } else {
        if tree_type != TreeType::Chroma {
            ec.trace_state(&format!("after_luma_coeffs block=({},{})", x, y));
        }
        (vec![], vec![])
    };

    if tree_type == TreeType::Chroma {
        ec.trace_state(&format!(
            "CHROMA_BLOCK_DONE mi=({},{}) {}x{} uv_mode={}",
            y / 4,
            x / 4,
            width / 2,
            height / 2,
            uv_mode
        ));
    }
    ec.trace_state(&format!(
        "after_block ({},{}) {}x{} tree={:?}",
        x, y, width, height, tree_type
    ));

    blocks.push(DecodedBlock {
        x,
        y,
        width,
        height,
        is_chroma_ref: chroma_ref_info.is_chroma_ref,
        chroma_x,
        chroma_y,
        chroma_width,
        chroma_height,
        tree_type,
        is_intrabc: false,
        morph_pred: false,
        bv_row: 0,
        bv_col: 0,
        y_mode,
        joint_y_mode,
        uv_mode,
        mrl_index,
        cfl_alpha_u,
        cfl_alpha_v,
        cfl_idx,
        mhccp_dir,
        y_tu,
        u_tu,
        v_tu,
        palette_y_colors,
        palette_y_map,
    });
}

fn subblock_offsets(
    partition: PartitionType,
    w: usize,
    h: usize,
) -> Vec<(usize, usize, usize, usize)> {
    match partition {
        PartitionType::None => vec![(0, 0, w, h)],
        PartitionType::Horz => vec![(0, 0, w, h / 2), (0, h / 2, w, h / 2)],
        PartitionType::Vert => vec![(0, 0, w / 2, h), (w / 2, 0, w / 2, h)],
        PartitionType::Split => {
            let hw = w / 2;
            let hh = h / 2;
            vec![
                (0, 0, hw, hh),
                (hw, 0, hw, hh),
                (0, hh, hw, hh),
                (hw, hh, hw, hh),
            ]
        }
        PartitionType::Horz3 => {
            let qh = h / 4;
            let hw = w / 2;
            let hh = h / 2;
            vec![
                (0, 0, w, qh),
                (0, qh, hw, hh),
                (hw, qh, hw, hh),
                (0, qh + hh, w, qh),
            ]
        }
        PartitionType::Vert3 => {
            let qw = w / 4;
            let hw = w / 2;
            let hh = h / 2;
            vec![
                (0, 0, qw, h),
                (qw, 0, hw, hh),
                (qw, hh, hw, hh),
                (qw + hw, 0, qw, h),
            ]
        }
        PartitionType::Horz4A => {
            let e = h / 8;
            vec![
                (0, 0, w, e),
                (0, e, w, 2 * e),
                (0, 3 * e, w, 4 * e),
                (0, 7 * e, w, e),
            ]
        }
        PartitionType::Horz4B => {
            let e = h / 8;
            vec![
                (0, 0, w, e),
                (0, e, w, 4 * e),
                (0, 5 * e, w, 2 * e),
                (0, 7 * e, w, e),
            ]
        }
        PartitionType::Vert4A => {
            let e = w / 8;
            vec![
                (0, 0, e, h),
                (e, 0, 2 * e, h),
                (3 * e, 0, 4 * e, h),
                (7 * e, 0, e, h),
            ]
        }
        PartitionType::Vert4B => {
            let e = w / 8;
            vec![
                (0, 0, e, h),
                (e, 0, 4 * e, h),
                (5 * e, 0, 2 * e, h),
                (7 * e, 0, e, h),
            ]
        }
    }
}

fn have_nz_chroma_ref_offset(w: usize, h: usize, partition: PartitionType) -> bool {
    let w_lt_8 = w < 8;
    let h_lt_8 = h < 8;
    let w_lt_16 = w < 16;
    let h_lt_16 = h < 16;
    let w_lt_32 = w < 32;
    let h_lt_32 = h < 32;
    let w_lt_64 = w < 64;
    let h_lt_64 = h < 64;

    match partition {
        PartitionType::None => w_lt_8 || h_lt_8,
        PartitionType::Horz => w_lt_8 || h_lt_16,
        PartitionType::Vert => w_lt_16 || h_lt_8,
        PartitionType::Split => w_lt_16 || h_lt_16,
        PartitionType::Horz4A | PartitionType::Horz4B => w_lt_8 || h_lt_64,
        PartitionType::Vert4A | PartitionType::Vert4B => w_lt_64 || h_lt_8,
        PartitionType::Horz3 => w_lt_16 || h_lt_32,
        PartitionType::Vert3 => w_lt_32 || h_lt_16,
    }
}

fn is_sub_partition_chroma_ref(
    partition: PartitionType,
    index: usize,
    w: usize,
    h: usize,
    parent_w: usize,
    parent_h: usize,
    offset_started: bool,
) -> bool {
    let w_lt_8 = w < 8;
    let h_lt_8 = h < 8;

    match partition {
        PartitionType::None => true,
        PartitionType::Horz | PartitionType::Vert => index == 1,
        PartitionType::Split => {
            if offset_started {
                index == 3
            } else if w_lt_8 && h_lt_8 {
                index == 3
            } else if w_lt_8 {
                index == 1 || index == 3
            } else if h_lt_8 {
                index == 2 || index == 3
            } else {
                index == 1
            }
        }
        PartitionType::Horz4A
        | PartitionType::Horz4B
        | PartitionType::Vert4A
        | PartitionType::Vert4B => index == 3,
        PartitionType::Horz3 => {
            if parent_w == 8 && parent_h == 32 {
                index != 1
            } else {
                index == 3
            }
        }
        PartitionType::Vert3 => {
            if parent_w == 32 && parent_h == 8 {
                index != 1
            } else {
                index == 3
            }
        }
    }
}

fn set_chroma_ref_offset_size(
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    parent_w: usize,
    parent_h: usize,
    partition: PartitionType,
    parent_info: ChromaRefInfo,
) -> ChromaRefInfo {
    let mut info = ChromaRefInfo {
        is_chroma_ref: true,
        offset_started: true,
        x_base: parent_info.x_base,
        y_base: parent_info.y_base,
        w_base: parent_info.w_base,
        h_base: parent_info.h_base,
    };

    match partition {
        PartitionType::None
        | PartitionType::Horz
        | PartitionType::Vert
        | PartitionType::Horz4A
        | PartitionType::Horz4B
        | PartitionType::Vert4A
        | PartitionType::Vert4B => {}
        PartitionType::Horz3 => {
            if parent_w == 8 && parent_h == 32 {
                if w == 4 && h == 16 {
                    info.y_base = parent_info.y_base + 8;
                    info.w_base = 8;
                    info.h_base = 16;
                } else {
                    info = ChromaRefInfo::new(x, y, w, h);
                }
            }
        }
        PartitionType::Vert3 => {
            if parent_w == 32 && parent_h == 8 {
                if w == 16 && h == 4 {
                    info.x_base = parent_info.x_base + 8;
                    info.w_base = 16;
                    info.h_base = 8;
                } else {
                    info = ChromaRefInfo::new(x, y, w, h);
                }
            }
        }
        PartitionType::Split => {
            if w < 8 && h < 8 {
            } else if w < 8 {
                info.w_base = parent_w;
                info.h_base = h;
                if y != parent_info.y_base {
                    info.y_base = parent_info.y_base + h;
                }
            } else {
                info.w_base = w;
                info.h_base = parent_h;
                if x != parent_info.x_base {
                    info.x_base = parent_info.x_base + w;
                }
            }
        }
    }

    info
}

fn compute_chroma_ref_info(
    tree_type: TreeType,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    index: usize,
    parent_ctx: Option<ChromaRefContext>,
) -> ChromaRefInfo {
    let mut info = ChromaRefInfo::new(x, y, w, h);

    match tree_type {
        TreeType::Luma => {
            info.is_chroma_ref = false;
            return info;
        }
        TreeType::Chroma => {
            info.is_chroma_ref = true;
            return info;
        }
        TreeType::Shared => {}
    }

    let Some(parent_ctx) = parent_ctx else {
        return info;
    };

    if parent_ctx.info.is_chroma_ref {
        if parent_ctx.info.offset_started {
            info.is_chroma_ref = is_sub_partition_chroma_ref(
                parent_ctx.partition,
                index,
                w,
                h,
                parent_ctx.width,
                parent_ctx.height,
                true,
            );
            info.offset_started = true;
            info.x_base = parent_ctx.info.x_base;
            info.y_base = parent_ctx.info.y_base;
            info.w_base = parent_ctx.info.w_base;
            info.h_base = parent_ctx.info.h_base;
        } else if have_nz_chroma_ref_offset(
            parent_ctx.width,
            parent_ctx.height,
            parent_ctx.partition,
        ) {
            info = set_chroma_ref_offset_size(
                x,
                y,
                w,
                h,
                parent_ctx.width,
                parent_ctx.height,
                parent_ctx.partition,
                parent_ctx.info,
            );
            info.is_chroma_ref = is_sub_partition_chroma_ref(
                parent_ctx.partition,
                index,
                w,
                h,
                parent_ctx.width,
                parent_ctx.height,
                false,
            );
        }
    } else {
        info.is_chroma_ref = false;
        info.offset_started = true;
        info.x_base = parent_ctx.info.x_base;
        info.y_base = parent_ctx.info.y_base;
        info.w_base = parent_ctx.info.w_base;
        info.h_base = parent_ctx.info.h_base;
    }

    info
}

fn bsize_map_val(w: usize, h: usize) -> usize {
    match (w, h) {
        (4, 4) | (4, 8) | (8, 4) | (8, 8) => 0,
        (8, 16) | (16, 8) | (16, 16) => 1,
        (16, 32) | (32, 16) | (32, 32) => 2,
        (32, 64) | (64, 32) | (64, 64) => 3,
        (64, 128) => 4,
        (128, 64) => 5,
        (128, 128) => 6,
        (4, 16) => 10,
        (16, 4) => 11,
        (8, 32) => 12,
        (32, 8) => 13,
        (16, 64) => 14,
        (64, 16) => 15,
        _ => 0,
    }
}

fn bsize_rect_map_val(w: usize, h: usize) -> usize {
    match (w, h) {
        (4, 4) | (4, 8) | (8, 4) | (8, 8) | (16, 16) => 0,
        (8, 16) | (16, 32) => 1,
        (16, 8) | (32, 16) => 2,
        (32, 32) => 3,
        (32, 64) => 4,
        (64, 32) => 5,
        (64, 64) => 6,
        (64, 128) => 7,
        (128, 64) => 8,
        (128, 128) => 9,
        (4, 16) | (8, 32) | (16, 64) => 13,
        (16, 4) | (32, 8) | (64, 16) => 14,
        _ => 0,
    }
}

fn is_partition_point(w: usize, h: usize) -> bool {
    matches!(
        (w, h),
        (4, 8)
            | (8, 4)
            | (8, 8)
            | (8, 16)
            | (16, 8)
            | (16, 16)
            | (16, 32)
            | (32, 16)
            | (32, 32)
            | (32, 64)
            | (64, 32)
            | (64, 64)
            | (64, 128)
            | (128, 64)
            | (128, 128)
            | (256, 256)
            | (256, 128)
            | (128, 256)
            | (4, 16)
            | (16, 4)
            | (8, 32)
            | (32, 8)
            | (16, 64)
            | (64, 16)
    )
}

fn is_square_split_eligible(w: usize, h: usize) -> bool {
    w == h && (w == 128 || w == 256)
}

fn is_ext_partition_allowed_at_bsize(w: usize, h: usize, tree_type: TreeType) -> bool {
    if w > 64 || h > 64 {
        return false;
    }
    if w <= 8 && h <= 8 {
        return false;
    }
    if tree_type == TreeType::Chroma && w <= 16 && h <= 16 {
        return false;
    }
    if (w == 4 && h == 16) || (w == 16 && h == 4) {
        return false;
    }
    if tree_type == TreeType::Chroma && ((w == 8 && h == 32) || (w == 32 && h == 8)) {
        return false;
    }
    true
}

fn is_ext_partition_valid(w: usize, h: usize, rect_type: usize, tree_type: TreeType) -> bool {
    if !is_ext_partition_allowed_at_bsize(w, h, tree_type) {
        return false;
    }
    if (w == 16 && h == 8 && rect_type == 0) || (w == 8 && h == 16 && rect_type == 1) {
        return false;
    }
    if tree_type == TreeType::Chroma
        && ((w == 32 && h == 16 && rect_type == 0) || (w == 16 && h == 32 && rect_type == 1))
    {
        return false;
    }
    if (w == 32 && h == 8 && rect_type == 0) || (w == 8 && h == 32 && rect_type == 1) {
        return false;
    }
    if (w == 64 && h == 16 && rect_type == 0) || (w == 16 && h == 64 && rect_type == 1) {
        return false;
    }
    true
}

fn is_3way_valid(w: usize, h: usize, _rect_type: usize) -> bool {
    let (sub_w, sub_h) = (w / 2, h / 2);
    sub_w >= 4 && sub_h >= 4
}

fn is_4way_valid(w: usize, h: usize, rect_type: usize, tree_type: TreeType) -> bool {
    let min_sub = if rect_type == 0 { h / 8 } else { w / 8 };
    if min_sub < 4 {
        return false;
    }
    if tree_type == TreeType::Chroma {
        let split_dim = if rect_type == 0 { h } else { w };
        return split_dim == 64;
    }
    true
}

fn partition_context_values(w: usize, h: usize) -> (u8, u8) {
    match (w, h) {
        (4, 4) => (63, 63),
        (4, 8) => (63, 62),
        (8, 4) => (62, 63),
        (8, 8) => (62, 62),
        (8, 16) => (62, 60),
        (16, 8) => (60, 62),
        (16, 16) => (60, 60),
        (16, 32) => (60, 56),
        (32, 16) => (56, 60),
        (32, 32) => (56, 56),
        (32, 64) => (56, 48),
        (64, 32) => (48, 56),
        (64, 64) => (48, 48),
        (64, 128) => (48, 32),
        (128, 64) => (32, 48),
        (128, 128) => (32, 32),
        (128, 256) => (32, 0),
        (256, 128) => (0, 32),
        (256, 256) => (0, 0),
        (4, 16) => (63, 60),
        (16, 4) => (60, 63),
        (8, 32) => (62, 56),
        (32, 8) => (56, 62),
        (16, 64) => (60, 48),
        (64, 16) => (48, 60),
        (4, 32) => (63, 56),
        (32, 4) => (56, 63),
        (8, 64) => (62, 48),
        (64, 8) => (48, 62),
        _ => (0, 0),
    }
}

fn update_partition_context(
    state: &mut TileState,
    tree_type: TreeType,
    mi_col: usize,
    mi_row: usize,
    leaf_w: usize,
    leaf_h: usize,
) {
    let plane = if tree_type == TreeType::Chroma { 1 } else { 0 };
    let (above_val, left_val) = partition_context_values(leaf_w, leaf_h);
    let mi_w = leaf_w / 4;
    let mi_h = leaf_h / 4;

    for i in 0..mi_w {
        if mi_col + i < state.above_part_ctx[plane].len() {
            state.above_part_ctx[plane][mi_col + i] = above_val;
        }
    }
    let mi_row_in_sb = mi_row & 63;
    for i in 0..mi_h {
        if mi_row_in_sb + i < state.left_part_ctx[plane].len() {
            state.left_part_ctx[plane][mi_row_in_sb + i] = left_val;
        }
    }
}

#[derive(Clone, Copy)]
enum PartCtxMode {
    Split,
    SquareSplit,
    Rect,
    Ext,
    FourWay,
}

fn partition_plane_context(
    above_ctx: &[u8],
    left_ctx: &[u8],
    w: usize,
    h: usize,
    rect_type: usize,
    ctx_mode: PartCtxMode,
) -> usize {
    let mi_w = w / 4;
    let mi_h = h / 4;
    let bsl_w = if mi_w > 0 {
        mi_w.trailing_zeros() as usize
    } else {
        0
    };
    let bsl_h = if mi_h > 0 {
        mi_h.trailing_zeros() as usize
    } else {
        0
    };

    let above_val = above_ctx.first().copied().unwrap_or(0);
    let left_val = left_ctx.first().copied().unwrap_or(0);

    let (ctx, bsize_offset) = match ctx_mode {
        PartCtxMode::Split => {
            let shift_w = if bsl_w >= 1 { bsl_w - 1 } else { 0 };
            let shift_h = if bsl_h >= 1 { bsl_h - 1 } else { 0 };
            let above_bit = (above_val >> shift_w) & 1;
            let left_bit = (left_val >> shift_h) & 1;
            let ctx = (left_bit as usize) * 2 + (above_bit as usize);
            eprintln!(
                "[PCTX] Split {}x{}: above_val={} left_val={} shift_w={} shift_h={} above_bit={} left_bit={} ctx={}",
                w, h, above_val, left_val, shift_w, shift_h, above_bit, left_bit, ctx
            );
            (ctx, bsize_map_val(w, h) * PARTITION_PLOFFSET)
        }
        PartCtxMode::SquareSplit => {
            let shift_w = if bsl_w >= 1 { bsl_w - 1 } else { 0 };
            let shift_h = if bsl_h >= 1 { bsl_h - 1 } else { 0 };
            let above_bit = (above_val >> shift_w) & 1;
            let left_bit = (left_val >> shift_h) & 1;
            let ctx = (left_bit as usize) * 2 + (above_bit as usize);
            let offset = if w == 256 { PARTITION_PLOFFSET } else { 0 };
            (ctx, offset)
        }
        PartCtxMode::Rect => {
            let shift_w = if bsl_w >= 1 { bsl_w - 1 } else { 0 };
            let shift_h = if bsl_h >= 1 { bsl_h - 1 } else { 0 };
            let above_bit = (above_val >> shift_w) & 1;
            let left_bit = (left_val >> shift_h) & 1;
            let ctx = (left_bit as usize) * 2 + (above_bit as usize);
            eprintln!(
                "[PCTX] Rect {}x{}: above_val={} left_val={} shift_w={} shift_h={} above_bit={} left_bit={} ctx={}",
                w, h, above_val, left_val, shift_w, shift_h, above_bit, left_bit, ctx
            );
            (ctx, bsize_rect_map_val(w, h) * PARTITION_PLOFFSET)
        }
        PartCtxMode::Ext | PartCtxMode::FourWay => {
            let shift_w = if bsl_w >= 2 { bsl_w - 2 } else { 0 };
            let shift_h = if bsl_h >= 2 { bsl_h - 2 } else { 0 };
            let (ctx1, ctx2) = if rect_type == 0 {
                let c1 = (left_ctx.first().copied().unwrap_or(0) >> shift_h) & 1;
                let mid = mi_h / 2;
                let c2 = if mid < left_ctx.len() {
                    (left_ctx[mid] >> shift_h) & 1
                } else {
                    0
                };
                (c1, c2)
            } else {
                let c1 = (above_ctx.first().copied().unwrap_or(0) >> shift_w) & 1;
                let mid = mi_w / 2;
                let c2 = if mid < above_ctx.len() {
                    (above_ctx[mid] >> shift_w) & 1
                } else {
                    0
                };
                (c1, c2)
            };
            let ctx = (ctx1 as usize) * 2 + (ctx2 as usize);
            eprintln!(
                "[PCTX] Ext {}x{} rt={}: ctx1={} ctx2={} ctx={}",
                w, h, rect_type, ctx1, ctx2, ctx
            );
            (ctx, bsize_map_val(w, h) * PARTITION_PLOFFSET)
        }
    };

    (ctx + bsize_offset).min(PARTITION_CONTEXTS - 1)
}

struct PartitionAllowed {
    none: bool,
    horz: bool,
    vert: bool,
    split: bool,
    horz_3: bool,
    vert_3: bool,
    horz_4a: bool,
    horz_4b: bool,
    vert_4a: bool,
    vert_4b: bool,
}

impl PartitionAllowed {
    fn count(&self) -> usize {
        self.none as usize
            + self.horz as usize
            + self.vert as usize
            + self.split as usize
            + self.horz_3 as usize
            + self.vert_3 as usize
            + self.horz_4a as usize
            + self.horz_4b as usize
            + self.vert_4a as usize
            + self.vert_4b as usize
    }

    fn only_one(&self) -> Option<PartitionType> {
        if self.count() != 1 {
            return None;
        }
        if self.none {
            return Some(PartitionType::None);
        }
        if self.horz {
            return Some(PartitionType::Horz);
        }
        if self.vert {
            return Some(PartitionType::Vert);
        }
        if self.split {
            return Some(PartitionType::Split);
        }
        if self.horz_3 {
            return Some(PartitionType::Horz3);
        }
        if self.vert_3 {
            return Some(PartitionType::Vert3);
        }
        if self.horz_4a {
            return Some(PartitionType::Horz4A);
        }
        if self.horz_4b {
            return Some(PartitionType::Horz4B);
        }
        if self.vert_4a {
            return Some(PartitionType::Vert4A);
        }
        if self.vert_4b {
            return Some(PartitionType::Vert4B);
        }
        None
    }

    fn horz_any(&self) -> bool {
        self.horz || self.horz_3 || self.horz_4a || self.horz_4b
    }

    fn vert_any(&self) -> bool {
        self.vert || self.vert_3 || self.vert_4a || self.vert_4b
    }

    fn horz_ext_any(&self) -> bool {
        self.horz_3 || self.horz_4a || self.horz_4b
    }

    fn vert_ext_any(&self) -> bool {
        self.vert_3 || self.vert_4a || self.vert_4b
    }

    fn only_rect_type(&self) -> Option<usize> {
        let h = self.horz_any();
        let v = self.vert_any();
        if h && v {
            None
        } else if h {
            Some(0)
        } else {
            Some(1)
        }
    }
}

fn is_valid_block_size(w: usize, h: usize) -> bool {
    w >= 4
        && h >= 4
        && w <= 256
        && h <= 256
        && w.is_power_of_two()
        && h.is_power_of_two()
        && w / h <= 16
        && h / w <= 16
}

fn compute_partition_allowed(
    w: usize,
    h: usize,
    tree_type: TreeType,
    enable_ext: bool,
    enable_uneven_4way: bool,
) -> PartitionAllowed {
    let mut pa = PartitionAllowed {
        none: true,
        horz: h / 2 >= 4 && is_valid_block_size(w, h / 2),
        vert: w / 2 >= 4 && is_valid_block_size(w / 2, h),
        split: is_square_split_eligible(w, h),
        horz_3: false,
        vert_3: false,
        horz_4a: false,
        horz_4b: false,
        vert_4a: false,
        vert_4b: false,
    };

    if enable_ext {
        if is_ext_partition_valid(w, h, 0, tree_type) {
            pa.horz_3 = is_3way_valid(w, h, 0);
            if enable_uneven_4way && is_4way_valid(w, h, 0, tree_type) {
                pa.horz_4a = true;
                pa.horz_4b = true;
            }
        }
        if is_ext_partition_valid(w, h, 1, tree_type) {
            pa.vert_3 = is_3way_valid(w, h, 1);
            if enable_uneven_4way && is_4way_valid(w, h, 1, tree_type) {
                pa.vert_4a = true;
                pa.vert_4b = true;
            }
        }
    }

    if tree_type == TreeType::Chroma {
        match (w, h) {
            (8, 16) | (8, 32) => {
                pa.vert = false;
                pa.vert_3 = false;
                pa.vert_4a = false;
                pa.vert_4b = false;
            }
            (16, 8) | (32, 8) => {
                pa.horz = false;
                pa.horz_3 = false;
                pa.horz_4a = false;
                pa.horz_4b = false;
            }
            _ => {}
        }
    }

    pa
}

fn rect_type_implied_full(w: usize, h: usize, tree_type: TreeType) -> Option<usize> {
    match (w, h) {
        (4, 8) | (64, 128) | (4, 16) | (128, 256) => return Some(0),
        (8, 4) | (128, 64) | (16, 4) | (256, 128) => return Some(1),
        _ => {}
    }
    if tree_type == TreeType::Chroma {
        match (w, h) {
            (8, 16) | (8, 32) => return Some(0),
            (16, 8) | (32, 8) => return Some(1),
            _ => {}
        }
    }
    None
}

fn read_partition_type(
    ec: &mut DaalaDecoder,
    w: usize,
    h: usize,
    tree_type: TreeType,
    cdf: &mut FrameCdfContext,
    above_part: &[u8],
    left_part: &[u8],
    cfg: &TileDecodeConfig,
) -> PartitionType {
    let plane = if tree_type == TreeType::Chroma { 1 } else { 0 };
    let pa = compute_partition_allowed(
        w,
        h,
        tree_type,
        cfg.enable_ext_partitions,
        cfg.enable_uneven_4way_partitions,
    );

    if let Some(only) = pa.only_one() {
        eprintln!("[PART] only_allowed={:?} at {}x{}", only, w, h);
        return only;
    }

    let do_split = if !pa.none {
        1
    } else {
        let split_ctx = partition_plane_context(above_part, left_part, w, h, 0, PartCtxMode::Split);
        ec.trace_state(&format!(
            "before_do_split bsize={}x{} ctx={}",
            w, h, split_ctx
        ));
        let ds = ec.decode_symbol_mut(&mut cdf.do_split_part[plane][split_ctx.min(63)]);
        ec.trace_state(&format!("after_do_split={} bsize={}x{}", ds, w, h));
        ds
    };
    if do_split == 0 {
        return PartitionType::None;
    }

    if pa.split {
        let sq_ctx =
            partition_plane_context(above_part, left_part, w, h, 0, PartCtxMode::SquareSplit);
        let do_sq = ec.decode_symbol_mut(
            &mut cdf.do_square_split[plane][sq_ctx.min(SQUARE_SPLIT_CONTEXTS - 1)],
        );
        ec.trace_state(&format!("after_do_sq_split={} bsize={}x{}", do_sq, w, h));
        if do_sq == 1 {
            return PartitionType::Split;
        }
    }

    let rect_type = if let Some(implied) = rect_type_implied_full(w, h, tree_type) {
        implied
    } else if let Some(only_rt) = pa.only_rect_type() {
        only_rt
    } else {
        let rect_ctx = partition_plane_context(above_part, left_part, w, h, 0, PartCtxMode::Rect);
        let rt = ec.decode_symbol_mut(&mut cdf.rect_type[plane][rect_ctx.min(63)]);
        ec.trace_state(&format!(
            "after_rect_type={} ctx={} bsize={}x{}",
            rt, rect_ctx, w, h
        ));
        rt
    };

    let base_type = if rect_type == 0 {
        PartitionType::Horz
    } else {
        PartitionType::Vert
    };

    let non_ext_allowed = if rect_type == 0 { pa.horz } else { pa.vert };
    let ext_allowed = if rect_type == 0 {
        pa.horz_ext_any()
    } else {
        pa.vert_ext_any()
    };

    if !ext_allowed {
        return base_type;
    }

    let do_ext = if !non_ext_allowed {
        1
    } else if !ext_allowed {
        0
    } else {
        let ext_ctx =
            partition_plane_context(above_part, left_part, w, h, rect_type, PartCtxMode::Ext);
        let ext = ec.decode_symbol_mut(&mut cdf.do_ext_partition[plane][ext_ctx.min(63)]);
        ec.trace_state(&format!(
            "after_do_ext={} ctx={} bsize={}x{}",
            ext, ext_ctx, w, h
        ));
        ext
    };

    if do_ext == 0 {
        return base_type;
    }

    let part_3_allowed = if rect_type == 0 { pa.horz_3 } else { pa.vert_3 };
    let part_4_allowed = if rect_type == 0 {
        pa.horz_4a || pa.horz_4b
    } else {
        pa.vert_4a || pa.vert_4b
    };

    if !part_4_allowed {
        return match base_type {
            PartitionType::Horz => PartitionType::Horz3,
            PartitionType::Vert => PartitionType::Vert3,
            _ => base_type,
        };
    }

    if !part_3_allowed {
        let four_way_type = ec.decode_literal(1);
        return match (base_type, four_way_type) {
            (PartitionType::Horz, 0) => PartitionType::Horz4A,
            (PartitionType::Horz, _) => PartitionType::Horz4B,
            (PartitionType::Vert, 0) => PartitionType::Vert4A,
            (PartitionType::Vert, _) => PartitionType::Vert4B,
            _ => base_type,
        };
    }

    let uneven_ctx =
        partition_plane_context(above_part, left_part, w, h, rect_type, PartCtxMode::FourWay);
    let uneven = ec.decode_symbol_mut(&mut cdf.do_uneven_4way[plane][uneven_ctx.min(63)]);
    if uneven == 0 {
        return match base_type {
            PartitionType::Horz => PartitionType::Horz3,
            PartitionType::Vert => PartitionType::Vert3,
            _ => base_type,
        };
    }

    let four_way_type = ec.decode_literal(1);
    match (base_type, four_way_type) {
        (PartitionType::Horz, 0) => PartitionType::Horz4A,
        (PartitionType::Horz, _) => PartitionType::Horz4B,
        (PartitionType::Vert, 0) => PartitionType::Vert4A,
        (PartitionType::Vert, _) => PartitionType::Vert4B,
        _ => base_type,
    }
}

fn is_cfl_allowed_for_sdp_partitions(
    luma_part: Option<PartitionType>,
    chroma_part: PartitionType,
) -> bool {
    let Some(luma) = luma_part else {
        return true;
    };
    if chroma_part == PartitionType::None {
        return true;
    }
    if luma == chroma_part {
        return true;
    }
    let horz_family = |p: PartitionType| {
        matches!(
            p,
            PartitionType::Horz
                | PartitionType::Horz3
                | PartitionType::Horz4A
                | PartitionType::Horz4B
        )
    };
    let vert_family = |p: PartitionType| {
        matches!(
            p,
            PartitionType::Vert
                | PartitionType::Vert3
                | PartitionType::Vert4A
                | PartitionType::Vert4B
        )
    };
    if horz_family(luma) && horz_family(chroma_part) {
        return true;
    }
    if vert_family(luma) && vert_family(chroma_part) {
        return true;
    }
    false
}

fn is_sdp_share_partition(parent: PartitionType, child: PartitionType) -> bool {
    if child == PartitionType::None {
        return false;
    }
    match parent {
        PartitionType::Vert4A
        | PartitionType::Vert4B
        | PartitionType::Horz4A
        | PartitionType::Horz4B => false,
        PartitionType::Horz | PartitionType::Horz3 => !matches!(
            child,
            PartitionType::Horz
                | PartitionType::Horz3
                | PartitionType::Horz4A
                | PartitionType::Horz4B
        ),
        PartitionType::Vert | PartitionType::Vert3 => !matches!(
            child,
            PartitionType::Vert
                | PartitionType::Vert3
                | PartitionType::Vert4A
                | PartitionType::Vert4B
        ),
        _ => false,
    }
}

fn should_sdp_share(top_partition: PartitionType, sub_partitions: &[PartitionType]) -> bool {
    if top_partition == PartitionType::None {
        return true;
    }
    for &sub in sub_partitions {
        if !is_sdp_share_partition(top_partition, sub) {
            return false;
        }
    }
    true
}

fn sdp_chroma_part_from_luma(luma_part: PartitionType) -> PartitionType {
    match luma_part {
        PartitionType::None => PartitionType::None,
        PartitionType::Horz => PartitionType::Horz,
        PartitionType::Vert => PartitionType::Vert,
        PartitionType::Split => PartitionType::Split,
        PartitionType::Horz3 => PartitionType::Horz3,
        PartitionType::Vert3 => PartitionType::Vert3,
        PartitionType::Horz4A => PartitionType::Horz4A,
        PartitionType::Horz4B => PartitionType::Horz4B,
        PartitionType::Vert4A => PartitionType::Vert4A,
        PartitionType::Vert4B => PartitionType::Vert4B,
    }
}

fn implied_boundary_partition(
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    frame_w: usize,
    frame_h: usize,
) -> Option<PartitionType> {
    let has_rows = (y + h / 2) < frame_h;
    let has_cols = (x + w / 2) < frame_w;

    if has_rows && has_cols {
        return None;
    }

    if w == h {
        if has_rows && !has_cols {
            Some(PartitionType::Vert)
        } else {
            Some(PartitionType::Horz)
        }
    } else if h > w {
        if !has_rows {
            Some(PartitionType::Horz)
        } else if !has_cols && (w < 16 || (x + w / 4) >= frame_w) {
            Some(PartitionType::Horz)
        } else {
            None
        }
    } else {
        if !has_cols {
            Some(PartitionType::Vert)
        } else if !has_rows && (h < 16 || (y + h / 4) >= frame_h) {
            Some(PartitionType::Vert)
        } else {
            None
        }
    }
}

fn decode_partition(
    ec: &mut DaalaDecoder,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    frame_w: usize,
    frame_h: usize,
    tree_type: TreeType,
    index: usize,
    parent_chroma_ctx: Option<ChromaRefContext>,
    cfg: &TileDecodeConfig,
    cdf: &mut FrameCdfContext,
    mi: &mut ModeInfoGrid,
    blocks: &mut Vec<DecodedBlock>,
    state: &mut TileState,
) {
    let was_tracking = state.sdp_luma_tracking;
    if was_tracking {
        state.sdp_luma_depth += 1;
    }

    if x >= frame_w || y >= frame_h {
        if was_tracking {
            state.sdp_luma_depth -= 1;
        }
        return;
    }

    let chroma_ref_info = compute_chroma_ref_info(tree_type, x, y, w, h, index, parent_chroma_ctx);

    if cfg.enable_sdp && w == 64 && h == 64 && tree_type == TreeType::Shared {
        eprintln!("[PART] SDP split at ({},{}) {}x{}", x, y, w, h);
        state.sdp_luma_partition_64x64 = None;
        state.sdp_luma_sub_partitions.clear();
        state.sdp_luma_tracking = false;
        state.sdp_luma_depth = 0;
        decode_partition(
            ec,
            x,
            y,
            w,
            h,
            frame_w,
            frame_h,
            TreeType::Luma,
            0,
            None,
            cfg,
            cdf,
            mi,
            blocks,
            state,
        );
        state.sdp_luma_tracking = false;
        ec.trace_state(&format!("SDP_LUMA_DONE pixel=({},{}) 64x64", x, y));

        state.sdp_luma_partition_for_cfl = state.sdp_luma_partition_64x64;

        let sdp_share = if let Some(luma_part) = state.sdp_luma_partition_64x64 {
            should_sdp_share(luma_part, &state.sdp_luma_sub_partitions)
        } else {
            false
        };
        eprintln!(
            "[SDP] share={} luma_part={:?} subs={:?}",
            sdp_share, state.sdp_luma_partition_64x64, state.sdp_luma_sub_partitions
        );
        if !sdp_share {
            state.sdp_luma_partition_64x64 = None;
        }

        state.cfl_allowed_in_sdp = true;
        decode_partition(
            ec,
            x,
            y,
            w,
            h,
            frame_w,
            frame_h,
            TreeType::Chroma,
            0,
            None,
            cfg,
            cdf,
            mi,
            blocks,
            state,
        );
        ec.trace_state(&format!("SDP_CHROMA_DONE pixel=({},{}) 64x64", x, y));
        state.sdp_luma_partition_64x64 = None;
        if was_tracking {
            state.sdp_luma_depth -= 1;
        }
        return;
    }

    if !is_partition_point(w, h) {
        let cw = w.min(frame_w - x);
        let ch = h.min(frame_h - y);
        eprintln!(
            "[PART] LEAF at ({},{}) {}x{} tree={:?}",
            x, y, cw, ch, tree_type
        );
        if was_tracking && state.sdp_luma_depth == 1 {
            state.sdp_luma_sub_partitions.push(PartitionType::None);
        }
        let is_sb2c_leaf = tree_type == TreeType::Chroma && x < 64 && y >= 64 && y < 128;
        if is_sb2c_leaf {
            ec.trace_state(&format!(
                "[SB2C_LEAF] mi=({},{}) {}x{}",
                y / 4,
                x / 4,
                cw,
                ch
            ));
        }
        let is_sb3l_leaf = (tree_type == TreeType::Luma || tree_type == TreeType::Shared)
            && x >= 64
            && x < 128
            && y >= 64
            && y < 128;
        if is_sb3l_leaf {
            ec.trace_state(&format!(
                "[SB3L_LEAF] mi=({},{}) {}x{}",
                y / 4,
                x / 4,
                cw,
                ch
            ));
        }
        decode_leaf_block(
            ec,
            x,
            y,
            cw,
            ch,
            tree_type,
            chroma_ref_info,
            cfg,
            cdf,
            mi,
            blocks,
            state,
        );
        if is_sb2c_leaf {
            ec.trace_state(&format!(
                "[SB2C_LEAF_DONE] mi=({},{}) {}x{}",
                y / 4,
                x / 4,
                cw,
                ch
            ));
        }
        if is_sb3l_leaf {
            ec.trace_state(&format!(
                "[SB3L_LEAF_DONE] mi=({},{}) {}x{}",
                y / 4,
                x / 4,
                cw,
                ch
            ));
        }
        let mi_col = x / 4;
        let mi_row = y / 4;
        update_partition_context(state, tree_type, mi_col, mi_row, cw, ch);
        if was_tracking {
            state.sdp_luma_depth -= 1;
        }
        return;
    }

    let part_plane = if tree_type == TreeType::Chroma { 1 } else { 0 };
    let mi_col = x / 4;
    let mi_row = y / 4;
    let mi_row_in_sb = mi_row & 63;
    let mi_w = w / 4;
    let mi_h = h / 4;

    let above_end = (mi_col + mi_w).min(state.above_part_ctx[part_plane].len());
    let left_end = (mi_row_in_sb + mi_h).min(state.left_part_ctx[part_plane].len());
    let above_part = state.above_part_ctx[part_plane][mi_col..above_end].to_vec();
    let left_part = state.left_part_ctx[part_plane][mi_row_in_sb..left_end].to_vec();

    if tree_type == TreeType::Chroma && w == 8 && h == 8 {
        eprintln!(
            "[PART] CHROMA_8x8_FORCED_NONE at ({},{}) tree={:?}",
            x, y, tree_type
        );
        decode_leaf_block(
            ec,
            x,
            y,
            w,
            h,
            tree_type,
            chroma_ref_info,
            cfg,
            cdf,
            mi,
            blocks,
            state,
        );
        update_partition_context(state, tree_type, mi_col, mi_row, w, h);
        if was_tracking {
            state.sdp_luma_depth -= 1;
        }
        return;
    }

    let is_sb2_chroma = tree_type == TreeType::Chroma && x < 64 && y >= 64 && y < 128;
    if is_sb2_chroma {
        ec.trace_state(&format!(
            "[SB2C] decode_partition mi=({},{}) {}x{}",
            y / 4,
            x / 4,
            w,
            h
        ));
    }
    let is_sb3_luma = (tree_type == TreeType::Luma || tree_type == TreeType::Shared)
        && x >= 64
        && x < 128
        && y >= 64
        && y < 128;
    if is_sb3_luma {
        ec.trace_state(&format!(
            "[SB3L] decode_partition mi=({},{}) {}x{}",
            y / 4,
            x / 4,
            w,
            h
        ));
    }
    let partition = if tree_type == TreeType::Chroma && w == 64 && h == 64 {
        if let Some(luma_part) = state.sdp_luma_partition_64x64 {
            let chroma_part = sdp_chroma_part_from_luma(luma_part);
            eprintln!(
                "[PART] SDP_INHERIT {:?} (from luma {:?}) at ({},{}) {}x{}",
                chroma_part, luma_part, x, y, w, h
            );
            state.cfl_allowed_in_sdp = true;
            chroma_part
        } else {
            let p = read_partition_type(ec, w, h, tree_type, cdf, &above_part, &left_part, cfg);
            eprintln!(
                "[PART] READ {:?} at ({},{}) {}x{} tree={:?}",
                p, x, y, w, h, tree_type
            );
            state.cfl_allowed_in_sdp =
                is_cfl_allowed_for_sdp_partitions(state.sdp_luma_partition_for_cfl, p);
            eprintln!(
                "[CFL_SDP] luma={:?} chroma={:?} cfl_allowed={}",
                state.sdp_luma_partition_for_cfl, p, state.cfl_allowed_in_sdp
            );
            p
        }
    } else if let Some(implied) = implied_boundary_partition(x, y, w, h, frame_w, frame_h) {
        eprintln!(
            "[PART] IMPLIED {:?} at ({},{}) {}x{} tree={:?}",
            implied, x, y, w, h, tree_type
        );
        implied
    } else {
        let p = read_partition_type(ec, w, h, tree_type, cdf, &above_part, &left_part, cfg);
        eprintln!(
            "[PART] READ {:?} at ({},{}) {}x{} tree={:?}",
            p, x, y, w, h, tree_type
        );
        if tree_type == TreeType::Luma && w == 64 && h == 64 {
            state.sdp_luma_partition_64x64 = Some(p);
            state.sdp_luma_tracking = true;
            state.sdp_luma_depth = 0;
        }
        if state.sdp_luma_tracking && state.sdp_luma_depth == 1 {
            state.sdp_luma_sub_partitions.push(p);
        }
        p
    };
    if is_sb2_chroma {
        ec.trace_state(&format!(
            "[SB2C] partition={:?} mi=({},{}) {}x{}",
            partition,
            y / 4,
            x / 4,
            w,
            h
        ));
    }
    if is_sb3_luma {
        ec.trace_state(&format!(
            "[SB3L] partition={:?} mi=({},{}) {}x{}",
            partition,
            y / 4,
            x / 4,
            w,
            h
        ));
    }

    let child_chroma_ctx = ChromaRefContext {
        info: chroma_ref_info,
        partition,
        width: w,
        height: h,
    };

    match partition {
        PartitionType::None => {
            if is_sb2_chroma {
                ec.trace_state(&format!("[SB2C_LEAF] mi=({},{}) {}x{}", y / 4, x / 4, w, h));
            }
            if is_sb3_luma {
                ec.trace_state(&format!("[SB3L_LEAF] mi=({},{}) {}x{}", y / 4, x / 4, w, h));
            }
            decode_leaf_block(
                ec,
                x,
                y,
                w,
                h,
                tree_type,
                chroma_ref_info,
                cfg,
                cdf,
                mi,
                blocks,
                state,
            );
            if is_sb2_chroma {
                ec.trace_state(&format!(
                    "[SB2C_LEAF_DONE] mi=({},{}) {}x{}",
                    y / 4,
                    x / 4,
                    w,
                    h
                ));
            }
            if is_sb3_luma {
                ec.trace_state(&format!(
                    "[SB3L_LEAF_DONE] mi=({},{}) {}x{}",
                    y / 4,
                    x / 4,
                    w,
                    h
                ));
            }
            update_partition_context(state, tree_type, mi_col, mi_row, w, h);
        }
        PartitionType::Split => {
            let hw = w / 2;
            let hh = h / 2;
            decode_partition(
                ec,
                x,
                y,
                hw,
                hh,
                frame_w,
                frame_h,
                tree_type,
                0,
                Some(child_chroma_ctx),
                cfg,
                cdf,
                mi,
                blocks,
                state,
            );
            decode_partition(
                ec,
                x + hw,
                y,
                hw,
                hh,
                frame_w,
                frame_h,
                tree_type,
                1,
                Some(child_chroma_ctx),
                cfg,
                cdf,
                mi,
                blocks,
                state,
            );
            decode_partition(
                ec,
                x,
                y + hh,
                hw,
                hh,
                frame_w,
                frame_h,
                tree_type,
                2,
                Some(child_chroma_ctx),
                cfg,
                cdf,
                mi,
                blocks,
                state,
            );
            decode_partition(
                ec,
                x + hw,
                y + hh,
                hw,
                hh,
                frame_w,
                frame_h,
                tree_type,
                3,
                Some(child_chroma_ctx),
                cfg,
                cdf,
                mi,
                blocks,
                state,
            );
        }
        PartitionType::Horz => {
            let hh = h / 2;
            decode_partition(
                ec,
                x,
                y,
                w,
                hh,
                frame_w,
                frame_h,
                tree_type,
                0,
                Some(child_chroma_ctx),
                cfg,
                cdf,
                mi,
                blocks,
                state,
            );
            decode_partition(
                ec,
                x,
                y + hh,
                w,
                hh,
                frame_w,
                frame_h,
                tree_type,
                1,
                Some(child_chroma_ctx),
                cfg,
                cdf,
                mi,
                blocks,
                state,
            );
        }
        PartitionType::Vert => {
            let hw = w / 2;
            decode_partition(
                ec,
                x,
                y,
                hw,
                h,
                frame_w,
                frame_h,
                tree_type,
                0,
                Some(child_chroma_ctx),
                cfg,
                cdf,
                mi,
                blocks,
                state,
            );
            decode_partition(
                ec,
                x + hw,
                y,
                hw,
                h,
                frame_w,
                frame_h,
                tree_type,
                1,
                Some(child_chroma_ctx),
                cfg,
                cdf,
                mi,
                blocks,
                state,
            );
        }
        _ => {
            let subs = subblock_offsets(partition, w, h);
            for (child_index, (dx, dy, sw, sh)) in subs.into_iter().enumerate() {
                if sw > 0 && sh > 0 {
                    decode_partition(
                        ec,
                        x + dx,
                        y + dy,
                        sw,
                        sh,
                        frame_w,
                        frame_h,
                        tree_type,
                        child_index,
                        Some(child_chroma_ctx),
                        cfg,
                        cdf,
                        mi,
                        blocks,
                        state,
                    );
                }
            }
        }
    }

    if was_tracking {
        state.sdp_luma_depth -= 1;
    }
}

fn read_tx_partition(
    ec: &mut DaalaDecoder,
    block_w: usize,
    block_h: usize,
    fsc_mode: usize,
    reduced_tx_part_set: bool,
    cdf: &mut FrameCdfContext,
) -> TxPartitionType {
    if coding_block_disallows_tx_partitioning(block_w, block_h) {
        return TxPartitionType::None;
    }

    let max_tx_w = block_w.min(64);
    let max_tx_h = block_h.min(64);

    let allow_horz = {
        let sub_h = max_tx_h >> 1;
        sub_h >= 4 && is_valid_tx_size(max_tx_w, sub_h)
    };
    let allow_vert = {
        let sub_w = max_tx_w >> 1;
        sub_w >= 4 && is_valid_tx_size(sub_w, max_tx_h)
    };

    if !allow_horz && !allow_vert {
        return TxPartitionType::None;
    }

    let bsize_group = tx_part_group(block_w, block_h);
    if bsize_group >= TXFM_SPLIT_GROUP {
        return TxPartitionType::None;
    }

    let is_fsc = if fsc_mode != 0 { 1 } else { 0 };
    let is_inter = 0;

    let do_partition =
        ec.decode_symbol_mut(&mut cdf.txfm_do_partition[is_fsc][is_inter][bsize_group]);
    ec.trace_state(&format!(
        "after_txfm_do_partition={} group={} bsize={}x{}",
        do_partition, bsize_group, block_w, block_h
    ));
    if do_partition == 0 {
        return TxPartitionType::None;
    }

    if allow_horz && allow_vert {
        let txsize_group = tx_type_group_vert_and_horz(block_w, block_h);
        if txsize_group < 255 {
            let partition_type = ec.decode_symbol_mut(
                &mut cdf.txfm_4way_partition_type[is_fsc][is_inter][txsize_group],
            );
            ec.trace_state(&format!(
                "after_txfm_4way_type={} tsg={}",
                partition_type, txsize_group
            ));
            match partition_type {
                0 => TxPartitionType::Split,
                1 => TxPartitionType::Horz,
                2 => TxPartitionType::Vert,
                3 => TxPartitionType::Horz4,
                4 => TxPartitionType::Vert4,
                _ => TxPartitionType::Split,
            }
        } else {
            TxPartitionType::Split
        }
    } else {
        let txsize_group = tx_type_group_vert_or_horz(block_w, block_h);
        if txsize_group > 0 && !reduced_tx_part_set {
            let partition_type = ec.decode_symbol_mut(
                &mut cdf.txfm_2or3_way_partition_type[is_fsc][is_inter][txsize_group - 1],
            );
            ec.trace_state(&format!(
                "after_txfm_2or3way_type={} tsg={}",
                partition_type, txsize_group
            ));
            if allow_horz {
                if partition_type == 0 {
                    TxPartitionType::Horz
                } else {
                    TxPartitionType::Horz4
                }
            } else {
                if partition_type == 0 {
                    TxPartitionType::Vert
                } else {
                    TxPartitionType::Vert4
                }
            }
        } else {
            if allow_horz {
                TxPartitionType::Horz
            } else {
                TxPartitionType::Vert
            }
        }
    }
}

fn get_tx_partition_sub_tus(
    partition: TxPartitionType,
    max_tx_w: usize,
    max_tx_h: usize,
) -> Vec<(TxSize, usize, usize)> {
    match partition {
        TxPartitionType::None => {
            vec![(
                crate::block::max_txsize_rect_for_block(max_tx_w, max_tx_h),
                0,
                0,
            )]
        }
        TxPartitionType::Split => {
            let hw = max_tx_w / 2;
            let hh = max_tx_h / 2;
            let sub = crate::block::max_txsize_rect_for_block(hw, hh);
            vec![(sub, 0, 0), (sub, 0, hw), (sub, hh, 0), (sub, hh, hw)]
        }
        TxPartitionType::Horz => {
            let hh = max_tx_h / 2;
            let sub = crate::block::max_txsize_rect_for_block(max_tx_w, hh);
            vec![(sub, 0, 0), (sub, hh, 0)]
        }
        TxPartitionType::Vert => {
            let hw = max_tx_w / 2;
            let sub = crate::block::max_txsize_rect_for_block(hw, max_tx_h);
            vec![(sub, 0, 0), (sub, 0, hw)]
        }
        TxPartitionType::Horz4 => {
            let qh = max_tx_h / 4;
            let sub = crate::block::max_txsize_rect_for_block(max_tx_w, qh);
            vec![
                (sub, 0, 0),
                (sub, qh, 0),
                (sub, 2 * qh, 0),
                (sub, 3 * qh, 0),
            ]
        }
        TxPartitionType::Vert4 => {
            let qw = max_tx_w / 4;
            let sub = crate::block::max_txsize_rect_for_block(qw, max_tx_h);
            vec![
                (sub, 0, 0),
                (sub, 0, qw),
                (sub, 0, 2 * qw),
                (sub, 0, 3 * qw),
            ]
        }
    }
}

const TXFM_SPLIT_GROUP: usize = 9;

fn coding_block_disallows_tx_partitioning(w: usize, h: usize) -> bool {
    (w >= 64 && h >= 128) || (w >= 128 && h >= 64)
}

fn is_valid_tx_size(w: usize, h: usize) -> bool {
    matches!(
        (w, h),
        (4, 4)
            | (8, 8)
            | (16, 16)
            | (32, 32)
            | (64, 64)
            | (4, 8)
            | (8, 4)
            | (8, 16)
            | (16, 8)
            | (16, 32)
            | (32, 16)
            | (32, 64)
            | (64, 32)
            | (4, 16)
            | (16, 4)
            | (8, 32)
            | (32, 8)
            | (16, 64)
            | (64, 16)
    )
}

fn tx_part_group(w: usize, h: usize) -> usize {
    match (w, h) {
        (4, 4) | (4, 8) | (8, 4) => 0,
        (8, 8) => 1,
        (8, 16) | (16, 8) => 2,
        (16, 16) => 3,
        (16, 32) | (32, 16) => 4,
        (32, 32) => 5,
        (32, 64) | (64, 32) => 6,
        (64, 64) | (64, 128) | (128, 64) | (128, 128) => 7,
        (4, 16)
        | (16, 4)
        | (8, 32)
        | (32, 8)
        | (16, 64)
        | (64, 16)
        | (4, 32)
        | (32, 4)
        | (8, 64)
        | (64, 8) => 8,
        _ => TXFM_SPLIT_GROUP,
    }
}

fn tx_type_group_vert_and_horz(w: usize, h: usize) -> usize {
    match (w, h) {
        (8, 8) => 0,
        (8, 16) => 1,
        (16, 8) => 2,
        (16, 16) => 3,
        (16, 32) => 4,
        (32, 16) => 5,
        (32, 32) => 6,
        (32, 64) => 7,
        (64, 32) => 8,
        (64, 64) | (64, 128) | (128, 64) | (128, 128) => 9,
        (8, 32) | (8, 64) => 10,
        (32, 8) | (64, 8) => 11,
        (16, 64) => 12,
        (64, 16) => 13,
        _ => 255,
    }
}

fn tx_type_group_vert_or_horz(w: usize, h: usize) -> usize {
    match (w, h) {
        (4, 4) | (4, 8) | (8, 4) => 0,
        (4, 16) | (4, 32) => 1,
        (16, 4) | (32, 4) => 2,
        _ => 0,
    }
}

fn allow_fsc_intra(block_w: usize, block_h: usize) -> bool {
    block_w >= 4 && block_h >= 4 && block_w <= 32 && block_h <= 32
}

fn fsc_bsize_group(w: usize, h: usize) -> usize {
    match (w, h) {
        (4, 4) => 0,
        (4, 8) | (8, 4) => 1,
        (8, 8) => 2,
        (8, 16) | (16, 8) | (4, 16) | (16, 4) => 3,
        (16, 16) | (8, 32) | (32, 8) | (4, 32) | (32, 4) => 4,
        (16, 32) | (32, 16) | (32, 32) => 5,
        _ => unreachable!("unexpected block size for FSC: {}x{}", w, h),
    }
}

pub fn decode_tile_data_partitioned(
    data: &[u8],
    width: usize,
    height: usize,
    sb_size: usize,
) -> Vec<DecodedBlock> {
    decode_tile_data_partitioned_opts(data, width, height, sb_size, false, 0)
}

pub fn decode_tile_data_partitioned_opts(
    data: &[u8],
    width: usize,
    height: usize,
    sb_size: usize,
    enable_cfl_intra: bool,
    base_qindex: u8,
) -> Vec<DecodedBlock> {
    decode_tile_data_partitioned_cfg(
        data,
        width,
        height,
        sb_size,
        base_qindex,
        &TileDecodeConfig {
            enable_cfl_intra,
            ..TileDecodeConfig::default()
        },
    )
}

pub fn decode_tile_data_partitioned_cfg(
    data: &[u8],
    width: usize,
    height: usize,
    sb_size: usize,
    base_qindex: u8,
    cfg: &TileDecodeConfig,
) -> Vec<DecodedBlock> {
    let mut ec = DaalaDecoder::new(data);
    let mut cdf = FrameCdfContext::new_with_qindex(base_qindex);
    let mut mi = ModeInfoGrid::new(width, height, cfg.sb_size);
    let mut blocks = Vec::new();

    let mi_w_y = (width + 3) / 4;
    let mi_w_uv = (width / 2 + 3) / 4;
    let sb_mi_h_y = sb_size / 4;
    let sb_mi_h_uv = sb_size / 2 / 4;

    let max_mib_size = 64;
    let mut state = TileState {
        cdef_read: HashSet::new(),
        ccso_read: [HashSet::new(), HashSet::new(), HashSet::new()],
        above_ctx: [vec![0u8; mi_w_y], vec![0u8; mi_w_uv], vec![0u8; mi_w_uv]],
        left_ctx: [
            vec![0u8; sb_mi_h_y],
            vec![0u8; sb_mi_h_uv],
            vec![0u8; sb_mi_h_uv],
        ],
        above_part_ctx: [vec![0u8; mi_w_y], vec![0u8; mi_w_y]],
        left_part_ctx: [vec![0u8; max_mib_size], vec![0u8; max_mib_size]],
        sb_row_y: 0,
        sdp_luma_partition_64x64: None,
        sdp_luma_sub_partitions: Vec::new(),
        sdp_luma_tracking: false,
        sdp_luma_depth: 0,
        cfl_allowed_in_sdp: true,
        sdp_luma_partition_for_cfl: None,
    };

    let sb_cols = (width + sb_size - 1) / sb_size;
    let sb_rows = (height + sb_size - 1) / sb_size;

    for sb_row in 0..sb_rows {
        state.sb_row_y = sb_row * sb_size;
        state.left_ctx[0].fill(0);
        state.left_ctx[1].fill(0);
        state.left_ctx[2].fill(0);
        state.left_part_ctx[0].fill(0);
        state.left_part_ctx[1].fill(0);
        mi.reset_intrabc_refmv_bank_for_row();

        for sb_col in 0..sb_cols {
            let x = sb_col * sb_size;
            let y = sb_row * sb_size;
            mi.reset_intrabc_refmv_bank_for_sb();
            ec.trace_state(&format!(
                "SB_START pixel=({},{}) sb_row={} sb_col={}",
                x, y, sb_row, sb_col
            ));
            decode_partition(
                &mut ec,
                x,
                y,
                sb_size,
                sb_size,
                width,
                height,
                TreeType::Shared,
                0,
                None,
                cfg,
                &mut cdf,
                &mut mi,
                &mut blocks,
                &mut state,
            );
        }
    }

    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intrabc_default_ref_bvs_allow_duplicates() {
        let mut mi = ModeInfoGrid::new(128, 128, 64);
        mi.fill_intrabc(116, 76, 4, 4, false, false, -32, 0);

        let cands = mi.intrabc_ref_bv_candidates(120, 76, 8, 4, false, 3);

        assert_eq!(cands, vec![(-32, 0), (-512, 0), (0, -2560), (-32, 0)]);
    }

    #[test]
    fn chroma_tx_type_tracks_uv_mode() {
        assert_eq!(get_chroma_tx_type(TxSize::Tx8x8, 1, false, 0), ADST_DCT);
        assert_eq!(get_chroma_tx_type(TxSize::Tx8x8, 2, false, 0), DCT_ADST);
        assert_eq!(get_chroma_tx_type(TxSize::Tx8x8, 13, false, 0), DCT_DCT);
    }

    #[test]
    fn chroma_tx_type_respects_rect_constraints() {
        assert_eq!(get_chroma_tx_type(TxSize::Tx8x32, 1, false, 0), DCT_DCT);
        assert_eq!(get_chroma_tx_type(TxSize::Tx32x8, 2, false, 0), DCT_DCT);
    }

    #[test]
    fn chroma_tx_type_respects_reduced_tx_set() {
        assert_eq!(get_chroma_tx_type(TxSize::Tx8x8, 1, false, 2), DCT_DCT);
    }

    #[test]
    fn chroma_ref_info_merges_horz_sub8_blocks() {
        let parent_ctx = ChromaRefContext {
            info: ChromaRefInfo::new(0, 0, 8, 8),
            partition: PartitionType::Horz,
            width: 8,
            height: 8,
        };

        let top = compute_chroma_ref_info(TreeType::Shared, 0, 0, 8, 4, 0, Some(parent_ctx));
        let bottom = compute_chroma_ref_info(TreeType::Shared, 0, 4, 8, 4, 1, Some(parent_ctx));

        assert!(!top.is_chroma_ref);
        assert!(bottom.is_chroma_ref);
        assert_eq!(
            (top.x_base, top.y_base, top.w_base, top.h_base),
            (0, 0, 8, 8)
        );
        assert_eq!(
            (bottom.x_base, bottom.y_base, bottom.w_base, bottom.h_base),
            (0, 0, 8, 8)
        );
    }

    #[test]
    fn chroma_ref_info_tracks_horz3_special_middle_pair() {
        let parent_ctx = ChromaRefContext {
            info: ChromaRefInfo::new(0, 0, 8, 32),
            partition: PartitionType::Horz3,
            width: 8,
            height: 32,
        };

        let left_mid = compute_chroma_ref_info(TreeType::Shared, 0, 8, 4, 16, 1, Some(parent_ctx));
        let right_mid = compute_chroma_ref_info(TreeType::Shared, 4, 8, 4, 16, 2, Some(parent_ctx));

        assert!(!left_mid.is_chroma_ref);
        assert!(right_mid.is_chroma_ref);
        assert_eq!(
            (
                left_mid.x_base,
                left_mid.y_base,
                left_mid.w_base,
                left_mid.h_base
            ),
            (0, 8, 8, 16)
        );
        assert_eq!(
            (
                right_mid.x_base,
                right_mid.y_base,
                right_mid.w_base,
                right_mid.h_base
            ),
            (0, 8, 8, 16)
        );
    }
}
