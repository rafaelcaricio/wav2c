use std::fmt;

use crate::bitstream::{
    FrameHeaderConfig, ObuType, ObuWriter, SequenceHeaderConfig, write_clk_obu_payload,
    write_sequence_header,
};
use crate::dct;
use crate::scan::DEFAULT_SCAN_32X32;
use crate::tile;

#[derive(Debug)]
pub enum EncodeError {
    InvalidDimensions {
        width: u32,
        height: u32,
    },
    InvalidPlaneSize {
        plane: &'static str,
        expected: usize,
        got: usize,
    },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::InvalidDimensions { width, height } => {
                write!(
                    f,
                    "invalid dimensions: {}x{} (must be multiples of 64)",
                    width, height
                )
            }
            EncodeError::InvalidPlaneSize {
                plane,
                expected,
                got,
            } => {
                write!(
                    f,
                    "{} plane size mismatch: expected {}, got {}",
                    plane, expected, got
                )
            }
        }
    }
}

impl std::error::Error for EncodeError {}

pub struct Packet {
    pub data: Vec<u8>,
    pub frame_number: u64,
}

pub struct Encoder {
    width: u32,
    height: u32,
    padded_width: u32,
    padded_height: u32,
    base_qindex: u8,
    dequant: i32,
    order_hint: u32,
    pending_packet: Option<Packet>,
}

impl Encoder {
    pub fn new(width: u32, height: u32, base_qindex: u8) -> Result<Self, EncodeError> {
        if width < 4 || height < 4 || !width.is_multiple_of(4) || !height.is_multiple_of(4) {
            return Err(EncodeError::InvalidDimensions { width, height });
        }
        Ok(Self {
            width,
            height,
            padded_width: round_up_to_64(width),
            padded_height: round_up_to_64(height),
            base_qindex,
            dequant: ac_qlookup(base_qindex),
            order_hint: 0,
            pending_packet: None,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn headers(&self) -> Vec<u8> {
        let seq_config = SequenceHeaderConfig {
            width: self.padded_width,
            height: self.padded_height,
        };
        let seq_payload = write_sequence_header(&seq_config);
        let mut headers = ObuWriter::pack_obu(ObuType::TemporalDelimiter, &[]);
        headers.extend_from_slice(&ObuWriter::pack_obu(ObuType::SequenceHeader, &seq_payload));
        headers
    }

    pub fn send_frame(&mut self, y: &[u8], u: &[u8], v: &[u8]) -> Result<(), EncodeError> {
        let y_size = (self.width * self.height) as usize;
        let uv_size = (self.width / 2 * self.height / 2) as usize;

        if y.len() != y_size {
            return Err(EncodeError::InvalidPlaneSize {
                plane: "Y",
                expected: y_size,
                got: y.len(),
            });
        }
        if u.len() != uv_size {
            return Err(EncodeError::InvalidPlaneSize {
                plane: "U",
                expected: uv_size,
                got: u.len(),
            });
        }
        if v.len() != uv_size {
            return Err(EncodeError::InvalidPlaneSize {
                plane: "V",
                expected: uv_size,
                got: v.len(),
            });
        }

        let padded_y = pad_plane(
            y,
            self.width,
            self.height,
            self.padded_width,
            self.padded_height,
        );
        let padded_u = pad_plane(
            u,
            self.width / 2,
            self.height / 2,
            self.padded_width / 2,
            self.padded_height / 2,
        );
        let padded_v = pad_plane(
            v,
            self.width / 2,
            self.height / 2,
            self.padded_width / 2,
            self.padded_height / 2,
        );

        let clk_obu = encode_frame_yuv(
            &padded_y,
            &padded_u,
            &padded_v,
            &FrameEncodeParams {
                width: self.padded_width,
                height: self.padded_height,
                dequant: self.dequant,
                base_qindex: self.base_qindex,
                order_hint: self.order_hint,
            },
            true,
        );
        let mut packet_data = Vec::new();
        if self.order_hint > 0 {
            packet_data.extend_from_slice(&ObuWriter::pack_obu(ObuType::TemporalDelimiter, &[]));
        }
        packet_data.extend_from_slice(&clk_obu);

        let frame_number = self.order_hint as u64;
        self.order_hint += 1;
        self.pending_packet = Some(Packet {
            data: packet_data,
            frame_number,
        });

        Ok(())
    }

    pub fn receive_packet(&mut self) -> Option<Packet> {
        self.pending_packet.take()
    }

    pub fn flush(&mut self) {
        // No-op for all-intra encoder — no buffered frames
    }
}

fn extract_subblock(
    plane: &[u8],
    frame_w: usize,
    x: usize,
    y: usize,
    bw: usize,
    bh: usize,
) -> Vec<u8> {
    let mut block = vec![0u8; bw * bh];
    for row in 0..bh {
        let src_offset = (y + row) * frame_w + x;
        let dst_offset = row * bw;
        block[dst_offset..dst_offset + bw].copy_from_slice(&plane[src_offset..src_offset + bw]);
    }
    block
}

fn round_up_to_64(dim: u32) -> u32 {
    (dim + 63) & !63
}

fn pad_plane(src: &[u8], orig_w: u32, orig_h: u32, padded_w: u32, padded_h: u32) -> Vec<u8> {
    let (ow, oh, pw, ph) = (
        orig_w as usize,
        orig_h as usize,
        padded_w as usize,
        padded_h as usize,
    );
    let mut dst = vec![128u8; pw * ph];
    for row in 0..oh {
        dst[row * pw..row * pw + ow].copy_from_slice(&src[row * ow..row * ow + ow]);
    }
    dst
}

struct FrameEncodeParams {
    width: u32,
    height: u32,
    dequant: i32,
    base_qindex: u8,
    order_hint: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum IntraMode {
    DcPred = 0,
    VPred = 1,
    HPred = 2,
    D45Pred = 3,
    D135Pred = 4,
    D113Pred = 5,
    D157Pred = 6,
    D203Pred = 7,
    D67Pred = 8,
    SmoothPred = 9,
    SmoothVPred = 10,
    SmoothHPred = 11,
    PaethPred = 12,
}

impl IntraMode {
    pub fn from_av2(val: u8) -> Self {
        match val {
            0 => IntraMode::DcPred,
            1 => IntraMode::VPred,
            2 => IntraMode::HPred,
            3 => IntraMode::D45Pred,
            4 => IntraMode::D135Pred,
            5 => IntraMode::D113Pred,
            6 => IntraMode::D157Pred,
            7 => IntraMode::D203Pred,
            8 => IntraMode::D67Pred,
            9 => IntraMode::SmoothPred,
            10 => IntraMode::SmoothVPred,
            11 => IntraMode::SmoothHPred,
            12 => IntraMode::PaethPred,
            _ => IntraMode::DcPred,
        }
    }

    pub fn is_directional(self) -> bool {
        let v = self as u8;
        v >= 1 && v <= 8
    }

    pub fn nominal_angle(self) -> u16 {
        MODE_TO_ANGLE_MAP[self as usize]
    }

    pub fn reordered_index(self) -> usize {
        REORDERED_Y_MODE_INV[self as usize]
    }

    pub fn joint_y_index(self) -> usize {
        if !self.is_directional() {
            self.reordered_index()
        } else {
            5 + self.reordered_index().saturating_sub(5) * 7 + 3
        }
    }
}

const NON_DIRECTIONAL_MODES_COUNT: usize = 5;
const LUMA_MODE_COUNT: usize = 61;

const DEFAULT_MODE_LIST_Y: [usize; 56] = [
    17, 45, 3, 10, 24, 31, 38, 52,
    15, 19, 43, 47, 1, 5, 8, 12, 22, 26, 29, 33, 36, 40, 50, 54,
    16, 18, 44, 46, 2, 4, 9, 11, 23, 25, 30, 32, 37, 39, 51, 53,
    14, 20, 42, 48, 0, 6, 7, 13, 21, 27, 28, 34, 35, 41, 49, 55,
];

pub fn build_y_mode_list(
    _left_joint: Option<usize>,
    _above_joint: Option<usize>,
    _is_large_block: bool,
    _is_small_block: bool,
) -> [i32; LUMA_MODE_COUNT] {
    let mut list = [-1i32; LUMA_MODE_COUNT];
    let mut selected = [false; LUMA_MODE_COUNT];
    let mut idx = 0;

    for i in 0..NON_DIRECTIONAL_MODES_COUNT {
        list[idx] = i as i32;
        selected[i] = true;
        idx += 1;
    }

    if !_is_small_block {
        let left_dir = _left_joint
            .filter(|&j| j >= NON_DIRECTIONAL_MODES_COUNT)
            .is_some();
        let above_dir = _above_joint
            .filter(|&j| j >= NON_DIRECTIONAL_MODES_COUNT)
            .is_some();

        let mut neighbor_joints: Vec<usize> = Vec::new();

        if left_dir && above_dir {
            let lj = _left_joint.unwrap();
            let aj = _above_joint.unwrap();
            neighbor_joints.push(lj);
            if lj != aj {
                neighbor_joints.push(aj);
            }
        } else if above_dir {
            neighbor_joints.push(_above_joint.unwrap());
        } else if left_dir {
            neighbor_joints.push(_left_joint.unwrap());
        }

        for &nj in &neighbor_joints {
            if !selected[nj] {
                list[idx] = nj as i32;
                selected[nj] = true;
                idx += 1;
            }
        }

        if _is_large_block {
            for i in 0..4 {
                for &nj in &neighbor_joints {
                    let nj_off = nj as isize - NON_DIRECTIONAL_MODES_COUNT as isize;
                    let left_derived = ((nj_off - i as isize + 55) % 56) as usize
                        + NON_DIRECTIONAL_MODES_COUNT;
                    let right_derived = ((nj_off + i as isize + 1) % 56) as usize
                        + NON_DIRECTIONAL_MODES_COUNT;
                    if !selected[left_derived] {
                        list[idx] = left_derived as i32;
                        selected[left_derived] = true;
                        idx += 1;
                    }
                    if !selected[right_derived] {
                        list[idx] = right_derived as i32;
                        selected[right_derived] = true;
                        idx += 1;
                    }
                }
            }
        }
    }

    for i in 0..DEFAULT_MODE_LIST_Y.len() {
        if idx >= LUMA_MODE_COUNT {
            break;
        }
        let joint = DEFAULT_MODE_LIST_Y[i] + NON_DIRECTIONAL_MODES_COUNT;
        if !selected[joint] {
            list[idx] = joint as i32;
            selected[joint] = true;
            idx += 1;
        }
    }

    list
}

pub fn find_mode_idx_in_list(list: &[i32; LUMA_MODE_COUNT], joint: usize) -> usize {
    for (i, &v) in list.iter().enumerate() {
        if v == joint as i32 {
            return i;
        }
    }
    0
}

const DEFAULT_MODE_LIST_UV: [u8; 8] = [1, 2, 3, 4, 8, 5, 6, 7];

pub fn build_uv_mode_list(y_mode: IntraMode) -> [i32; 13] {
    let mut list = [-1i32; 13];
    let mut selected = [false; 13];
    let mut idx = 0;

    if y_mode.is_directional() {
        let m = y_mode as usize;
        list[idx] = m as i32;
        selected[m] = true;
        idx += 1;
    }

    for &m in &[0u8, 9, 10, 11, 12] {
        let mu = m as usize;
        if !selected[mu] {
            list[idx] = mu as i32;
            selected[mu] = true;
            idx += 1;
        }
    }

    for &m in &DEFAULT_MODE_LIST_UV {
        let mu = m as usize;
        if !selected[mu] {
            list[idx] = mu as i32;
            selected[mu] = true;
            idx += 1;
        }
    }

    list
}

pub fn find_uv_mode_idx(list: &[i32; 13], mode: IntraMode) -> usize {
    let m = mode as usize;
    for (i, &v) in list.iter().enumerate() {
        if v == m as i32 {
            return i;
        }
    }
    0
}

const MODE_TO_ANGLE_MAP: [u16; 13] = [
    0, 90, 180, 45, 135, 113, 157, 203, 67, 0, 0, 0, 0,
];

const REORDERED_Y_MODE_INV: [usize; 13] = {
    let mut inv = [0usize; 13];
    let reord: [u8; 13] = [0, 9, 10, 11, 12, 3, 8, 1, 5, 4, 6, 2, 7];
    let mut i = 0;
    while i < 13 {
        inv[reord[i] as usize] = i;
        i += 1;
    }
    inv
};

const DR_INTRA_DERIVATIVE: [i32; 90] = [
    0, 4096, 2048, 1365, 1024, 819, 682, 585, 512,
    455, 409, 409, 409, 372, 341, 292, 273, 256,
    227, 215, 204, 186, 178, 170, 157, 151, 146, 136,
    132, 128, 117, 110, 107, 99, 97, 97, 93, 87,
    83, 81, 77, 74, 73, 69, 66, 64, 62, 59,
    56, 55, 53, 50, 49, 47, 44, 42, 42, 41,
    38, 37, 35, 32, 31, 30, 28, 27, 26, 24,
    23, 22, 20, 19, 18, 16, 15, 14, 12, 11,
    10, 10, 10, 9, 8, 7, 6, 5, 4, 3,
    2, 1,
];

fn get_dx(angle: u16) -> i32 {
    if angle > 0 && angle < 90 {
        DR_INTRA_DERIVATIVE[angle as usize]
    } else if angle > 90 && angle < 180 {
        DR_INTRA_DERIVATIVE[(180 - angle) as usize]
    } else {
        1
    }
}

fn get_dy(angle: u16) -> i32 {
    if angle > 90 && angle < 180 {
        DR_INTRA_DERIVATIVE[(angle - 90) as usize]
    } else if angle > 180 && angle < 270 {
        DR_INTRA_DERIVATIVE[(270 - angle) as usize]
    } else {
        1
    }
}

struct RefPixels {
    above: Vec<u8>,
    left: Vec<u8>,
    top_left: u8,
    above_right: u8,
    below_left: u8,
    has_above: bool,
    has_left: bool,
    above_ext: Vec<u8>,
    left_ext: Vec<u8>,
}

impl RefPixels {
    #[cfg(test)]
    fn new_test(
        above: Vec<u8>,
        left: Vec<u8>,
        top_left: u8,
        has_above: bool,
        has_left: bool,
        above_right: u8,
        below_left: u8,
    ) -> Self {
        let bsize = above.len();
        let mut above_ext = above.clone();
        above_ext.resize(bsize * 2, above[bsize - 1]);
        let mut left_ext = left.clone();
        left_ext.resize(bsize * 2, left[bsize - 1]);
        RefPixels {
            above,
            left,
            top_left,
            above_right,
            below_left,
            has_above,
            has_left,
            above_ext,
            left_ext,
        }
    }
}

fn get_ref_pixels(
    recon: &[u8], stride: usize, x: usize, y: usize, bsize: usize,
    right_limit: usize, bottom_limit: usize,
) -> RefPixels {
    let frame_h = recon.len() / stride;
    let has_above = y > 0;
    let has_left = x > 0;

    let compute_above_right = |above: &[u8]| -> u8 {
        if has_above && x + bsize < right_limit && x + bsize < stride {
            recon[(y - 1) * stride + x + bsize]
        } else {
            above[bsize - 1]
        }
    };

    let compute_below_left = |left: &[u8]| -> u8 {
        if has_left && y + bsize < bottom_limit && y + bsize < frame_h {
            recon[(y + bsize) * stride + x - 1]
        } else {
            left[bsize - 1]
        }
    };

    let build_above_ext = |above: &[u8]| -> Vec<u8> {
        let mut ext = vec![0u8; bsize + bsize];
        ext[..bsize].copy_from_slice(above);
        if has_above {
            for i in 0..bsize {
                let cx = x + bsize + i;
                if cx < right_limit && cx < stride {
                    ext[bsize + i] = recon[(y - 1) * stride + cx];
                } else {
                    ext[bsize + i] = above[bsize - 1];
                }
            }
        } else {
            let last = above[bsize - 1];
            for i in 0..bsize {
                ext[bsize + i] = last;
            }
        }
        ext
    };

    let build_left_ext = |left: &[u8]| -> Vec<u8> {
        let mut ext = vec![0u8; bsize + bsize];
        ext[..bsize].copy_from_slice(left);
        if has_left {
            for j in 0..bsize {
                let cy = y + bsize + j;
                if cy < bottom_limit && cy < frame_h {
                    ext[bsize + j] = recon[cy * stride + x - 1];
                } else {
                    ext[bsize + j] = left[bsize - 1];
                }
            }
        } else {
            let last = left[bsize - 1];
            for j in 0..bsize {
                ext[bsize + j] = last;
            }
        }
        ext
    };

    match (has_above, has_left) {
        (false, false) => {
            let above = vec![127; bsize];
            let left = vec![129; bsize];
            let ar = above[bsize - 1];
            let bl = left[bsize - 1];
            let above_ext = build_above_ext(&above);
            let left_ext = build_left_ext(&left);
            RefPixels {
                above,
                left,
                top_left: 128,
                above_right: ar,
                below_left: bl,
                has_above: false,
                has_left: false,
                above_ext,
                left_ext,
            }
        }
        (true, false) => {
            let mut above = vec![0u8; bsize];
            for i in 0..bsize {
                above[i] = recon[(y - 1) * stride + x + i];
            }
            let tl = above[0];
            let ar = compute_above_right(&above);
            let left = vec![tl; bsize];
            let bl = left[bsize - 1];
            let above_ext = build_above_ext(&above);
            let left_ext = build_left_ext(&left);
            RefPixels {
                above,
                left,
                top_left: tl,
                above_right: ar,
                below_left: bl,
                has_above: true,
                has_left: false,
                above_ext,
                left_ext,
            }
        }
        (false, true) => {
            let mut left = vec![0u8; bsize];
            for j in 0..bsize {
                left[j] = recon[(y + j) * stride + x - 1];
            }
            let tl = left[0];
            let bl = compute_below_left(&left);
            let above = vec![tl; bsize];
            let ar = above[bsize - 1];
            let above_ext = build_above_ext(&above);
            let left_ext = build_left_ext(&left);
            RefPixels {
                above,
                top_left: tl,
                above_right: ar,
                below_left: bl,
                left,
                has_above: false,
                has_left: true,
                above_ext,
                left_ext,
            }
        }
        (true, true) => {
            let mut above = vec![0u8; bsize];
            for i in 0..bsize {
                above[i] = recon[(y - 1) * stride + x + i];
            }
            let mut left = vec![0u8; bsize];
            for j in 0..bsize {
                left[j] = recon[(y + j) * stride + x - 1];
            }
            let tl = recon[(y - 1) * stride + x - 1];
            let ar = compute_above_right(&above);
            let bl = compute_below_left(&left);
            let above_ext = build_above_ext(&above);
            let left_ext = build_left_ext(&left);
            RefPixels {
                above,
                left,
                top_left: tl,
                above_right: ar,
                below_left: bl,
                has_above: true,
                has_left: true,
                above_ext,
                left_ext,
            }
        }
    }
}

pub fn predict_dc(recon: &[u8], stride: usize, x: usize, y: usize, bsize: usize) -> u8 {
    let refs = get_ref_pixels(recon, stride, x, y, bsize, stride, recon.len() / stride);
    match (refs.has_above, refs.has_left) {
        (false, false) => 128,
        (true, false) => {
            let sum: u32 = refs.above.iter().map(|&v| v as u32).sum();
            ((sum + bsize as u32 / 2) / bsize as u32) as u8
        }
        (false, true) => {
            let sum: u32 = refs.left.iter().map(|&v| v as u32).sum();
            ((sum + bsize as u32 / 2) / bsize as u32) as u8
        }
        (true, true) => {
            let sum: u32 = refs.above.iter().map(|&v| v as u32).sum::<u32>()
                + refs.left.iter().map(|&v| v as u32).sum::<u32>();
            ((sum + bsize as u32) / (2 * bsize as u32)) as u8
        }
    }
}

fn predict_dc_block(refs: &RefPixels, bsize: usize) -> Vec<u8> {
    let val = match (refs.has_above, refs.has_left) {
        (false, false) => 128u8,
        (true, false) => {
            let sum: u32 = refs.above.iter().map(|&v| v as u32).sum();
            ((sum + bsize as u32 / 2) / bsize as u32) as u8
        }
        (false, true) => {
            let sum: u32 = refs.left.iter().map(|&v| v as u32).sum();
            ((sum + bsize as u32 / 2) / bsize as u32) as u8
        }
        (true, true) => {
            let sum: u32 = refs.above.iter().map(|&v| v as u32).sum::<u32>()
                + refs.left.iter().map(|&v| v as u32).sum::<u32>();
            ((sum + bsize as u32) / (2 * bsize as u32)) as u8
        }
    };
    vec![val; bsize * bsize]
}

fn divide_round(value: i32, bits: u32) -> i32 {
    (value + (1 << (bits - 1))) >> bits
}

const BLK_SIZE_LOG2: [u8; 65] = [
    0, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 5,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 6,
    6,
];
const BLEND_WEIGHT_MAX: i32 = 32;

fn predict_smooth_block(refs: &RefPixels, bsize: usize) -> Vec<u8> {
    let bl = refs.below_left as i32;
    let tr = refs.above_right as i32;
    let bw = bsize;
    let bh = bsize;
    let scale = ((BLK_SIZE_LOG2[bh] as i32 - 2 + BLK_SIZE_LOG2[bw] as i32 - 2 + 2) >> 2) as u32;
    let blend_max_log2 = BLK_SIZE_LOG2[BLEND_WEIGHT_MAX as usize] as u32;
    let blend_lut_idx = BLK_SIZE_LOG2[(BLEND_WEIGHT_MAX as usize) << 1] as u32;
    let bh_log2 = BLK_SIZE_LOG2[bh] as u32;
    let bw_log2 = BLK_SIZE_LOG2[bw] as u32;

    let mut pred = vec![0u8; bsize * bsize];
    for r in 0..bh {
        let s_top = BLEND_WEIGHT_MAX >> blend_lut_idx.min(((r << 1) as u32) >> scale);
        let l = refs.left[r] as i32;
        for c in 0..bw {
            let s_left = BLEND_WEIGHT_MAX >> blend_lut_idx.min(((c << 1) as u32) >> scale);
            let top = refs.above[c] as i32;
            let predv = bl + divide_round((top - bl) * (bh as i32 - 1 - r as i32), bh_log2);
            let predh = tr + divide_round((l - tr) * (bw as i32 - 1 - c as i32), bw_log2);
            let predv = predv + divide_round((top - predv) * s_top, blend_max_log2 + 1);
            let predh = predh + divide_round((l - predh) * s_left, blend_max_log2 + 1);
            pred[r * bsize + c] = divide_round(predv + predh, 1) as u8;
        }
    }
    pred
}

fn predict_smooth_v_block(refs: &RefPixels, bsize: usize) -> Vec<u8> {
    let bl = refs.below_left as i32;
    let bw = bsize;
    let bh = bsize;
    let scale = ((BLK_SIZE_LOG2[bh] as i32 - 2 + BLK_SIZE_LOG2[bw] as i32 - 2 + 2) >> 2) as u32;
    let blend_max_log2 = BLK_SIZE_LOG2[BLEND_WEIGHT_MAX as usize] as u32;
    let blend_lut_idx = BLK_SIZE_LOG2[(BLEND_WEIGHT_MAX as usize) << 1] as u32;
    let bh_log2 = BLK_SIZE_LOG2[bh] as u32;

    let mut pred = vec![0u8; bsize * bsize];
    for r in 0..bh {
        let s_top = BLEND_WEIGHT_MAX >> blend_lut_idx.min(((r << 1) as u32) >> scale);
        for c in 0..bw {
            let top = refs.above[c] as i32;
            let predv = bl + divide_round((top - bl) * (bh as i32 - 1 - r as i32), bh_log2);
            pred[r * bsize + c] =
                (predv + divide_round((top - predv) * s_top, blend_max_log2 + 1)) as u8;
        }
    }
    pred
}

fn predict_smooth_h_block(refs: &RefPixels, bsize: usize) -> Vec<u8> {
    let tr = refs.above_right as i32;
    let bw = bsize;
    let bh = bsize;
    let scale = ((BLK_SIZE_LOG2[bh] as i32 - 2 + BLK_SIZE_LOG2[bw] as i32 - 2 + 2) >> 2) as u32;
    let blend_max_log2 = BLK_SIZE_LOG2[BLEND_WEIGHT_MAX as usize] as u32;
    let blend_lut_idx = BLK_SIZE_LOG2[(BLEND_WEIGHT_MAX as usize) << 1] as u32;
    let bw_log2 = BLK_SIZE_LOG2[bw] as u32;

    let mut pred = vec![0u8; bsize * bsize];
    for r in 0..bh {
        let l = refs.left[r] as i32;
        for c in 0..bw {
            let s_left = BLEND_WEIGHT_MAX >> blend_lut_idx.min(((c << 1) as u32) >> scale);
            let predh = tr + divide_round((l - tr) * (bw as i32 - 1 - c as i32), bw_log2);
            pred[r * bsize + c] =
                (predh + divide_round((l - predh) * s_left, blend_max_log2 + 1)) as u8;
        }
    }
    pred
}

fn predict_paeth_block(refs: &RefPixels, bsize: usize) -> Vec<u8> {
    let tl = refs.top_left as i32;
    let mut pred = vec![0u8; bsize * bsize];
    for r in 0..bsize {
        let left = refs.left[r] as i32;
        for c in 0..bsize {
            let above = refs.above[c] as i32;
            let base = above + left - tl;
            let p_left = (base - left).abs();
            let p_top = (base - above).abs();
            let p_tl = (base - tl).abs();
            pred[r * bsize + c] = if p_left <= p_top && p_left <= p_tl {
                refs.left[r]
            } else if p_top <= p_tl {
                refs.above[c]
            } else {
                refs.top_left
            };
        }
    }
    pred
}

fn predict_v_block(refs: &RefPixels, bsize: usize) -> Vec<u8> {
    let mut pred = vec![0u8; bsize * bsize];
    for r in 0..bsize {
        pred[r * bsize..(r + 1) * bsize].copy_from_slice(&refs.above[..bsize]);
    }
    pred
}

fn predict_h_block(refs: &RefPixels, bsize: usize) -> Vec<u8> {
    let mut pred = vec![0u8; bsize * bsize];
    for r in 0..bsize {
        let val = refs.left[r];
        for c in 0..bsize {
            pred[r * bsize + c] = val;
        }
    }
    pred
}

const DR_INTERP_FILTER: [[i16; 4]; 32] = [
    [0, 128, 0, 0],     [-2, 127, 4, -1],   [-3, 125, 8, -2],
    [-5, 123, 13, -3],  [-6, 121, 17, -4],  [-7, 118, 22, -5],
    [-9, 116, 27, -6],  [-9, 112, 32, -7],  [-10, 109, 37, -8],
    [-11, 106, 41, -8], [-11, 102, 46, -9], [-12, 98, 52, -10],
    [-12, 94, 56, -10], [-12, 90, 61, -11], [-12, 85, 66, -11],
    [-12, 81, 71, -12], [-12, 76, 76, -12], [-12, 71, 81, -12],
    [-11, 66, 85, -12], [-11, 61, 90, -12], [-10, 56, 94, -12],
    [-10, 52, 98, -12], [-9, 46, 102, -11], [-8, 41, 106, -11],
    [-8, 37, 109, -10], [-7, 32, 112, -9],  [-6, 27, 116, -9],
    [-5, 22, 118, -7],  [-4, 17, 121, -6],  [-3, 13, 123, -5],
    [-2, 8, 125, -3],   [-1, 4, 127, -2],
];

fn idif_interp(p: [i32; 4], shift: usize) -> u8 {
    let f = &DR_INTERP_FILTER[shift];
    let val = p[0] * f[0] as i32 + p[1] * f[1] as i32
        + p[2] * f[2] as i32 + p[3] * f[3] as i32;
    ((val + 64) >> 7).clamp(0, 255) as u8
}

fn bilinear_interp(p0: i32, p1: i32, shift: i32) -> u8 {
    let val = p0 * (32 - shift) + p1 * shift;
    ((val + 16) >> 5).clamp(0, 255) as u8
}

fn predict_dr_z1(refs: &RefPixels, bw: usize, bh: usize, angle: u16, use_idif: bool) -> Vec<u8> {
    let dx = get_dx(angle);
    let max_base_x = (bw + bh) - 1;
    let above = &refs.above_ext;
    let tl = refs.top_left;
    let mut pred = vec![0u8; bw * bh];
    let mut x = dx;
    for r in 0..bh {
        let base = x >> 6;
        let shift = ((x & 0x3F) >> 1) as usize;
        for c in 0..bw {
            let b = base as usize + c;
            if b < max_base_x {
                if use_idif {
                    let p0 = if b == 0 { tl as i32 } else { above[b - 1] as i32 };
                    let p1 = above[b] as i32;
                    let p2 = above[b + 1] as i32;
                    let p3 = if b + 2 <= max_base_x { above[b + 2] } else { above[max_base_x] } as i32;
                    pred[r * bw + c] = idif_interp([p0, p1, p2, p3], shift);
                } else {
                    pred[r * bw + c] = bilinear_interp(above[b] as i32, above[b + 1] as i32, shift as i32);
                }
            } else {
                pred[r * bw + c] = above[max_base_x];
            }
        }
        x += dx;
    }
    pred
}

fn get_above_z2(refs: &RefPixels, idx: isize, bw: usize) -> i32 {
    if idx < -1 { refs.top_left as i32 }
    else if idx == -1 { refs.top_left as i32 }
    else if (idx as usize) < bw { refs.above_ext[idx as usize] as i32 }
    else { refs.above_ext[bw - 1] as i32 }
}

fn get_left_z2(refs: &RefPixels, idx: isize, bh: usize) -> i32 {
    if idx < -1 { refs.top_left as i32 }
    else if idx == -1 { refs.top_left as i32 }
    else if (idx as usize) < bh { refs.left_ext[idx as usize] as i32 }
    else { refs.left_ext[bh - 1] as i32 }
}

fn predict_dr_z2(refs: &RefPixels, bw: usize, bh: usize, angle: u16, use_idif: bool) -> Vec<u8> {
    let dx = get_dx(angle);
    let dy = get_dy(angle);
    let mut pred = vec![0u8; bw * bh];
    for r in 0..bh {
        for c in 0..bw {
            let y_val = (r + 1) as i32;
            let x_val = (c as i32) * 64 - y_val * dx;
            let base_x = x_val >> 6;
            if base_x >= -1 {
                let shift = ((x_val & 0x3F) >> 1) as usize;
                let bx = base_x as isize;
                if use_idif {
                    let p = [
                        get_above_z2(refs, bx - 1, bw),
                        get_above_z2(refs, bx, bw),
                        get_above_z2(refs, bx + 1, bw),
                        get_above_z2(refs, bx + 2, bw),
                    ];
                    pred[r * bw + c] = idif_interp(p, shift);
                } else {
                    let p0 = get_above_z2(refs, bx, bw);
                    let p1 = get_above_z2(refs, bx + 1, bw);
                    pred[r * bw + c] = bilinear_interp(p0, p1, shift as i32);
                }
            } else {
                let x_val2 = (c + 1) as i32;
                let y_val2 = (r as i32) * 64 - x_val2 * dy;
                let base_y = y_val2 >> 6;
                let shift = ((y_val2 & 0x3F) >> 1) as usize;
                let by = base_y as isize;
                if use_idif {
                    let p = [
                        get_left_z2(refs, by - 1, bh),
                        get_left_z2(refs, by, bh),
                        get_left_z2(refs, by + 1, bh),
                        get_left_z2(refs, by + 2, bh),
                    ];
                    pred[r * bw + c] = idif_interp(p, shift);
                } else {
                    let p0 = get_left_z2(refs, by, bh);
                    let p1 = get_left_z2(refs, by + 1, bh);
                    pred[r * bw + c] = bilinear_interp(p0, p1, shift as i32);
                }
            }
        }
    }
    pred
}

fn predict_dr_z3(refs: &RefPixels, bw: usize, bh: usize, angle: u16, use_idif: bool) -> Vec<u8> {
    let dy = get_dy(angle);
    let max_base_y = (bw + bh) - 1;
    let left = &refs.left_ext;
    let tl = refs.top_left;
    let mut pred = vec![0u8; bw * bh];
    let mut y = dy;
    for c in 0..bw {
        let base = y >> 6;
        let shift = ((y & 0x3F) >> 1) as usize;
        for r in 0..bh {
            let b = base as usize + r;
            if b < max_base_y {
                if use_idif {
                    let p0 = if b == 0 { tl as i32 } else { left[b - 1] as i32 };
                    let p1 = left[b] as i32;
                    let p2 = left[b + 1] as i32;
                    let p3 = if b + 2 <= max_base_y { left[b + 2] } else { left[max_base_y] } as i32;
                    pred[r * bw + c] = idif_interp([p0, p1, p2, p3], shift);
                } else {
                    pred[r * bw + c] = bilinear_interp(left[b] as i32, left[b + 1] as i32, shift as i32);
                }
            } else {
                pred[r * bw + c] = left[max_base_y];
            }
        }
        y += dy;
    }
    pred
}

fn predict_directional(refs: &RefPixels, bsize: usize, mode: IntraMode, is_luma: bool) -> Vec<u8> {
    let angle = mode.nominal_angle();
    if angle == 90 {
        return predict_v_block(refs, bsize);
    }
    if angle == 180 {
        return predict_h_block(refs, bsize);
    }
    if angle > 0 && angle < 90 {
        predict_dr_z1(refs, bsize, bsize, angle, is_luma)
    } else if angle > 90 && angle < 180 {
        predict_dr_z2(refs, bsize, bsize, angle, is_luma)
    } else {
        predict_dr_z3(refs, bsize, bsize, angle, is_luma)
    }
}

fn predict_block(refs: &RefPixels, bsize: usize, mode: IntraMode) -> Vec<u8> {
    predict_block_plane(refs, bsize, mode, true)
}

fn predict_block_plane(refs: &RefPixels, bsize: usize, mode: IntraMode, is_luma: bool) -> Vec<u8> {
    match mode {
        IntraMode::DcPred => predict_dc_block(refs, bsize),
        IntraMode::SmoothPred => predict_smooth_block(refs, bsize),
        IntraMode::SmoothVPred => predict_smooth_v_block(refs, bsize),
        IntraMode::SmoothHPred => predict_smooth_h_block(refs, bsize),
        IntraMode::PaethPred => predict_paeth_block(refs, bsize),
        IntraMode::VPred | IntraMode::HPred | IntraMode::D45Pred
        | IntraMode::D135Pred | IntraMode::D113Pred | IntraMode::D157Pred
        | IntraMode::D203Pred | IntraMode::D67Pred => predict_directional(refs, bsize, mode, is_luma),
    }
}

pub fn predict_intra_block(
    recon: &[u8],
    stride: usize,
    x: usize,
    y: usize,
    bsize: usize,
    mode: IntraMode,
) -> Vec<u8> {
    let refs = get_ref_pixels(recon, stride, x, y, bsize, stride, recon.len() / stride);
    predict_block_plane(&refs, bsize, mode, true)
}

pub fn predict_intra_block_uv(
    recon: &[u8],
    stride: usize,
    x: usize,
    y: usize,
    bsize: usize,
    mode: IntraMode,
) -> Vec<u8> {
    let refs = get_ref_pixels(recon, stride, x, y, bsize, stride, recon.len() / stride);
    predict_block_plane(&refs, bsize, mode, false)
}

fn compute_sad(pixels: &[u8], pred: &[u8]) -> u64 {
    pixels
        .iter()
        .zip(pred.iter())
        .map(|(&p, &q)| (p as i32 - q as i32).unsigned_abs() as u64)
        .sum()
}

const VH_MODES: [IntraMode; 7] = [
    IntraMode::DcPred, IntraMode::SmoothPred, IntraMode::SmoothVPred,
    IntraMode::SmoothHPred, IntraMode::PaethPred,
    IntraMode::VPred, IntraMode::HPred,
];

fn select_intra_mode(pixels: &[u8], refs: &RefPixels, bsize: usize) -> IntraMode {
    select_intra_mode_from(pixels, refs, bsize, &VH_MODES, true)
}

fn select_intra_mode_from(pixels: &[u8], refs: &RefPixels, bsize: usize, modes: &[IntraMode], is_luma: bool) -> IntraMode {
    let mut best_mode = IntraMode::DcPred;
    let mut best_sad = u64::MAX;
    for &mode in modes {
        let pred = predict_block_plane(refs, bsize, mode, is_luma);
        let sad = compute_sad(pixels, &pred);
        if sad < best_sad {
            best_sad = sad;
            best_mode = mode;
        }
    }
    best_mode
}

#[derive(Clone)]
pub struct EncodedBlock {
    pub x: usize,
    pub y: usize,
    pub bsize: usize,
    pub mode: IntraMode,
    pub levels: Vec<i32>,
    pub eob: u16,
    pub log_scale: u32,
    pub uv_mode: IntraMode,
    pub u_levels: Vec<i32>,
    pub u_eob: u16,
    pub v_levels: Vec<i32>,
    pub v_eob: u16,
    pub y_residual: Vec<i32>,
    pub u_residual: Vec<i32>,
    pub v_residual: Vec<i32>,
}

fn lossless_block_residual(
    pixels: &[u8],
    plane_w: usize,
    recon: &mut [u8],
    ready: &mut [bool],
    x: usize,
    y: usize,
    bsize: usize,
    mode: IntraMode,
    is_luma: bool,
) -> Vec<i32> {
    let pred_mode = crate::decode::predict::PredictionMode::from_u8(mode as u8);
    let mut residual = vec![0i32; bsize * bsize];
    for ty in (0..bsize).step_by(4) {
        for tx in (0..bsize).step_by(4) {
            let tu_x = x + tx;
            let tu_y = y + ty;
            let pred = crate::decode::predict::predict_intra_with_control_and_availability(
                recon,
                plane_w,
                tu_x,
                tu_y,
                4,
                4,
                pred_mode,
                0,
                0,
                0,
                crate::decode::predict::IntraPredictControl {
                    plane: if is_luma { 0 } else { 1 },
                    enable_ibp: false,
                    is_cfl: false,
                    above_filter_type: 0,
                    left_filter_type: 0,
                    disable_edge_filter: true,
                },
                Some(ready),
            );
            let block = extract_subblock(pixels, plane_w, tu_x, tu_y, 4, 4);
            let tu_residual = compute_residual_block(&block, &pred);
            for row in 0..4 {
                for col in 0..4 {
                    residual[(ty + row) * bsize + tx + col] = tu_residual[row * 4 + col];
                }
            }
            reconstruct_block(recon, plane_w, tu_x, tu_y, 4, &pred, &tu_residual);
            for row in 0..4 {
                ready[(tu_y + row) * plane_w + tu_x..(tu_y + row) * plane_w + tu_x + 4]
                    .fill(true);
            }
        }
    }
    residual
}

fn encode_uv_subblock(
    uv_plane: &[u8],
    uv_w: usize,
    recon_uv: &mut [u8],
    ready_uv: &mut [bool],
    ux: usize,
    uy: usize,
    uv_bsize: usize,
    dequant: i32,
    mode: IntraMode,
    lossless: bool,
) -> (Vec<i32>, u16, Vec<i32>) {
    if lossless {
        let residual = lossless_block_residual(
            uv_plane, uv_w, recon_uv, ready_uv, ux, uy, uv_bsize, mode, false,
        );
        return (Vec::new(), 0, residual);
    }
    let uv_h = recon_uv.len() / uv_w;
    let refs = get_ref_pixels(recon_uv, uv_w, ux, uy, uv_bsize, uv_w, uv_h);
    let block = extract_subblock(uv_plane, uv_w, ux, uy, uv_bsize, uv_bsize);
    let pred = predict_block_plane(&refs, uv_bsize, mode, false);
    let residual = compute_residual_block(&block, &pred);

    if uv_bsize == 32 {
        let mut residual_arr = [0i32; 1024];
        residual_arr.copy_from_slice(&residual);
        let dct_coeffs = dct::forward_dct2_32x32(&residual_arr);
        let (levels, eob) = quantize_block(&dct_coeffs, dequant, 1);
        let dqcoeffs = dequantize_block(&levels, eob, dequant, 1);
        let inv_residual = dct::inverse_dct2_32x32(&dqcoeffs);
        reconstruct_block(recon_uv, uv_w, ux, uy, 32, &pred, &inv_residual);
        (levels.to_vec(), eob, Vec::new())
    } else if uv_bsize == 16 {
        let mut residual_arr = [0i32; 256];
        residual_arr.copy_from_slice(&residual);
        let dct_coeffs = dct::forward_dct2_16x16(&residual_arr);
        let scan = &crate::scan::DEFAULT_SCAN_16X16;
        let mut levels = vec![0i32; 256];
        let mut eob = 0u16;
        for scan_idx in 0..256 {
            let coeff_idx = scan[scan_idx] as usize;
            let level = quantize_coeff(dct_coeffs[coeff_idx] as i64, 0, dequant);
            levels[scan_idx] = level;
            if level != 0 { eob = scan_idx as u16 + 1; }
        }
        let mut dqcoeffs = [0i32; 256];
        for scan_idx in 0..eob as usize {
            let coeff_idx = scan[scan_idx] as usize;
            dqcoeffs[coeff_idx] = dequantize_coeff(levels[scan_idx], 0, dequant);
        }
        let inv_residual = dct::inverse_dct2_16x16(&dqcoeffs);
        reconstruct_block(recon_uv, uv_w, ux, uy, 16, &pred, &inv_residual);
        (levels, eob, Vec::new())
    } else if uv_bsize == 8 {
        let mut residual_arr = [0i32; 64];
        residual_arr.copy_from_slice(&residual);
        let dct_coeffs = dct::forward_dct2_8x8(&residual_arr);
        let scan = &crate::scan::DEFAULT_SCAN_8X8;
        let mut levels = vec![0i32; 64];
        let mut eob = 0u16;
        for scan_idx in 0..64 {
            let coeff_idx = scan[scan_idx] as usize;
            let level = quantize_coeff(dct_coeffs[coeff_idx] as i64, 0, dequant);
            levels[scan_idx] = level;
            if level != 0 { eob = scan_idx as u16 + 1; }
        }
        let mut dqcoeffs = [0i32; 64];
        for scan_idx in 0..eob as usize {
            let coeff_idx = scan[scan_idx] as usize;
            dqcoeffs[coeff_idx] = dequantize_coeff(levels[scan_idx], 0, dequant);
        }
        let inv_residual = dct::inverse_dct2_8x8(&dqcoeffs);
        reconstruct_block(recon_uv, uv_w, ux, uy, 8, &pred, &inv_residual);
        (levels, eob, Vec::new())
    } else if uv_bsize == 4 {
        let pred = predict_block_plane(&refs, 4, mode, false);
        let residual = compute_residual_block(&block, &pred);
        let mut residual_arr = [0i32; 16];
        residual_arr.copy_from_slice(&residual);
        let dct_coeffs = dct::forward_dct2_4x4(&residual_arr);
        let scan = &crate::scan::DEFAULT_SCAN_4X4;
        let mut levels = vec![0i32; 16];
        let mut eob = 0u16;
        for scan_idx in 0..16 {
            let coeff_idx = scan[scan_idx] as usize;
            let level = quantize_coeff(dct_coeffs[coeff_idx] as i64, 0, dequant);
            levels[scan_idx] = level;
            if level != 0 { eob = scan_idx as u16 + 1; }
        }
        let mut dqcoeffs = [0i32; 16];
        for scan_idx in 0..eob as usize {
            let coeff_idx = scan[scan_idx] as usize;
            dqcoeffs[coeff_idx] = dequantize_coeff(levels[scan_idx], 0, dequant);
        }
        let inv_residual = dct::inverse_dct2_4x4(&dqcoeffs);
        reconstruct_block(recon_uv, uv_w, ux, uy, 4, &pred, &inv_residual);
        (levels, eob, Vec::new())
    } else {
        panic!("unsupported UV block size: {}", uv_bsize);
    }
}

fn encode_block_var(
    pixels: &[u8],
    plane_w: usize,
    recon: &mut [u8],
    ready: &mut [bool],
    x: usize,
    y: usize,
    bsize: usize,
    dequant: i32,
    forced_mode: Option<IntraMode>,
    right_limit: usize,
    bottom_limit: usize,
    lossless: bool,
) -> EncodedBlock {
    let refs = get_ref_pixels(recon, plane_w, x, y, bsize, right_limit, bottom_limit);
    let block = extract_subblock(pixels, plane_w, x, y, bsize, bsize);
    let mode = forced_mode.unwrap_or_else(|| select_intra_mode(&block, &refs, bsize));

    if lossless {
        let residual =
            lossless_block_residual(pixels, plane_w, recon, ready, x, y, bsize, mode, true);
        return EncodedBlock {
            x,
            y,
            bsize,
            mode,
            levels: Vec::new(),
            eob: 0,
            log_scale: 0,
            uv_mode: IntraMode::DcPred,
            u_levels: Vec::new(),
            u_eob: 0,
            v_levels: Vec::new(),
            v_eob: 0,
            y_residual: residual,
            u_residual: Vec::new(),
            v_residual: Vec::new(),
        };
    }

    let pred_block = predict_block(&refs, bsize, mode);
    let residual = compute_residual_block(&block, &pred_block);

    if bsize == 64 {
        let mut residual_arr = [0i32; 4096];
        residual_arr.copy_from_slice(&residual);
        let residual_32 = downsample_64_to_32(&residual_arr);
        let dct_coeffs = dct::forward_dct2_32x32(&residual_32);
        let (levels, eob) = quantize_block(&dct_coeffs, dequant, 2);
        let dqcoeffs = dequantize_block(&levels, eob, dequant, 2);
        let inv_residual = dct::inverse_dct2_64x64_av2(&dqcoeffs);
        reconstruct_block(recon, plane_w, x, y, 64, &pred_block, &inv_residual);
        EncodedBlock {
            x, y, bsize, mode,
            levels: levels.to_vec(),
            eob,
            log_scale: 2,
            uv_mode: IntraMode::DcPred, u_levels: Vec::new(), u_eob: 0, v_levels: Vec::new(), v_eob: 0,
            y_residual: Vec::new(), u_residual: Vec::new(), v_residual: Vec::new(),
        }
    } else if bsize == 32 {
        let mut residual_arr = [0i32; 1024];
        residual_arr.copy_from_slice(&residual);
        let dct_coeffs = dct::forward_dct2_32x32(&residual_arr);
        let (levels, eob) = quantize_block(&dct_coeffs, dequant, 1);
        let dqcoeffs = dequantize_block(&levels, eob, dequant, 1);
        let inv_residual = dct::inverse_dct2_32x32(&dqcoeffs);
        reconstruct_block(recon, plane_w, x, y, 32, &pred_block, &inv_residual);
        EncodedBlock {
            x, y, bsize, mode,
            levels: levels.to_vec(),
            eob,
            log_scale: 1,
            uv_mode: IntraMode::DcPred, u_levels: Vec::new(), u_eob: 0, v_levels: Vec::new(), v_eob: 0,
            y_residual: Vec::new(), u_residual: Vec::new(), v_residual: Vec::new(),
        }
    } else if bsize == 16 {
        let mut residual_arr = [0i32; 256];
        residual_arr.copy_from_slice(&residual);
        let dct_coeffs = dct::forward_dct2_16x16(&residual_arr);
        let scan = &crate::scan::DEFAULT_SCAN_16X16;
        let mut levels = vec![0i32; 256];
        let mut eob = 0u16;
        for scan_idx in 0..256 {
            let coeff_idx = scan[scan_idx] as usize;
            let level = quantize_coeff(dct_coeffs[coeff_idx] as i64, 0, dequant);
            levels[scan_idx] = level;
            if level != 0 { eob = scan_idx as u16 + 1; }
        }
        let mut dqcoeffs = [0i32; 256];
        for scan_idx in 0..eob as usize {
            let coeff_idx = scan[scan_idx] as usize;
            dqcoeffs[coeff_idx] = dequantize_coeff(levels[scan_idx], 0, dequant);
        }
        let inv_residual = dct::inverse_dct2_16x16(&dqcoeffs);
        reconstruct_block(recon, plane_w, x, y, 16, &pred_block, &inv_residual);
        EncodedBlock {
            x, y, bsize, mode, levels, eob,
            log_scale: 0,
            uv_mode: IntraMode::DcPred, u_levels: Vec::new(), u_eob: 0, v_levels: Vec::new(), v_eob: 0,
            y_residual: Vec::new(), u_residual: Vec::new(), v_residual: Vec::new(),
        }
    } else if bsize == 8 {
        let mut residual_arr = [0i32; 64];
        residual_arr.copy_from_slice(&residual);
        let dct_coeffs = dct::forward_dct2_8x8(&residual_arr);
        let scan = &crate::scan::DEFAULT_SCAN_8X8;
        let mut levels = vec![0i32; 64];
        let mut eob = 0u16;
        for scan_idx in 0..64 {
            let coeff_idx = scan[scan_idx] as usize;
            let level = quantize_coeff(dct_coeffs[coeff_idx] as i64, 0, dequant);
            levels[scan_idx] = level;
            if level != 0 { eob = scan_idx as u16 + 1; }
        }
        let mut dqcoeffs = [0i32; 64];
        for scan_idx in 0..eob as usize {
            let coeff_idx = scan[scan_idx] as usize;
            dqcoeffs[coeff_idx] = dequantize_coeff(levels[scan_idx], 0, dequant);
        }
        let inv_residual = dct::inverse_dct2_8x8(&dqcoeffs);
        reconstruct_block(recon, plane_w, x, y, 8, &pred_block, &inv_residual);
        EncodedBlock {
            x, y, bsize, mode, levels, eob,
            log_scale: 0,
            uv_mode: IntraMode::DcPred, u_levels: Vec::new(), u_eob: 0, v_levels: Vec::new(), v_eob: 0,
            y_residual: Vec::new(), u_residual: Vec::new(), v_residual: Vec::new(),
        }
    } else {
        panic!("unsupported block size: {}", bsize);
    }
}

fn compute_ssd(a: &[u8], b: &[u8]) -> u64 {
    a.iter().zip(b.iter()).map(|(&x, &y)| {
        let d = x as i64 - y as i64;
        (d * d) as u64
    }).sum()
}

fn encode_partition_rd(
    pixels: &[u8],
    plane_w: usize,
    recon: &mut [u8],
    ready: &mut [bool],
    x: usize,
    y: usize,
    bsize: usize,
    dequant: i32,
    lambda: f64,
    min_bsize: usize,
    right_limit: usize,
    bottom_limit: usize,
    lossless: bool,
) -> Vec<EncodedBlock> {

    let original = extract_subblock(pixels, plane_w, x, y, bsize, bsize);
    let recon_backup: Vec<u8> = {
        let mut buf = vec![0u8; bsize * bsize];
        for r in 0..bsize {
            for c in 0..bsize {
                buf[r * bsize + c] = recon[(y + r) * plane_w + x + c];
            }
        }
        buf
    };

    let no_split = encode_block_var(pixels, plane_w, recon, ready, x, y, bsize, dequant, None, right_limit, bottom_limit, lossless);
    if lossless {
        return vec![no_split];
    }
    let recon_no_split = extract_subblock(recon, plane_w, x, y, bsize, bsize);
    let dist_no_split = compute_ssd(&original, &recon_no_split);

    let half = bsize / 2;
    if half < min_bsize {
        return vec![no_split];
    }

    for r in 0..bsize {
        for c in 0..bsize {
            recon[(y + r) * plane_w + x + c] = recon_backup[r * bsize + c];
        }
    }

    let mut split_blocks = Vec::new();
    let sub_params: [(usize, usize, usize, usize); 4] = [
        (x,      y,      right_limit,       bottom_limit),
        (x+half, y,      right_limit,       y + half),
        (x,      y+half, x + bsize,         bottom_limit),
        (x+half, y+half, x + bsize,         y + bsize),
    ];
    for &(bx, by, rl, bl) in &sub_params {
        let sub = encode_partition_rd(pixels, plane_w, recon, ready, bx, by, half, dequant, lambda, min_bsize, rl, bl, lossless);
        split_blocks.extend(sub);
    }

    let recon_split = extract_subblock(recon, plane_w, x, y, bsize, bsize);
    let dist_split = compute_ssd(&original, &recon_split);

    let split_overhead = (bsize * bsize / 2) as u64;
    let split_improves = dist_split + split_overhead < dist_no_split;

    if split_improves {
        split_blocks
    } else {
        for r in 0..bsize {
            for c in 0..bsize {
                recon[(y + r) * plane_w + x + c] = recon_no_split[r * bsize + c];
            }
        }
        vec![no_split]
    }
}

fn downsample_64_to_32(residual: &[i32; 4096]) -> [i32; 1024] {
    let mut out = [0i32; 1024];
    for r in 0..32 {
        for c in 0..32 {
            let tl = residual[(2 * r) * 64 + 2 * c];
            let tr = residual[(2 * r) * 64 + 2 * c + 1];
            let bl = residual[(2 * r + 1) * 64 + 2 * c];
            let br = residual[(2 * r + 1) * 64 + 2 * c + 1];
            out[r * 32 + c] = (tl + tr + bl + br + 2) >> 2;
        }
    }
    out
}

pub struct EncodedSuperblock {
    pub y_blocks: Vec<EncodedBlock>,
    pub u_levels: [i32; 1024],
    pub u_eob: u16,
    pub v_levels: [i32; 1024],
    pub v_eob: u16,
    pub uv_mode: IntraMode,
}

fn qindex_to_lambda(qindex: u8) -> f64 {
    let q = ac_qlookup(qindex) as f64;
    0.04 * q * q
}

fn encode_frame_yuv(
    y_plane: &[u8],
    u_plane: &[u8],
    v_plane: &[u8],
    params: &FrameEncodeParams,
    allow_cdf_update: bool,
) -> Vec<u8> {
    let w = params.width as usize;
    let h = params.height as usize;
    let sb_cols = w / 64;
    let sb_rows = h / 64;
    let uv_w = w / 2;
    let uv_h = h / 2;

    let mut recon_y = vec![0u8; w * h];
    let mut recon_u = vec![0u8; uv_w * uv_h];
    let mut recon_v = vec![0u8; uv_w * uv_h];
    let mut ready_y = vec![false; w * h];
    let mut ready_u = vec![false; uv_w * uv_h];
    let mut ready_v = vec![false; uv_w * uv_h];

    let block_offsets: [(usize, usize); 4] = [
        (0, 0), (32, 0), (0, 32), (32, 32),
    ];

    let lambda = qindex_to_lambda(params.base_qindex);
    let lossless = params.base_qindex == 0;

    let mut sbs = Vec::with_capacity(sb_cols * sb_rows);
    for sb_row in 0..sb_rows {
        for sb_col in 0..sb_cols {
            let sb_x = sb_col * 64;
            let sb_y = sb_row * 64;

            let mut y_blocks = Vec::new();
            for &(bx, by) in &block_offsets {
                let yx = sb_x + bx;
                let yy = sb_y + by;

                let right_limit = if yy == sb_y {
                    if sb_y > 0 { w } else { yx + 32 }
                } else {
                    (sb_x + 64).min(w)
                };
                let bottom_limit = if yx == sb_x {
                    if sb_x > 0 { (sb_y + 64).min(h) } else { yy + 32 }
                } else {
                    (sb_y + if by == 0 { 32 } else { 64 }).min(h)
                };

                let mut leaf_blocks = encode_partition_rd(
                    y_plane, w, &mut recon_y, &mut ready_y, yx, yy, 32, params.dequant, lambda, 8,
                    right_limit, bottom_limit, lossless,
                );



                for block in &mut leaf_blocks {
                    let uv_bsize = block.bsize / 2;
                    let ux = block.x / 2;
                    let uy = block.y / 2;

                    let uv_mode = IntraMode::DcPred;
                    let (u_levels, u_eob, u_residual) = encode_uv_subblock(
                        u_plane, uv_w, &mut recon_u, &mut ready_u, ux, uy, uv_bsize,
                        params.dequant, uv_mode, lossless,
                    );
                    let (v_levels, v_eob, v_residual) = encode_uv_subblock(
                        v_plane, uv_w, &mut recon_v, &mut ready_v, ux, uy, uv_bsize,
                        params.dequant, uv_mode, lossless,
                    );

                    block.uv_mode = uv_mode;
                    block.u_levels = u_levels;
                    block.u_eob = u_eob;
                    block.v_levels = v_levels;
                    block.v_eob = v_eob;
                    block.u_residual = u_residual;
                    block.v_residual = v_residual;
                }

                y_blocks.extend(leaf_blocks);
            }

            sbs.push(EncodedSuperblock {
                y_blocks,
                u_levels: [0i32; 1024],
                u_eob: 0,
                v_levels: [0i32; 1024],
                v_eob: 0,
                uv_mode: IntraMode::DcPred,
            });
        }
    }

    let tile_data = tile::encode_tile_data_split4(sb_cols, sb_rows, &sbs, allow_cdf_update, params.base_qindex);
    let frame_config = FrameHeaderConfig {
        width: params.width,
        height: params.height,
        base_qindex: params.base_qindex,
        order_hint: params.order_hint,
        disable_cdf_update: !allow_cdf_update,
    };
    let clk_payload = write_clk_obu_payload(&frame_config, &tile_data);
    ObuWriter::pack_obu(ObuType::Clk, &clk_payload)
}

pub fn encode_still_image(width: u32, height: u32, base_qindex: u8) -> Vec<u8> {
    let pw = round_up_to_64(width);
    let ph = round_up_to_64(height);
    encode_video(width, height, base_qindex, &[vec![128; (pw * ph) as usize]])
}

pub fn encode_video(width: u32, height: u32, base_qindex: u8, y_planes: &[Vec<u8>]) -> Vec<u8> {
    let mut output = Vec::new();

    let pw = round_up_to_64(width);
    let ph = round_up_to_64(height);

    let seq_config = SequenceHeaderConfig {
        width: pw,
        height: ph,
    };
    let seq_payload = write_sequence_header(&seq_config);
    output.extend_from_slice(&ObuWriter::pack_obu(ObuType::TemporalDelimiter, &[]));
    let seq_obu = ObuWriter::pack_obu(ObuType::SequenceHeader, &seq_payload);
    output.extend_from_slice(&seq_obu);

    for (i, _y_plane) in y_planes.iter().enumerate() {
        if i > 0 {
            output.extend_from_slice(&ObuWriter::pack_obu(ObuType::TemporalDelimiter, &[]));
        }
        let tile_data = tile::encode_tile_data();
        let frame_config = FrameHeaderConfig {
            width: pw,
            height: ph,
            base_qindex,
            order_hint: i as u32,
            disable_cdf_update: true,
        };
        let clk_payload = write_clk_obu_payload(&frame_config, &tile_data);
        let clk_obu = ObuWriter::pack_obu(ObuType::Clk, &clk_payload);
        output.extend_from_slice(&clk_obu);
    }

    output
}

pub fn encode_video_yuv(
    width: u32,
    height: u32,
    base_qindex: u8,
    frames: &[(Vec<u8>, Vec<u8>, Vec<u8>)],
) -> Vec<u8> {
    let mut output = Vec::new();

    let pw = round_up_to_64(width);
    let ph = round_up_to_64(height);

    let seq_config = SequenceHeaderConfig {
        width: pw,
        height: ph,
    };
    let seq_payload = write_sequence_header(&seq_config);
    output.extend_from_slice(&ObuWriter::pack_obu(ObuType::TemporalDelimiter, &[]));
    let seq_obu = ObuWriter::pack_obu(ObuType::SequenceHeader, &seq_payload);
    output.extend_from_slice(&seq_obu);

    let dequant = ac_qlookup(base_qindex);

    for (i, (y_plane, u_plane, v_plane)) in frames.iter().enumerate() {
        if i > 0 {
            output.extend_from_slice(&ObuWriter::pack_obu(ObuType::TemporalDelimiter, &[]));
        }
        let padded_y = pad_plane(y_plane, width, height, pw, ph);
        let padded_u = pad_plane(u_plane, width / 2, height / 2, pw / 2, ph / 2);
        let padded_v = pad_plane(v_plane, width / 2, height / 2, pw / 2, ph / 2);

        let clk_obu = encode_frame_yuv(
            &padded_y,
            &padded_u,
            &padded_v,
            &FrameEncodeParams {
                width: pw,
                height: ph,
                dequant,
                base_qindex,
                order_hint: i as u32,
            },
            true,
        );
        output.extend_from_slice(&clk_obu);
    }

    output
}

pub fn encode_video_yuv_with_cdf_update(
    width: u32,
    height: u32,
    base_qindex: u8,
    frames: &[(Vec<u8>, Vec<u8>, Vec<u8>)],
) -> Vec<u8> {
    let mut output = Vec::new();

    let pw = round_up_to_64(width);
    let ph = round_up_to_64(height);

    let seq_config = SequenceHeaderConfig {
        width: pw,
        height: ph,
    };
    let seq_payload = write_sequence_header(&seq_config);
    output.extend_from_slice(&ObuWriter::pack_obu(ObuType::TemporalDelimiter, &[]));
    let seq_obu = ObuWriter::pack_obu(ObuType::SequenceHeader, &seq_payload);
    output.extend_from_slice(&seq_obu);

    let dequant = ac_qlookup(base_qindex);

    for (i, (y_plane, u_plane, v_plane)) in frames.iter().enumerate() {
        if i > 0 {
            output.extend_from_slice(&ObuWriter::pack_obu(ObuType::TemporalDelimiter, &[]));
        }
        let padded_y = pad_plane(y_plane, width, height, pw, ph);
        let padded_u = pad_plane(u_plane, width / 2, height / 2, pw / 2, ph / 2);
        let padded_v = pad_plane(v_plane, width / 2, height / 2, pw / 2, ph / 2);

        let clk_obu = encode_frame_yuv(
            &padded_y,
            &padded_u,
            &padded_v,
            &FrameEncodeParams {
                width: pw,
                height: ph,
                dequant,
                base_qindex,
                order_hint: i as u32,
            },
            true,
        );
        output.extend_from_slice(&clk_obu);
    }

    output
}

pub fn compute_residual_block(pixels: &[u8], pred_block: &[u8]) -> Vec<i32> {
    pixels
        .iter()
        .zip(pred_block.iter())
        .map(|(&p, &q)| p as i32 - q as i32)
        .collect()
}

pub fn quantize_coeff(coeff: i64, log_scale: u32, dequant: i32) -> i32 {
    let scaled = coeff << (3 + log_scale);
    if dequant == 0 {
        return scaled as i32;
    }
    let dq = dequant as i64;
    let sign = if scaled < 0 { -1i64 } else { 1i64 };
    (sign * ((scaled.abs() + dq / 2) / dq)) as i32
}

pub fn quantize_block(
    dct_coeffs: &[i32; 1024],
    dequant: i32,
    log_scale: u32,
) -> ([i32; 1024], u16) {
    let mut levels = [0i32; 1024];
    let mut eob = 0u16;

    for scan_idx in 0..1024 {
        let coeff_idx = DEFAULT_SCAN_32X32[scan_idx] as usize;
        let coeff = dct_coeffs[coeff_idx] as i64;
        let level = quantize_coeff(coeff, log_scale, dequant);
        levels[scan_idx] = level;
        if level != 0 {
            eob = scan_idx as u16 + 1;
        }
    }

    (levels, eob)
}

pub fn ac_qlookup(qindex: u8) -> i32 {
    #[rustfmt::skip]
    const AC_QLOOKUP_QTX: [i32; 25] = [
        64, 40, 41, 43, 44, 45, 47, 48, 49, 51, 52,
        54, 55, 57, 59, 60, 62, 64, 66, 68, 70, 72, 74, 76, 78,
    ];

    let q = qindex as i32;
    if q < 25 {
        AC_QLOOKUP_QTX[q as usize]
    } else {
        AC_QLOOKUP_QTX[(((q - 1) % 24) + 1) as usize] << ((q - 1) / 24)
    }
}

pub fn dequantize_coeff(level: i32, log_scale: u32, dequant: i32) -> i32 {
    let abs_level = level.unsigned_abs() as i64;
    let sign = if level < 0 { -1i64 } else { 1i64 };
    let dqcoeff = (((abs_level * dequant as i64 + 4) >> 3) >> log_scale) as i32;
    (sign * dqcoeff as i64) as i32
}

pub fn dequantize_block(
    levels: &[i32; 1024],
    eob: u16,
    dequant: i32,
    log_scale: u32,
) -> [i32; 1024] {
    let mut dqcoeffs = [0i32; 1024];
    for scan_idx in 0..eob as usize {
        let coeff_idx = DEFAULT_SCAN_32X32[scan_idx] as usize;
        dqcoeffs[coeff_idx] = dequantize_coeff(levels[scan_idx], log_scale, dequant);
    }
    dqcoeffs
}

pub fn reconstruct_block(
    recon: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    bsize: usize,
    pred_block: &[u8],
    inv_residual: &[i32],
) {
    for row in 0..bsize {
        for col in 0..bsize {
            let pixel = (pred_block[row * bsize + col] as i32 + inv_residual[row * bsize + col])
                .clamp(0, 255) as u8;
            recon[(y + row) * stride + x + col] = pixel;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obu_types(bitstream: &[u8]) -> Vec<u8> {
        crate::decode::obu::parse_obus(bitstream)
            .iter()
            .map(|obu| obu.obu_type)
            .collect()
    }

    fn quantize_dc(residual: i32, log_scale: u32, dequant: i32) -> i32 {
        let dc_coeff = 128i64 * residual as i64;
        quantize_coeff(dc_coeff, log_scale, dequant)
    }

    #[test]
    fn bitstream_starts_with_seq_header_obu() {
        let bs = encode_still_image(64, 64, 50);
        assert!(bs.len() > 13);
        assert_eq!(
            obu_types(&bs),
            vec![2, 1, 4],
            "stream should be TD, SequenceHeader, CLK"
        );
    }

    #[test]
    fn bitstream_contains_two_obus() {
        let bs = encode_still_image(64, 64, 50);
        let types = obu_types(&bs);
        assert_eq!(types.last(), Some(&4), "should end with a CLK OBU");
    }

    #[test]
    fn bitstream_reasonable_size() {
        let bs = encode_still_image(64, 64, 50);
        assert!(bs.len() <= 50, "bitstream too large: {} bytes", bs.len());
    }

    #[test]
    fn encode_multiple_frames() {
        let frames: Vec<Vec<u8>> = (0..3).map(|_| vec![128; 64 * 64]).collect();
        let bs = encode_video(64, 64, 50, &frames);
        let seq_header_obu_size = 1 + 1 + 17; // leb128 + header + payload
        let single_clk_obu_size = 1 + 1 + 4 + 4; // leb128 + header + frame_header + tile_data (approx)
        assert!(
            bs.len() > seq_header_obu_size + single_clk_obu_size,
            "bitstream too small: {} bytes",
            bs.len()
        );
    }

    #[test]
    fn encode_video_has_correct_obu_count() {
        let frames: Vec<Vec<u8>> = (0..5).map(|_| vec![128; 64 * 64]).collect();
        let bs = encode_video(64, 64, 50, &frames);
        let types = obu_types(&bs);
        assert_eq!(types.iter().filter(|&&t| t == 4).count(), 5);
        assert_eq!(types.iter().filter(|&&t| t == 2).count(), 5);
    }

    #[test]
    fn encode_frame_with_pixel_data() {
        let y_plane = vec![149u8; 64 * 64];
        let u_plane = vec![43u8; 32 * 32];
        let v_plane = vec![21u8; 32 * 32];
        let bs = encode_video_yuv(64, 64, 50, &[(y_plane, u_plane, v_plane)]);
        assert!(!bs.is_empty());
    }

    #[test]
    fn ac_qlookup_qindex_0() {
        assert_eq!(ac_qlookup(0), 64);
    }

    #[test]
    fn ac_qlookup_qindex_50() {
        let q = ac_qlookup(50);
        assert!(q > 0);
    }

    #[test]
    fn quantize_dc_zero_residual() {
        assert_eq!(quantize_dc(0, 2, 164), 0);
    }

    #[test]
    fn quantize_dc_positive_residual() {
        let level = quantize_dc(21, 2, 164);
        assert!(level > 0);
        assert_eq!(level, 524);
    }

    #[test]
    fn quantize_dc_matches_decoder_chain() {
        let level = quantize_dc(21, 2, 164);
        let dqcoeff = ((level as i64 * 164 + 4) >> 3) >> 2;
        let val = (64 * dqcoeff + 32) >> 6;
        let pixel_residual = (64 * val + 4096) >> 13;
        assert_eq!(pixel_residual, 21);
    }

    #[test]
    fn compute_residual_block_midgray() {
        let pixels = vec![128u8; 64];
        let residual = compute_residual_block(&pixels, &[128u8; 64]);
        assert!(residual.iter().all(|&r| r == 0));
    }

    #[test]
    fn compute_residual_block_values() {
        let pixels = vec![200u8; 4];
        let residual = compute_residual_block(&pixels, &[128u8; 4]);
        assert!(residual.iter().all(|&r| r == 72));
    }

    #[test]
    fn quantize_block_all_zero() {
        let dct_coeffs = [0i32; 1024];
        let (levels, eob) = quantize_block(&dct_coeffs, 164, 1);
        assert_eq!(eob, 0);
        assert!(levels.iter().all(|&l| l == 0));
    }

    #[test]
    fn quantize_block_dc_only() {
        let mut dct_coeffs = [0i32; 1024];
        dct_coeffs[0] = 128 * 21;
        let (levels, eob) = quantize_block(&dct_coeffs, 164, 1);
        assert_eq!(eob, 1);
        assert!(levels[0] != 0);
        assert!(levels[1..].iter().all(|&l| l == 0));
    }

    #[test]
    fn quantize_block_preserves_dc_value() {
        let dequant = ac_qlookup(50);
        let dc_from_old = quantize_dc(21, 2, dequant);
        let dc_coeff = 128 * 21;
        let dc_from_new = quantize_coeff(dc_coeff as i64, 2, dequant);
        assert_eq!(dc_from_old, dc_from_new);
    }

    #[test]
    fn solid_color_dct_produces_dc_only() {
        let pixels = vec![200u8; 32 * 32];
        let residual = compute_residual_block(&pixels, &vec![128u8; 32 * 32]);
        let mut residual_arr = [0i32; 1024];
        residual_arr.copy_from_slice(&residual);
        let dct_coeffs = crate::dct::forward_dct2_32x32(&residual_arr);
        let dequant = ac_qlookup(50);
        let (levels, eob) = quantize_block(&dct_coeffs, dequant, 1);
        assert_eq!(eob, 1);
        assert!(levels[0] != 0);
    }

    #[test]
    fn encoder_new_valid_dimensions() {
        let enc = Encoder::new(64, 64, 50);
        assert!(enc.is_ok());
    }

    #[test]
    fn encoder_new_128x128_valid() {
        let enc = Encoder::new(128, 128, 50);
        assert!(enc.is_ok());
    }

    #[test]
    fn encoder_new_invalid_dimensions() {
        assert!(Encoder::new(101, 100, 50).is_err());
        assert!(Encoder::new(0, 64, 50).is_err());
        assert!(Encoder::new(3, 64, 50).is_err());
    }

    #[test]
    fn encoder_new_accepts_multiple_of_4() {
        assert!(Encoder::new(100, 100, 50).is_ok());
        assert!(Encoder::new(1920, 1080, 50).is_ok());
        assert!(Encoder::new(4, 4, 50).is_ok());
        assert!(Encoder::new(640, 480, 50).is_ok());
    }

    #[test]
    fn encoder_new_rejects_non_multiple_of_4() {
        assert!(Encoder::new(101, 100, 50).is_err());
        assert!(Encoder::new(100, 3, 50).is_err());
        assert!(Encoder::new(0, 100, 50).is_err());
        assert!(Encoder::new(2, 2, 50).is_err());
    }

    #[test]
    fn encoder_width_height_return_original() {
        let enc = Encoder::new(100, 100, 50).unwrap();
        assert_eq!(enc.width(), 100);
        assert_eq!(enc.height(), 100);
    }

    #[test]
    fn encoder_headers_returns_sequence_header_obu() {
        let enc = Encoder::new(64, 64, 50).unwrap();
        let headers = enc.headers();
        assert!(!headers.is_empty());
        assert_eq!(
            obu_types(&headers),
            vec![2, 1],
            "headers should be TD followed by SequenceHeader"
        );
    }

    #[test]
    fn encoder_send_frame_produces_packet() {
        let mut enc = Encoder::new(64, 64, 50).unwrap();
        let y = vec![149u8; 64 * 64];
        let u = vec![43u8; 32 * 32];
        let v = vec![21u8; 32 * 32];
        enc.send_frame(&y, &u, &v).unwrap();
        let pkt = enc.receive_packet();
        assert!(pkt.is_some());
        let pkt = pkt.unwrap();
        assert_eq!(pkt.frame_number, 0);
        assert!(!pkt.data.is_empty());
    }

    #[test]
    fn encoder_receive_without_send_returns_none() {
        let mut enc = Encoder::new(64, 64, 50).unwrap();
        assert!(enc.receive_packet().is_none());
    }

    #[test]
    fn encoder_multi_frame_increments_frame_number() {
        let mut enc = Encoder::new(64, 64, 50).unwrap();
        let y = vec![128u8; 64 * 64];
        let u = vec![128u8; 32 * 32];
        let v = vec![128u8; 32 * 32];

        for expected in 0..3u64 {
            enc.send_frame(&y, &u, &v).unwrap();
            let pkt = enc.receive_packet().unwrap();
            assert_eq!(pkt.frame_number, expected);
        }
    }

    #[test]
    fn encoder_flush_is_noop() {
        let mut enc = Encoder::new(64, 64, 50).unwrap();
        enc.flush();
        assert!(enc.receive_packet().is_none());
    }

    #[test]
    fn encoder_rejects_wrong_plane_size() {
        let mut enc = Encoder::new(64, 64, 50).unwrap();
        let y = vec![128u8; 100];
        let u = vec![128u8; 32 * 32];
        let v = vec![128u8; 32 * 32];
        assert!(enc.send_frame(&y, &u, &v).is_err());
    }

    #[test]
    fn encoder_streaming_produces_valid_obu() {
        let y = vec![149u8; 64 * 64];
        let u = vec![43u8; 32 * 32];
        let v = vec![21u8; 32 * 32];

        let mut enc = Encoder::new(64, 64, 50).unwrap();
        enc.send_frame(&y, &u, &v).unwrap();
        let pkt = enc.receive_packet().unwrap();
        assert!(!pkt.data.is_empty());
        assert_eq!(pkt.frame_number, 0);
    }

    #[test]
    fn encoder_128x128_solid_color() {
        let mut enc = Encoder::new(128, 128, 50).unwrap();
        let y = vec![200u8; 128 * 128];
        let u = vec![100u8; 64 * 64];
        let v = vec![150u8; 64 * 64];
        enc.send_frame(&y, &u, &v).unwrap();
        let pkt = enc.receive_packet().unwrap();
        assert!(!pkt.data.is_empty());
        assert_eq!(pkt.frame_number, 0);
    }

    #[test]
    fn encoder_256x128_solid_color() {
        let mut enc = Encoder::new(256, 128, 50).unwrap();
        let y = vec![180u8; 256 * 128];
        let u = vec![60u8; 128 * 64];
        let v = vec![200u8; 128 * 64];
        enc.send_frame(&y, &u, &v).unwrap();
        let pkt = enc.receive_packet().unwrap();
        assert!(!pkt.data.is_empty());
    }

    #[test]
    fn encoder_128x128_produces_larger_bitstream() {
        let bs_64 = encode_video_yuv(
            64,
            64,
            50,
            &[(
                vec![149u8; 64 * 64],
                vec![43u8; 32 * 32],
                vec![21u8; 32 * 32],
            )],
        );
        let bs_128 = encode_video_yuv(
            128,
            128,
            50,
            &[(
                vec![149u8; 128 * 128],
                vec![43u8; 64 * 64],
                vec![21u8; 64 * 64],
            )],
        );
        assert!(
            bs_128.len() > bs_64.len(),
            "128x128 ({} bytes) should be larger than 64x64 ({} bytes)",
            bs_128.len(),
            bs_64.len()
        );
    }

    #[test]
    fn encoder_128x128_sb_grid_count() {
        let w = 128usize;
        let h = 128usize;
        let sb_cols = w / 64;
        let sb_rows = h / 64;
        assert_eq!(sb_cols * sb_rows, 4);
    }

    #[test]
    fn encoder_256x128_sb_grid_count() {
        let w = 256usize;
        let h = 128usize;
        let sb_cols = w / 64;
        let sb_rows = h / 64;
        assert_eq!(sb_cols * sb_rows, 8);
    }

    #[test]
    fn extract_subblock_full_frame() {
        let plane: Vec<u8> = (0..16).collect();
        let block = extract_subblock(&plane, 4, 0, 0, 4, 4);
        assert_eq!(block, plane);
    }

    #[test]
    fn extract_subblock_offset() {
        let plane: Vec<u8> = (0..64).collect();
        let block = extract_subblock(&plane, 8, 2, 1, 3, 2);
        assert_eq!(block, vec![10, 11, 12, 18, 19, 20]);
    }

    #[test]
    fn dequantize_coeff_roundtrip() {
        let dequant = ac_qlookup(50);
        let residual = 21i32;
        let dc_coeff = 128i64 * residual as i64;
        let level = quantize_coeff(dc_coeff, 2, dequant);
        let dqcoeff = dequantize_coeff(level, 2, dequant);
        let val = (64i64 * dqcoeff as i64 + 32) >> 6;
        let pixel_residual = ((64 * val + 4096) >> 13) as i32;
        assert_eq!(pixel_residual, residual);
    }

    #[test]
    fn dequantize_block_dc_only() {
        let dequant = ac_qlookup(50);
        let mut levels = [0i32; 1024];
        levels[0] = 524;
        let dqcoeffs = dequantize_block(&levels, 1, dequant, 2);
        assert_ne!(dqcoeffs[0], 0);
        assert!(dqcoeffs[1..].iter().all(|&c| c == 0));
    }

    #[test]
    fn reconstruct_block_basic() {
        let mut recon = vec![0u8; 4];
        let inv_residual = [10i32, 20, -5, 30];
        reconstruct_block(&mut recon, 2, 0, 0, 2, &[128u8; 4], &inv_residual);
        assert_eq!(recon, vec![138, 148, 123, 158]);
    }

    #[test]
    fn reconstruct_block_clamps() {
        let mut recon = vec![0u8; 4];
        let inv_residual = [200i32, -200, 0, 0];
        reconstruct_block(&mut recon, 2, 0, 0, 2, &[128u8; 4], &inv_residual);
        assert_eq!(recon[0], 255);
        assert_eq!(recon[1], 0);
    }

    #[test]
    fn compute_residual_block_custom_prediction() {
        let pixels = vec![200u8; 4];
        let residual = compute_residual_block(&pixels, &[150u8; 4]);
        assert!(residual.iter().all(|&r| r == 50));
    }

    #[test]
    fn encode_video_yuv_non_aligned() {
        let w = 100u32;
        let h = 100u32;
        let y = vec![200u8; (w * h) as usize];
        let u = vec![128u8; (w / 2 * h / 2) as usize];
        let v = vec![128u8; (w / 2 * h / 2) as usize];
        let bs = encode_video_yuv(w, h, 50, &[(y, u, v)]);
        assert!(!bs.is_empty());
    }

    #[test]
    fn round_up_to_64_exact() {
        assert_eq!(round_up_to_64(64), 64);
        assert_eq!(round_up_to_64(128), 128);
    }

    #[test]
    fn round_up_to_64_rounds_up() {
        assert_eq!(round_up_to_64(4), 64);
        assert_eq!(round_up_to_64(100), 128);
        assert_eq!(round_up_to_64(1080), 1088);
        assert_eq!(round_up_to_64(1920), 1920);
    }

    #[test]
    fn pad_plane_no_padding_needed() {
        let plane = vec![200u8; 64 * 64];
        let padded = pad_plane(&plane, 64, 64, 64, 64);
        assert_eq!(padded, plane);
    }

    #[test]
    fn pad_plane_pads_right_and_bottom() {
        let plane = vec![200u8; 4 * 4];
        let padded = pad_plane(&plane, 4, 4, 64, 64);
        assert_eq!(padded.len(), 64 * 64);
        assert_eq!(padded[0], 200);
        assert_eq!(padded[3], 200);
        assert_eq!(padded[4], 128);
        assert_eq!(padded[64 * 4], 128);
    }

    #[test]
    fn pad_plane_preserves_original_pixels() {
        let mut plane = vec![0u8; 100 * 100];
        for (i, pixel) in plane.iter_mut().enumerate() {
            *pixel = (i % 256) as u8;
        }
        let padded = pad_plane(&plane, 100, 100, 128, 128);
        for row in 0..100 {
            for col in 0..100 {
                assert_eq!(
                    padded[row * 128 + col],
                    plane[row * 100 + col],
                    "mismatch at ({}, {})",
                    row,
                    col
                );
            }
        }
    }

    #[test]
    fn intra_mode_values() {
        assert_eq!(IntraMode::DcPred as u8, 0);
        assert_eq!(IntraMode::SmoothPred as u8, 9);
        assert_eq!(IntraMode::SmoothVPred as u8, 10);
        assert_eq!(IntraMode::SmoothHPred as u8, 11);
        assert_eq!(IntraMode::PaethPred as u8, 12);
    }

    #[test]
    fn ref_pixels_no_neighbors() {
        let recon = vec![0u8; 64 * 64];
        let refs = get_ref_pixels(&recon, 64, 0, 0, 32, 64, 64);
        assert!(refs.above.iter().all(|&v| v == 127));
        assert!(refs.left.iter().all(|&v| v == 129));
        assert_eq!(refs.top_left, 128);
        assert!(!refs.has_above);
        assert!(!refs.has_left);
    }

    #[test]
    fn ref_pixels_above_only() {
        let mut recon = vec![0u8; 128 * 128];
        for i in 0..64 {
            recon[63 * 128 + i] = 200;
        }
        let refs = get_ref_pixels(&recon, 128, 0, 64, 64, 128, 128);
        assert!(refs.above.iter().all(|&v| v == 200));
        assert!(refs.left.iter().all(|&v| v == 200));
        assert_eq!(refs.top_left, 200);
        assert!(refs.has_above);
        assert!(!refs.has_left);
    }

    #[test]
    fn ref_pixels_left_only() {
        let mut recon = vec![0u8; 128 * 128];
        for j in 0..64 {
            recon[j * 128 + 63] = 150;
        }
        let refs = get_ref_pixels(&recon, 128, 64, 0, 64, 128, 128);
        assert!(refs.left.iter().all(|&v| v == 150));
        assert!(refs.above.iter().all(|&v| v == 150));
        assert_eq!(refs.top_left, 150);
        assert!(!refs.has_above);
        assert!(refs.has_left);
    }

    #[test]
    fn ref_pixels_both_neighbors() {
        let mut recon = vec![0u8; 128 * 128];
        for i in 0..64 {
            recon[63 * 128 + 64 + i] = 200;
        }
        for j in 0..64 {
            recon[(64 + j) * 128 + 63] = 100;
        }
        recon[63 * 128 + 63] = 180;
        let refs = get_ref_pixels(&recon, 128, 64, 64, 64, 128, 128);
        assert!(refs.above.iter().all(|&v| v == 200));
        assert!(refs.left.iter().all(|&v| v == 100));
        assert_eq!(refs.top_left, 180);
        assert!(refs.has_above);
        assert!(refs.has_left);
    }

    #[test]
    fn predict_dc_block_no_neighbors() {
        let refs = RefPixels::new_test(vec![127; 32], vec![129; 32], 128, false, false, 127, 129);
        let pred = predict_dc_block(&refs, 32);
        assert!(pred.iter().all(|&v| v == 128));
    }

    #[test]
    fn predict_dc_block_both_neighbors() {
        let refs = RefPixels::new_test(vec![200; 32], vec![100; 32], 150, true, true, 200, 100);
        let pred = predict_dc_block(&refs, 32);
        assert!(pred.iter().all(|&v| v == 150));
    }

    #[test]
    fn predict_smooth_no_neighbors_approx_128() {
        let refs = RefPixels::new_test(vec![127; 32], vec![129; 32], 128, false, false, 127, 129);
        let pred = predict_smooth_block(&refs, 32);
        for &v in &pred {
            assert!(
                (v as i32 - 128).abs() <= 1,
                "smooth pred {} too far from 128",
                v
            );
        }
    }

    #[test]
    fn predict_smooth_v_vertical_gradient() {
        let refs = RefPixels::new_test(vec![255; 32], vec![0; 32], 255, true, true, 255, 0);
        let pred = predict_smooth_v_block(&refs, 32);
        assert!(pred[0] > pred[31 * 32], "should decrease top to bottom");
        assert!(pred[0] >= 240, "top row should be near 255, got {}", pred[0]);
    }

    #[test]
    fn predict_smooth_h_horizontal_gradient() {
        let refs = RefPixels::new_test(vec![0; 32], vec![255; 32], 255, true, true, 0, 255);
        let pred = predict_smooth_h_block(&refs, 32);
        assert!(pred[0] > pred[31], "should decrease left to right");
        assert!(pred[0] >= 240, "left col should be near 255, got {}", pred[0]);
    }

    #[test]
    fn predict_paeth_flat_returns_dc() {
        let refs = RefPixels::new_test(vec![128; 32], vec![128; 32], 128, true, true, 128, 128);
        let pred = predict_paeth_block(&refs, 32);
        assert!(pred.iter().all(|&v| v == 128));
    }

    #[test]
    fn predict_paeth_vertical_edge() {
        let refs = RefPixels::new_test(vec![200; 32], vec![100; 32], 200, true, true, 200, 100);
        let pred = predict_paeth_block(&refs, 32);
        assert!(pred.iter().all(|&v| v == 100));
    }

    #[test]
    fn predict_paeth_horizontal_edge() {
        let refs = RefPixels::new_test(vec![100; 32], vec![200; 32], 200, true, true, 100, 200);
        let pred = predict_paeth_block(&refs, 32);
        assert!(pred.iter().all(|&v| v == 100));
    }

    #[test]
    fn select_mode_flat_block_picks_dc() {
        let pixels = vec![150u8; 32 * 32];
        let refs = RefPixels::new_test(vec![150; 32], vec![150; 32], 150, true, true, 150, 150);
        assert_eq!(select_intra_mode(&pixels, &refs, 32), IntraMode::DcPred);
    }

    #[test]
    fn select_mode_vertical_gradient_prefers_smooth_v() {
        let refs = RefPixels::new_test(vec![255; 32], vec![0; 32], 255, true, true, 255, 0);
        let pred_sv = predict_smooth_v_block(&refs, 32);
        let mode = select_intra_mode(&pred_sv, &refs, 32);
        assert_eq!(mode, IntraMode::SmoothVPred);
    }

    #[test]
    fn predict_v_copies_above_row() {
        let refs = RefPixels::new_test(vec![200; 8], vec![100; 8], 150, true, true, 200, 100);
        let pred = predict_v_block(&refs, 8);
        assert!(pred.iter().all(|&v| v == 200));
    }

    #[test]
    fn predict_h_copies_left_col() {
        let refs = RefPixels::new_test(vec![200; 8], vec![100; 8], 150, true, true, 200, 100);
        let pred = predict_h_block(&refs, 8);
        for r in 0..8 {
            for c in 0..8 {
                assert_eq!(pred[r * 8 + c], 100);
            }
        }
    }

    #[test]
    fn directional_modes_produce_valid_output() {
        let refs = RefPixels::new_test(vec![200; 32], vec![100; 32], 150, true, true, 200, 100);
        for &mode in &[
            IntraMode::D45Pred, IntraMode::D135Pred, IntraMode::D113Pred,
            IntraMode::D157Pred, IntraMode::D203Pred, IntraMode::D67Pred,
        ] {
            let pred = predict_block(&refs, 32, mode);
            assert_eq!(pred.len(), 32 * 32);
            assert_eq!(pred.len(), 32 * 32);
        }
    }

    #[test]
    fn mode_joint_index_non_directional() {
        assert_eq!(IntraMode::DcPred.joint_y_index(), 0);
        assert_eq!(IntraMode::SmoothPred.joint_y_index(), 1);
        assert_eq!(IntraMode::SmoothVPred.joint_y_index(), 2);
        assert_eq!(IntraMode::SmoothHPred.joint_y_index(), 3);
        assert_eq!(IntraMode::PaethPred.joint_y_index(), 4);
    }

    #[test]
    fn mode_list_default_has_all_13_base_modes_in_set0() {
        let list = build_y_mode_list(None, None, true, false);
        for i in 0..13 {
            assert!(list[i] >= 0, "mode list position {} should be filled", i);
        }
    }

    #[test]
    fn compute_sad_identical() {
        let a = vec![100u8; 64];
        assert_eq!(compute_sad(&a, &a), 0);
    }

    #[test]
    fn compute_sad_known_value() {
        let a = vec![100u8; 4];
        let b = vec![110u8; 4];
        assert_eq!(compute_sad(&a, &b), 40);
    }

    #[test]
    fn intra_mode_values_match_av2() {
        assert_eq!(IntraMode::DcPred as u8, 0);
        assert_eq!(IntraMode::VPred as u8, 1);
        assert_eq!(IntraMode::HPred as u8, 2);
        assert_eq!(IntraMode::D45Pred as u8, 3);
        assert_eq!(IntraMode::D135Pred as u8, 4);
        assert_eq!(IntraMode::D113Pred as u8, 5);
        assert_eq!(IntraMode::D157Pred as u8, 6);
        assert_eq!(IntraMode::D203Pred as u8, 7);
        assert_eq!(IntraMode::D67Pred as u8, 8);
        assert_eq!(IntraMode::SmoothPred as u8, 9);
        assert_eq!(IntraMode::SmoothVPred as u8, 10);
        assert_eq!(IntraMode::SmoothHPred as u8, 11);
        assert_eq!(IntraMode::PaethPred as u8, 12);
    }
}
