pub mod bitreader;
pub mod entropy;
pub mod frame;
pub mod obu;
pub mod predict;
pub mod qm;
pub mod sequence;
pub mod tile;
pub mod transform;

use std::fmt;

#[derive(Debug)]
pub enum DecodeError {
    InvalidObu,
    UnsupportedProfile,
    UnsupportedFeature(&'static str),
    BitstreamCorrupt,
    UnexpectedEof,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::InvalidObu => write!(f, "invalid OBU"),
            DecodeError::UnsupportedProfile => write!(f, "unsupported profile"),
            DecodeError::UnsupportedFeature(feat) => write!(f, "unsupported feature: {}", feat),
            DecodeError::BitstreamCorrupt => write!(f, "bitstream corrupt"),
            DecodeError::UnexpectedEof => write!(f, "unexpected end of data"),
        }
    }
}

impl std::error::Error for DecodeError {}

fn trace_block_match(tag: &str, x: usize, y: usize) -> bool {
    match std::env::var(tag) {
        Ok(spec) => {
            let mut parts = spec.split(',');
            let sx = parts.next().and_then(|v| v.trim().parse::<usize>().ok());
            let sy = parts.next().and_then(|v| v.trim().parse::<usize>().ok());
            sx == Some(x) && sy == Some(y)
        }
        Err(_) => false,
    }
}

fn trace_point_in_region(tag: &str, x: usize, y: usize, w: usize, h: usize) -> bool {
    match std::env::var(tag) {
        Ok(spec) => {
            let mut parts = spec.split(',');
            let px = parts
                .next()
                .and_then(|value| value.trim().parse::<usize>().ok());
            let py = parts
                .next()
                .and_then(|value| value.trim().parse::<usize>().ok());
            match (px, py) {
                (Some(px), Some(py)) => px >= x && px < x + w && py >= y && py < y + h,
                _ => false,
            }
        }
        Err(_) => false,
    }
}

fn trace_point(tag: &str) -> Option<(usize, usize)> {
    let spec = std::env::var(tag).ok()?;
    let mut parts = spec.split(',');
    let x = parts
        .next()
        .and_then(|value| value.trim().parse::<usize>().ok())?;
    let y = parts
        .next()
        .and_then(|value| value.trim().parse::<usize>().ok())?;
    Some((x, y))
}

pub struct DecodedFrame {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub frame_number: u64,
}

pub struct Decoder;

impl Decoder {
    pub fn decode_all(data: &[u8]) -> Result<Vec<DecodedFrame>, DecodeError> {
        let obus = obu::parse_obus(data);
        if obus.is_empty() {
            return Err(DecodeError::InvalidObu);
        }

        let seq_header_index = obus
            .iter()
            .position(|obu| obu.obu_type == 1)
            .ok_or(DecodeError::InvalidObu)?;
        let seq = sequence::parse_sequence_header(obus[seq_header_index].payload)?;
        let mut frames = Vec::new();
        let mut frame_number = 0u64;

        for raw_obu in &obus[seq_header_index + 1..] {
            if raw_obu.obu_type != 4 {
                continue;
            }
            let fp = frame::parse_frame_header(raw_obu.payload, &seq)?;
            if fp.deblocking_enabled
                || fp.gdf_enabled
                || fp.cdef_frame_enable
                || fp.restoration_enabled
                || fp.ccso_frame_enable
            {
                return Err(DecodeError::UnsupportedFeature("in-loop filters"));
            }
            let dequant = crate::encode::ac_qlookup(fp.base_qindex);
            let uv_dequant = crate::encode::ac_qlookup(
                (fp.base_qindex as i32 + fp.u_ac_delta_q).clamp(0, 255) as u8,
            );
            let tile_cfg = tile::TileDecodeConfig {
                enable_cfl_intra: seq.enable_cfl_intra,
                enable_mhccp: seq.enable_mhccp,
                enable_fsc: seq.enable_fsc,
                enable_idtx_intra: seq.enable_idtx_intra,
                enable_mrls: seq.enable_mrls,
                enable_ist: seq.enable_ist,
                enable_cctx: seq.enable_cctx,
                tx_mode_select: fp.tx_mode_select,
                coded_lossless: fp.base_qindex == 0,
                reduced_tx_set_used: fp.reduced_tx_set_used,
                reduced_tx_part_set: seq.reduced_tx_part_set,
                allow_intrabc: fp.allow_intrabc,
                allow_screen_content_tools: fp.allow_screen_content_tools,
                enable_sdp: seq.enable_sdp,
                cdef_frame_enable: fp.cdef_frame_enable,
                nb_cdef_strengths: fp.nb_cdef_strengths,
                ccso_frame_enable: fp.ccso_frame_enable,
                ccso_enable: fp.ccso_enable,
                sb_size: seq.sb_size as usize,
                tcq_mode: fp.tcq_mode,
                allow_parity_hiding: fp.allow_parity_hiding,
                enable_ext_partitions: seq.enable_ext_partitions,
                enable_uneven_4way_partitions: seq.enable_uneven_4way_partitions,
                enable_refmvbank: seq.enable_refmvbank,
                max_bvp_drl_bits: fp.max_bvp_drl_bits,
                cur_frame_force_integer_mv: fp.cur_frame_force_integer_mv,
                allow_intrabc_morph_pred: seq.enable_intra_bawp && fp.allow_screen_content_tools,
            };

            let (y_plane, u_plane, v_plane) = if !fp.tile_data.is_empty() {
                let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                    || -> Result<(Vec<u8>, Vec<u8>, Vec<u8>), DecodeError> {
                    let blocks = tile::decode_tile_data_partitioned_cfg(
                        fp.tile_data,
                        seq.width as usize,
                        seq.height as usize,
                        seq.sb_size as usize,
                        fp.base_qindex,
                        &tile_cfg,
                    )?;
                    Ok(reconstruct_partitioned_frame(
                        &blocks,
                        seq.width,
                        seq.height,
                        dequant,
                        uv_dequant,
                        fp.tcq_mode,
                        fp.base_qindex == 0,
                        seq.cfl_ds_filter_index,
                        seq.sb_size as usize,
                        seq.enable_ibp,
                        !seq.enable_intra_edge_filter,
                        if fp.using_qmatrix {
                            [fp.qm_y[0], fp.qm_u[0], fp.qm_v[0]]
                        } else {
                            [qm::NUM_QM_LEVELS as u8 - 1; 3]
                        },
                        &fp.ccso_params,
                    ))
                }))
                .map_err(|_| DecodeError::UnsupportedFeature("tile syntax"))??;
                decoded
            } else {
                gray_frame(seq.width, seq.height)
            };

            frames.push(DecodedFrame {
                y: y_plane,
                u: u_plane,
                v: v_plane,
                width: seq.width,
                height: seq.height,
                frame_number,
            });
            frame_number += 1;
        }
        Ok(frames)
    }
}

fn round_power_of_two_signed(value: i32, n: u32) -> i32 {
    if value < 0 {
        -((-value + (1 << (n - 1))) >> n)
    } else {
        (value + (1 << (n - 1))) >> n
    }
}


const NUM_REF_SAM_CFL: usize = 8;
const DIV_LUT_BITS: i32 = 7;
const DIV_LUT_PREC_BITS: i32 = 9;
const DIV_LUT: [u16; 129] = [
    512, 508, 504, 500, 496, 493, 489, 485, 482, 478, 475, 471, 468, 465, 462, 458, 455, 452, 449,
    446, 443, 440, 437, 434, 431, 428, 426, 423, 420, 417, 415, 412, 410, 407, 405, 402, 400, 397,
    395, 392, 390, 388, 386, 383, 381, 379, 377, 374, 372, 370, 368, 366, 364, 362, 360, 358, 356,
    354, 352, 350, 349, 347, 345, 343, 341, 340, 338, 336, 334, 333, 331, 329, 328, 326, 324, 323,
    321, 320, 318, 317, 315, 314, 312, 311, 309, 308, 306, 305, 303, 302, 301, 299, 298, 297, 295,
    294, 293, 291, 290, 289, 287, 286, 285, 284, 282, 281, 280, 279, 278, 277, 275, 274, 273, 272,
    271, 270, 269, 267, 266, 265, 264, 263, 262, 261, 260, 259, 258, 257, 256,
];

#[derive(Clone, Debug)]
struct CflImplicitRefs {
    have_top: bool,
    have_left: bool,
    luma_top_q3: Vec<i32>,
    luma_left_q3: Vec<i32>,
}

#[derive(Clone, Debug)]
struct CflImplicitSample {
    side: &'static str,
    idx: usize,
    x_q3: i32,
    y: u8,
}

#[derive(Clone, Debug)]
struct CflImplicitAlpha {
    alpha_q3: i32,
    numb_up: usize,
    numb_left: usize,
    samples: Vec<CflImplicitSample>,
}

fn get_msb_u32(value: u32) -> i32 {
    31 - value.leading_zeros() as i32
}

fn round_power_of_two_unsigned(value: i32, shift: i32) -> i32 {
    if shift <= 0 {
        value
    } else {
        (value + (1 << (shift - 1))) >> shift
    }
}

fn resolve_divisor_32(value: u32) -> (u16, i16) {
    let mut shift = get_msb_u32(value);
    let e = value as i32 - (1 << shift);
    let f = if shift > DIV_LUT_BITS {
        round_power_of_two_unsigned(e, shift - DIV_LUT_BITS)
    } else {
        e << (DIV_LUT_BITS - shift)
    };
    shift += DIV_LUT_PREC_BITS;
    (DIV_LUT[f as usize], shift as i16)
}

fn resolve_divisor_32_cfl(numerator: i32, denominator: i32, shift: i16) -> i16 {
    if numerator == 0 || denominator == 0 {
        return 0;
    }

    let sign = numerator < 0;
    let n = numerator.abs();
    let shift_n = get_msb_u32(n as u32);
    let shift_d = get_msb_u32(denominator as u32);
    let e_d = denominator - (1 << shift_d);
    let f_d = if shift_d > DIV_LUT_BITS {
        round_power_of_two_unsigned(e_d, shift_d - DIV_LUT_BITS)
    } else {
        e_d << (DIV_LUT_BITS - shift_d)
    };
    let f_n = if shift_n > DIV_LUT_BITS {
        round_power_of_two_unsigned(n, shift_n - DIV_LUT_BITS)
    } else {
        n << (DIV_LUT_BITS - shift_n)
    };

    let shift_add = shift_d - shift_n - shift as i32;
    let max_value = (2 << shift as i32) - 1;
    let mut result = if shift_add <= 1 {
        let shift0 = DIV_LUT_PREC_BITS + DIV_LUT_BITS + shift_add;
        if shift0 >= 0 {
            ((DIV_LUT[f_d as usize] as i32) * f_n) >> shift0
        } else {
            max_value
        }
    } else {
        0
    };
    if result >= max_value {
        result = max_value;
    }
    if sign {
        result = -result;
    }
    result as i16
}

fn derive_linear_parameters_alpha_cfl(
    sum_x: i32,
    sum_y: i32,
    sum_xx: i32,
    sum_xy: i32,
    count: i32,
    shift: i16,
) -> i32 {
    if count == 0 {
        return 0;
    }
    let denominator = sum_xx - ((sum_x as i64 * sum_x as i64) / count as i64) as i32;
    let numerator = sum_xy - ((sum_x as i64 * sum_y as i64) / count as i64) as i32;
    if denominator == 0 || numerator == 0 {
        return 0;
    }
    resolve_divisor_32_cfl(numerator, denominator, shift) as i32
}

fn clamp_index(index: isize, limit: usize) -> usize {
    if limit == 0 {
        0
    } else {
        index.clamp(0, limit.saturating_sub(1) as isize) as usize
    }
}

fn sample_plane(
    plane: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    x: isize,
    y: isize,
) -> u8 {
    plane[clamp_index(y, height) * stride + clamp_index(x, width)]
}

fn cfl_top_bottom_offsets(is_top_sb_boundary: bool) -> (isize, isize) {
    if is_top_sb_boundary { (1, 0) } else { (2, 1) }
}

fn cfl_store_luma_q3_420(
    y_plane: &[u8],
    y_w: usize,
    y_h: usize,
    chroma_x: usize,
    chroma_y: usize,
    chroma_width: usize,
    chroma_height: usize,
    filter_type: u8,
) -> Vec<i32> {
    let uv_bw = chroma_width / 2;
    let uv_bh = chroma_height / 2;
    let mut recon_q3 = vec![0i32; uv_bw * uv_bh];

    for j in (0..chroma_height).step_by(2) {
        for i in (0..chroma_width).step_by(2) {
            let x = chroma_x as isize + i as isize;
            let y = chroma_y as isize + j as isize;
            let left = chroma_x as isize + if i == 0 { 0 } else { i - 1 } as isize;
            let value = match filter_type {
                1 => {
                    sample_plane(y_plane, y_w, y_w, y_h, left, y) as i32
                        + 2 * sample_plane(y_plane, y_w, y_w, y_h, x, y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x + 1, y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, left, y + 1) as i32
                        + 2 * sample_plane(y_plane, y_w, y_w, y_h, x, y + 1) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x + 1, y + 1) as i32
                }
                2 => {
                    let top_y = if j == 0 { y } else { y - 1 };
                    sample_plane(y_plane, y_w, y_w, y_h, left, y) as i32
                        + 4 * sample_plane(y_plane, y_w, y_w, y_h, x, y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x + 1, y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x, top_y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x, y + 1) as i32
                }
                _ => {
                    (sample_plane(y_plane, y_w, y_w, y_h, x, y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x + 1, y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x, y + 1) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x + 1, y + 1) as i32)
                        << 1
                }
            };
            recon_q3[(j / 2) * uv_bw + (i / 2)] = value;
        }
    }

    recon_q3
}

fn cfl_fetch_neighbor_luma_refs_420(
    y_plane: &[u8],
    y_w: usize,
    y_h: usize,
    chroma_x: usize,
    chroma_y: usize,
    chroma_width: usize,
    chroma_height: usize,
    sb_size: usize,
    filter_type: u8,
) -> CflImplicitRefs {
    let uv_bw = chroma_width / 2;
    let uv_bh = chroma_height / 2;
    let have_top = chroma_y > 0;
    let have_left = chroma_x > 0;
    let is_top_sb_boundary = chroma_y % sb_size == 0;
    let trace = trace_block_match("WAV2C_TRACE_CFL", chroma_x / 2, chroma_y / 2);
    let mut luma_top_q3 = vec![0i32; uv_bw];
    let mut luma_left_q3 = vec![0i32; uv_bh];

    if have_top {
        let (top_offset, bottom_offset) = cfl_top_bottom_offsets(is_top_sb_boundary);
        if trace {
            let input_y = chroma_y as isize - top_offset;
            let bot_y = input_y + bottom_offset;
            let mut src_top = Vec::with_capacity(chroma_width);
            let mut src_bot = Vec::with_capacity(chroma_width);
            for i in 0..chroma_width {
                let x = chroma_x as isize + i as isize;
                src_top.push(sample_plane(y_plane, y_w, y_w, y_h, x, input_y));
                src_bot.push(sample_plane(y_plane, y_w, y_w, y_h, x, bot_y));
            }
            eprintln!("[WAV2C_CFL_FETCH_LUMA_SRC_TOP] {:?}", src_top);
            eprintln!("[WAV2C_CFL_FETCH_LUMA_SRC_BOT] {:?}", src_bot);
        }
        for i in (0..chroma_width).step_by(2) {
            let x = chroma_x as isize + i as isize;
            let left = chroma_x as isize + if i == 0 { 0 } else { i - 1 } as isize;
            let input_y = chroma_y as isize - top_offset;
            let bot_y = input_y + bottom_offset;
            let value = match filter_type {
                1 => {
                    sample_plane(y_plane, y_w, y_w, y_h, left, input_y) as i32
                        + 2 * sample_plane(y_plane, y_w, y_w, y_h, x, input_y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x + 1, input_y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, left, bot_y) as i32
                        + 2 * sample_plane(y_plane, y_w, y_w, y_h, x, bot_y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x + 1, bot_y) as i32
                }
                2 => {
                    let top_y = if is_top_sb_boundary {
                        input_y
                    } else {
                        input_y - 1
                    };
                    sample_plane(y_plane, y_w, y_w, y_h, left, input_y) as i32
                        + 4 * sample_plane(y_plane, y_w, y_w, y_h, x, input_y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x + 1, input_y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x, top_y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x, bot_y) as i32
                }
                _ => {
                    (sample_plane(y_plane, y_w, y_w, y_h, x, input_y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x + 1, input_y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x, bot_y) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x + 1, bot_y) as i32)
                        << 1
                }
            };
            luma_top_q3[i / 2] = value;
        }

        if chroma_x >= y_w {
            luma_top_q3.fill(128);
        } else if chroma_x + chroma_width > y_w {
            let temp = chroma_width - ((chroma_x + chroma_width) - y_w);
            for idx in (temp / 2)..uv_bw {
                luma_top_q3[idx] = luma_top_q3[idx.saturating_sub(1)];
            }
        }
    }

    if have_left {
        for j in (0..chroma_height).step_by(2) {
            let row = chroma_y as isize + j as isize;
            let next_row = row + 1;
            let x_m3 = chroma_x as isize - 3;
            let x_m2 = chroma_x as isize - 2;
            let x_m1 = chroma_x as isize - 1;
            let value = match filter_type {
                1 => {
                    sample_plane(y_plane, y_w, y_w, y_h, x_m3, row) as i32
                        + 2 * sample_plane(y_plane, y_w, y_w, y_h, x_m2, row) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x_m1, row) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x_m3, next_row) as i32
                        + 2 * sample_plane(y_plane, y_w, y_w, y_h, x_m2, next_row) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x_m1, next_row) as i32
                }
                2 => {
                    let top_row = if j == 0 { row } else { row - 1 };
                    sample_plane(y_plane, y_w, y_w, y_h, x_m3, row) as i32
                        + 4 * sample_plane(y_plane, y_w, y_w, y_h, x_m2, row) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x_m1, row) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x_m2, top_row) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x_m2, next_row) as i32
                }
                _ => {
                    (sample_plane(y_plane, y_w, y_w, y_h, x_m2, row) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x_m1, row) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x_m2, next_row) as i32
                        + sample_plane(y_plane, y_w, y_w, y_h, x_m1, next_row) as i32)
                        << 1
                }
            };
            luma_left_q3[j / 2] = value;
        }

        if chroma_y >= y_h {
            luma_left_q3.fill(128);
        } else if chroma_y + chroma_height > y_h {
            let temp = chroma_height - ((chroma_y + chroma_height) - y_h);
            for idx in (temp / 2)..uv_bh {
                luma_left_q3[idx] = luma_left_q3[idx.saturating_sub(1)];
            }
        }
    }

    CflImplicitRefs {
        have_top,
        have_left,
        luma_top_q3,
        luma_left_q3,
    }
}

fn cfl_fetch_neighbor_chroma_refs(
    chroma_plane: &[u8],
    uv_w: usize,
    uv_h: usize,
    uv_bx: usize,
    uv_by: usize,
    uv_bw: usize,
    uv_bh: usize,
) -> (Vec<u8>, Vec<u8>) {
    let mut top = vec![0u8; uv_bw];
    let mut left = vec![0u8; uv_bh];
    let trace = trace_block_match("WAV2C_TRACE_CFL", uv_bx, uv_by);

    if uv_by > 0 {
        if trace {
            let mut src_top = Vec::with_capacity(uv_bw);
            for x in 0..uv_bw {
                src_top.push(sample_plane(
                    chroma_plane,
                    uv_w,
                    uv_w,
                    uv_h,
                    (uv_bx + x) as isize,
                    (uv_by - 1) as isize,
                ));
            }
            eprintln!("[WAV2C_CFL_FETCH_CHROMA_SRC_TOP] {:?}", src_top);
        }
        for x in 0..uv_bw {
            top[x] = sample_plane(
                chroma_plane,
                uv_w,
                uv_w,
                uv_h,
                (uv_bx + x) as isize,
                (uv_by - 1) as isize,
            );
        }
        if uv_bx >= uv_w {
            top.fill(128);
        } else if uv_bx + uv_bw > uv_w {
            let temp = uv_bw - ((uv_bx + uv_bw) - uv_w);
            for idx in temp..uv_bw {
                top[idx] = top[idx.saturating_sub(1)];
            }
        }
    }

    if uv_bx > 0 {
        for y in 0..uv_bh {
            left[y] = sample_plane(
                chroma_plane,
                uv_w,
                uv_w,
                uv_h,
                (uv_bx - 1) as isize,
                (uv_by + y) as isize,
            );
        }
        if uv_by >= uv_h {
            left.fill(128);
        } else if uv_by + uv_bh > uv_h {
            let temp = uv_bh - ((uv_by + uv_bh) - uv_h);
            for idx in temp..uv_bh {
                left[idx] = left[idx.saturating_sub(1)];
            }
        }
    }

    (top, left)
}

fn cfl_calc_luma_dc_q3(refs: &CflImplicitRefs, width: usize, height: usize) -> i32 {
    let ss_hor = if width > 32 { 2 } else { 1 };
    let ss_ver = if height > 32 { 2 } else { 1 };
    let mut sum_x = 0i32;
    let mut count = 0usize;

    if refs.have_top {
        for idx in (0..width).step_by(ss_hor) {
            sum_x += refs.luma_top_q3[idx];
            count += 1;
        }
    }

    if refs.have_left {
        for idx in (0..height).step_by(ss_ver) {
            sum_x += refs.luma_left_q3[idx];
            count += 1;
        }
    }

    if count == 0 {
        128 * 8
    } else {
        let (scale, shift) = resolve_divisor_32(count as u32);
        let rounding = (1i32 << shift) >> 1;
        let value = ((sum_x * scale as i32) + rounding) >> shift;
        value.min((1 << 11) - 1)
    }
}

fn cfl_sample_counts(
    width: usize,
    height: usize,
    have_top: bool,
    have_left: bool,
) -> (usize, usize) {
    let (mut numb_up, mut numb_left) = if have_top && have_left {
        if width > height * 2 {
            (NUM_REF_SAM_CFL, 0)
        } else if height > width * 2 {
            (0, NUM_REF_SAM_CFL)
        } else {
            (NUM_REF_SAM_CFL >> 1, NUM_REF_SAM_CFL >> 1)
        }
    } else {
        (
            if have_top { NUM_REF_SAM_CFL } else { 0 },
            if have_left { NUM_REF_SAM_CFL } else { 0 },
        )
    };

    numb_up = numb_up.min(width);
    numb_left = numb_left.min(height);
    (numb_up, numb_left)
}

fn cfl_derive_implicit_alpha_q3(
    refs: &CflImplicitRefs,
    chroma_top: &[u8],
    chroma_left: &[u8],
    width: usize,
    height: usize,
) -> CflImplicitAlpha {
    let (numb_up, numb_left) = cfl_sample_counts(width, height, refs.have_top, refs.have_left);
    let mut count = 0i32;
    let mut sum_x = 0i32;
    let mut sum_y = 0i32;
    let mut sum_xx = 0i32;
    let mut sum_xy = 0i32;
    let mut samples = Vec::with_capacity(numb_up + numb_left);

    if numb_up > 0 {
        let step_up = (width / numb_up).max(1);
        let start_up = if step_up == 1 { 0 } else { step_up >> 1 };
        for idx in (start_up..width).step_by(step_up) {
            let x_q3 = refs.luma_top_q3[idx];
            let y = chroma_top[idx];
            let x_q0 = x_q3 >> 3;
            sum_x += x_q0;
            sum_y += y as i32;
            sum_xy += x_q0 * y as i32;
            sum_xx += x_q0 * x_q0;
            count += 1;
            samples.push(CflImplicitSample {
                side: "top",
                idx,
                x_q3,
                y,
            });
        }
    }

    if numb_left > 0 {
        let step_left = (height / numb_left).max(1);
        let start_left = if step_left == 1 { 0 } else { step_left >> 1 };
        for idx in (start_left..height).step_by(step_left) {
            let x_q3 = refs.luma_left_q3[idx];
            let y = chroma_left[idx];
            let x_q0 = x_q3 >> 3;
            sum_x += x_q0;
            sum_y += y as i32;
            sum_xy += x_q0 * y as i32;
            sum_xx += x_q0 * x_q0;
            count += 1;
            samples.push(CflImplicitSample {
                side: "left",
                idx,
                x_q3,
                y,
            });
        }
    }

    CflImplicitAlpha {
        alpha_q3: derive_linear_parameters_alpha_cfl(sum_x, sum_y, sum_xx, sum_xy, count, 8),
        numb_up,
        numb_left,
        samples,
    }
}

const MHCCP_LINE_NUM: usize = 1;
const MHCCP_NUM_PARAMS: usize = 3;
const MHCCP_DECIM_BITS: i32 = 16;
const MHCCP_BUF_STRIDE: usize = 256;
const MHCCP_BUF_SQUARE: usize = MHCCP_BUF_STRIDE * MHCCP_BUF_STRIDE;

#[derive(Clone, Debug)]
struct MhccpRefWindow {
    have_top: bool,
    have_left: bool,
    above_lines: usize,
    left_lines: usize,
    ref_width: usize,
    ref_height: usize,
    stride: usize,
    luma_q3: Vec<u16>,
    chroma: Vec<u16>,
}

fn ceil_log2_usize(value: usize) -> i32 {
    if value <= 1 {
        0
    } else {
        usize::BITS as i32 - (value - 1).leading_zeros() as i32
    }
}

fn mhccp_pixel_is_ready(
    ready: Option<&[bool]>,
    stride: usize,
    frame_h: usize,
    x: usize,
    y: usize,
) -> bool {
    x < stride && y < frame_h && ready.map_or(true, |mask| mask[y * stride + x])
}

fn mhccp_count_available_horizontal(
    ready: Option<&[bool]>,
    stride: usize,
    frame_h: usize,
    x: usize,
    y: usize,
    len: usize,
) -> usize {
    if y >= frame_h || x >= stride {
        return 0;
    }
    let limit = len.min(stride - x);
    let mut count = 0usize;
    while count < limit && mhccp_pixel_is_ready(ready, stride, frame_h, x + count, y) {
        count += 1;
    }
    count
}

fn mhccp_count_available_vertical(
    ready: Option<&[bool]>,
    stride: usize,
    frame_h: usize,
    x: usize,
    y: usize,
    len: usize,
) -> usize {
    if y >= frame_h || x >= stride {
        return 0;
    }
    let limit = len.min(frame_h - y);
    let mut count = 0usize;
    while count < limit && mhccp_pixel_is_ready(ready, stride, frame_h, x, y + count) {
        count += 1;
    }
    count
}

fn mhccp_should_fill(
    row: usize,
    col: usize,
    above_lines: usize,
    left_lines: usize,
    width: usize,
    height: usize,
) -> bool {
    !((row >= above_lines && col >= left_lines + width)
        || (row >= above_lines + height && col >= left_lines))
}


fn mhccp_sample_chroma_input(
    chroma_plane: &[u8],
    uv_w: usize,
    uv_h: usize,
    origin_uv_x: usize,
    origin_uv_y: usize,
    sample_x: isize,
    sample_y: isize,
    uv_bx: usize,
    uv_by: usize,
    uv_bw: usize,
    uv_bh: usize,
    dc_pred: &[u8],
) -> u16 {
    let abs_x = origin_uv_x as isize + sample_x;
    let abs_y = origin_uv_y as isize + sample_y;
    if abs_x >= uv_bx as isize
        && abs_x < (uv_bx + uv_bw) as isize
        && abs_y >= uv_by as isize
        && abs_y < (uv_by + uv_bh) as isize
    {
        let local_x = (abs_x as usize) - uv_bx;
        let local_y = (abs_y as usize) - uv_by;
        dc_pred[local_y * uv_bw + local_x] as u16
    } else {
        sample_plane(chroma_plane, uv_w, uv_w, uv_h, abs_x, abs_y) as u16
    }
}

fn mhccp_non_linear(value: u16, mid: u16, bit_depth: usize) -> u16 {
    (((value as u32 * value as u32) + mid as u32) >> bit_depth) as u16
}

fn mul_fixed32_adapt(a: i32, b: i32, shift: i32) -> i32 {
    let ua = a.unsigned_abs();
    let ub = b.unsigned_abs();
    let bits_a = 31 - (ua | 1).leading_zeros() as i32 + 1;
    let bits_b = 31 - (ub | 1).leading_zeros() as i32 + 1;
    let bits_limit = 29;

    let mut need = bits_a + bits_b - bits_limit;
    if need < 0 {
        need = 0;
    }

    let s1 = need >> 1;
    let s2 = need - s1;
    let adj = shift - (s1 + s2);
    let a_sh = if s1 > 0 { a >> s1 } else { a };
    let b_sh = if s2 > 0 { b >> s2 } else { b };
    let prod = a_sh.saturating_mul(b_sh);

    if adj <= 0 {
        return prod;
    }

    let bias = if adj <= bits_limit {
        1u32 << (adj - 1)
    } else {
        0
    };
    if prod >= 0 {
        if adj <= bits_limit {
            ((prod as u32 + bias) >> adj) as i32
        } else {
            0
        }
    } else if adj <= bits_limit {
        -(((prod.unsigned_abs() + bias) >> adj) as i32)
    } else {
        0
    }
}

fn floor_log2_u64(mut value: u64) -> i32 {
    if value == 0 {
        return 0;
    }
    let mut result = 0i32;
    if value & 0xffffffff00000000 != 0 {
        value >>= 32;
        result += 32;
    }
    if value & 0xffff0000 != 0 {
        value >>= 16;
        result += 16;
    }
    if value & 0xff00 != 0 {
        value >>= 8;
        result += 8;
    }
    if value & 0xf0 != 0 {
        value >>= 4;
        result += 4;
    }
    if value & 0xc != 0 {
        value >>= 2;
        result += 2;
    }
    if value & 0x2 != 0 {
        result += 1;
    }
    result
}

fn mhccp_get_division_scale_shift(denom: u32) -> (i32, i32, i32) {
    const DIV_PREC_BITS: i32 = 14;
    const DIV_PREC_BITS_POW2: usize = 8;
    const DIV_SLOT_BITS: i32 = 3;
    const DIV_INTR_BITS: i32 = DIV_PREC_BITS - DIV_SLOT_BITS;
    const POW2_W: [i32; DIV_PREC_BITS_POW2] = [214, 153, 113, 86, 67, 53, 43, 35];
    const POW2_O: [i32; DIV_PREC_BITS_POW2] = [4822, 5952, 6624, 6792, 6408, 5424, 3792, 1466];
    const POW2_B: [i32; DIV_PREC_BITS_POW2] =
        [12784, 12054, 11670, 11583, 11764, 12195, 12870, 13782];

    let shift = floor_log2_u64(denom as u64);
    let round = if shift == 0 {
        0
    } else {
        (1u32 << shift >> 1) as i32
    };
    let delta = shift - DIV_PREC_BITS;
    let norm_diff_tmp = if delta >= 0 {
        let bias = if delta > 0 {
            (1u32 << (delta - 1)) as i32
        } else {
            0
        };
        ((denom as i32) + bias) >> delta
    } else {
        (denom as i32) << -delta
    };
    let norm_diff_clip = norm_diff_tmp.clamp(1, (1 << (DIV_PREC_BITS + 1)) - 1);
    let norm_diff = norm_diff_clip & ((1 << DIV_PREC_BITS) - 1);
    let index = (norm_diff >> DIV_INTR_BITS) as usize;
    let norm_diff2 = norm_diff - POW2_O[index];
    let mut scale = ((POW2_W[index] * ((norm_diff2 * norm_diff2) >> DIV_PREC_BITS))
        >> DIV_PREC_BITS_POW2 as i32)
        - (norm_diff2 >> 1)
        + POW2_B[index];
    scale <<= MHCCP_DECIM_BITS - DIV_PREC_BITS;
    (scale, round, shift)
}

fn mhccp_gauss_back_substitute(
    x: &mut [i32; MHCCP_NUM_PARAMS],
    c: &[[i32; MHCCP_NUM_PARAMS + 1]; MHCCP_NUM_PARAMS],
    num_eq: usize,
    col: usize,
    bits: i32,
) {
    x[num_eq - 1] = c[num_eq - 1][col];
    for i in (0..(num_eq - 1)).rev() {
        let mut value = c[i][col];
        for j in (i + 1)..num_eq {
            value -= mul_fixed32_adapt(c[i][j], x[j], bits);
        }
        x[i] = value;
    }
}

fn mhccp_gauss_elimination(
    a: &[[i32; MHCCP_NUM_PARAMS]; MHCCP_NUM_PARAMS],
    y: &[i32; MHCCP_NUM_PARAMS],
    bit_depth: usize,
) -> [i32; MHCCP_NUM_PARAMS] {
    let num_eq = MHCCP_NUM_PARAMS;
    let col = num_eq;
    let reg = 2 << (bit_depth.saturating_sub(8));
    let mut c = [[0i32; MHCCP_NUM_PARAMS + 1]; MHCCP_NUM_PARAMS];

    for i in 0..num_eq {
        for j in 0..num_eq {
            c[i][j] = if j >= i { a[i][j] } else { a[j][i] };
        }
        c[i][i] += reg as i32;
        c[i][col] = y[i];
    }

    for i in 0..num_eq {
        let diag = c[i][i].abs().max(1) as u32;
        let (scale, _, shift) = mhccp_get_division_scale_shift(diag);
        for j in (i + 1)..=num_eq {
            c[i][j] = mul_fixed32_adapt(c[i][j], scale, shift);
        }
        for j in (i + 1)..num_eq {
            let scale_factor = c[j][i];
            for k in (i + 1)..=num_eq {
                let delta = mul_fixed32_adapt(scale_factor, c[i][k], MHCCP_DECIM_BITS);
                c[j][k] -= delta;
            }
        }
    }

    let mut x = [0i32; MHCCP_NUM_PARAMS];
    mhccp_gauss_back_substitute(&mut x, &c, num_eq, col, MHCCP_DECIM_BITS);
    x
}

fn mhccp_convolve(params: &[i32; MHCCP_NUM_PARAMS], vector: &[u16; MHCCP_NUM_PARAMS]) -> i16 {
    let mut sum = 0i32;
    for i in 0..MHCCP_NUM_PARAMS {
        sum += mul_fixed32_adapt(params[i], vector[i] as i32, MHCCP_DECIM_BITS);
    }
    sum.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

fn mhccp_build_ref_window(
    y_plane: &[u8],
    _y_ready: &[bool],
    y_w: usize,
    y_h: usize,
    chroma_plane: &[u8],
    chroma_ready: &[bool],
    uv_w: usize,
    uv_h: usize,
    uv_bx: usize,
    uv_by: usize,
    uv_bw: usize,
    uv_bh: usize,
    dc_pred: &[u8],
    cfl_ds_filter_index: u8,
    sb_size: usize,
    plane_name: char,
    luma_scratch: &mut [u16],
    chroma_scratch: &mut [u16],
) -> MhccpRefWindow {
    let have_top = uv_by > 0
        && mhccp_count_available_horizontal(Some(chroma_ready), uv_w, uv_h, uv_bx, uv_by - 1, 1)
            > 0;
    let have_left = uv_bx > 0
        && mhccp_count_available_vertical(Some(chroma_ready), uv_w, uv_h, uv_bx - 1, uv_by, 1) > 0;
    let above_lines = if have_top {
        (MHCCP_LINE_NUM + 1).min(uv_by)
    } else {
        0
    };
    let left_lines = if have_left {
        (MHCCP_LINE_NUM + 1).min(uv_bx)
    } else {
        0
    };
    let top_right = if have_top && uv_bw > 2 && uv_bx + uv_bw < uv_w {
        mhccp_count_available_horizontal(
            Some(chroma_ready),
            uv_w,
            uv_h,
            uv_bx + uv_bw,
            uv_by - 1,
            uv_bw,
        )
        .min(uv_bw)
    } else {
        0
    };
    let bottom_left = if have_left && uv_bh > 2 && uv_by + uv_bh < uv_h {
        mhccp_count_available_vertical(
            Some(chroma_ready),
            uv_w,
            uv_h,
            uv_bx - 1,
            uv_by + uv_bh,
            uv_bh,
        )
        .min(uv_bh)
    } else {
        0
    };

    let origin_uv_x = uv_bx.saturating_sub(left_lines);
    let origin_uv_y = uv_by.saturating_sub(above_lines);
    let ref_width = (left_lines + uv_bw + top_right)
        .min(64)
        .min(uv_w.saturating_sub(origin_uv_x));
    let ref_height = (above_lines + uv_bh + bottom_left)
        .min(64)
        .min(uv_h.saturating_sub(origin_uv_y));
    let trace = trace_block_match("WAV2C_TRACE_MHCCP", uv_bx, uv_by);
    let is_top_sb_boundary = (uv_by * 2) % sb_size == 0;

    let origin_lx = origin_uv_x * 2;
    let origin_ly = origin_uv_y * 2;
    let luma_above_lines = above_lines * 2;
    let _luma_left_lines = left_lines * 2;
    let luma_ref_width = ref_width * 2;
    let luma_ref_height = ref_height * 2;
    let luma_width = uv_bw * 2;
    let luma_height = uv_bh * 2;

    for row in 0..ref_height {
        for col in 0..ref_width {
            if !mhccp_should_fill(row, col, above_lines, left_lines, uv_bw, uv_bh) {
                continue;
            }

            let h = row * 2;
            let w = col * 2;
            let mut ref_h_t_off = 0isize;
            let mut ref_h_c_off = 0isize;
            let mut ref_h_b_off = 0isize;
            let ref_w_off = 0isize;
            if luma_above_lines == ((MHCCP_LINE_NUM + 1) << 1)
                && is_top_sb_boundary
                && h < luma_above_lines
            {
                ref_h_t_off = if h != 0 {
                    (((MHCCP_LINE_NUM + 1) << 1) - h) as isize
                } else {
                    (((MHCCP_LINE_NUM + 1) << 1) - (h + 1)) as isize
                };
                ref_h_c_off = (((MHCCP_LINE_NUM + 1) << 1) - (h + 1)) as isize;
                ref_h_b_off = (((MHCCP_LINE_NUM + 1) << 1) - (h + 2)) as isize;
            }
            let center_x = w as isize + ref_w_off;
            let center_y = h as isize + ref_h_c_off;
            let left_x = w.saturating_sub(1) as isize + ref_w_off;
            let right_x = w as isize + 1 + ref_w_off;
            let top_y = if h != 0 { h as isize - 1 } else { h as isize } + ref_h_t_off;
            let bottom_y = h as isize + 1 + ref_h_b_off;
            let center = sample_plane(
                y_plane,
                y_w,
                y_w,
                y_h,
                origin_lx as isize + center_x,
                origin_ly as isize + center_y,
            ) as u16;
            let right = sample_plane(
                y_plane,
                y_w,
                y_w,
                y_h,
                origin_lx as isize + right_x,
                origin_ly as isize + center_y,
            ) as u16;
            let down = sample_plane(
                y_plane,
                y_w,
                y_w,
                y_h,
                origin_lx as isize + center_x,
                origin_ly as isize + bottom_y,
            ) as u16;
            let down_right = sample_plane(
                y_plane,
                y_w,
                y_w,
                y_h,
                origin_lx as isize + right_x,
                origin_ly as isize + bottom_y,
            ) as u16;
            let left = sample_plane(
                y_plane,
                y_w,
                y_w,
                y_h,
                origin_lx as isize + left_x,
                origin_ly as isize + center_y,
            ) as u16;
            let down_left = sample_plane(
                y_plane,
                y_w,
                y_w,
                y_h,
                origin_lx as isize + left_x,
                origin_ly as isize + bottom_y,
            ) as u16;
            let top = sample_plane(
                y_plane,
                y_w,
                y_w,
                y_h,
                origin_lx as isize + center_x,
                origin_ly as isize + top_y,
            ) as u16;

            let value = match cfl_ds_filter_index {
                1 => left + 2 * center + right + down_left + 2 * down + down_right,
                2 => left + 4 * center + right + top + down,
                _ => (center + right + down + down_right) << 1,
            };
            luma_scratch[row * MHCCP_BUF_STRIDE + col] = value;
            let mut ref_h_offset = 0isize;
            let mut ref_w_offset = 0isize;
            if above_lines == (MHCCP_LINE_NUM + 1) {
                if is_top_sb_boundary && row < above_lines {
                    ref_h_offset = (MHCCP_LINE_NUM + 1 - (row + 1)) as isize;
                } else if row == 0 {
                    ref_h_offset = 1;
                }
            }
            if left_lines == (MHCCP_LINE_NUM + 1) && col == 0 {
                ref_w_offset = 1;
            }
            chroma_scratch[row * MHCCP_BUF_STRIDE + col] = mhccp_sample_chroma_input(
                chroma_plane,
                uv_w,
                uv_h,
                origin_uv_x,
                origin_uv_y,
                col as isize + ref_w_offset,
                row as isize + ref_h_offset,
                uv_bx,
                uv_by,
                uv_bw,
                uv_bh,
                dc_pred,
            );
        }
    }

    let stride = ref_width;
    let mut luma_q3 = vec![0u16; stride * ref_height];
    let mut chroma = vec![0u16; stride * ref_height];
    for row in 0..ref_height {
        let src = row * MHCCP_BUF_STRIDE;
        let dst = row * stride;
        luma_q3[dst..dst + ref_width].copy_from_slice(&luma_scratch[src..src + ref_width]);
        chroma[dst..dst + ref_width].copy_from_slice(&chroma_scratch[src..src + ref_width]);
    }

    if trace {
        eprintln!(
            "[WAV2C_MHCCP_SETUP] plane={} uv=({}, {}) size={}x{} above={} left={} ref={}x{} top_sb={}",
            plane_name,
            uv_bx,
            uv_by,
            uv_bw,
            uv_bh,
            above_lines,
            left_lines,
            ref_width,
            ref_height,
            is_top_sb_boundary
        );
        for row in 0..ref_height {
            let start = row * stride;
            let end = start + ref_width;
            eprintln!(
                "[WAV2C_MHCCP_LUMA_BUF_ROW] plane={} y={} {:?}",
                plane_name,
                row,
                &luma_q3[start..end]
            );
            eprintln!(
                "[WAV2C_MHCCP_CHROMA_BUF_ROW] plane={} y={} {:?}",
                plane_name,
                row,
                &chroma[start..end]
            );
        }
        let _ = (
            luma_ref_width,
            luma_ref_height,
            luma_width,
            luma_height,
            _y_ready,
        );
    }

    MhccpRefWindow {
        have_top,
        have_left,
        above_lines,
        left_lines,
        ref_width,
        ref_height,
        stride,
        luma_q3,
        chroma,
    }
}

fn mhccp_derive_params(
    window: &MhccpRefWindow,
    mh_dir: u8,
    plane_name: char,
    uv_bx: usize,
    uv_by: usize,
) -> [i32; MHCCP_NUM_PARAMS] {
    let mut ata = [[0i32; MHCCP_NUM_PARAMS]; MHCCP_NUM_PARAMS];
    let mut ty = [0i32; MHCCP_NUM_PARAMS];
    let mid = 1u16 << 7;
    let trace = trace_block_match("WAV2C_TRACE_MHCCP", uv_bx, uv_by);
    let mut count = 0usize;

    if window.ref_width > 0 && window.ref_height > 0 {
        for row in 1..window.ref_height.saturating_sub(1) {
            for col in 1..window.ref_width.saturating_sub(1) {
                if col >= window.left_lines && row >= window.above_lines {
                    continue;
                }
                let idx = row * window.stride + col;
                let center = window.luma_q3[idx] >> 3;
                let base = match mh_dir {
                    1 => window.luma_q3[(row - 1) * window.stride + col] >> 3,
                    2 => window.luma_q3[idx - 1] >> 3,
                    _ => center,
                };
                let vector = [
                    base as i32,
                    mhccp_non_linear(center, mid, 8) as i32,
                    mid as i32,
                ];
                let y_value = window.chroma[idx] as i32;
                if trace {
                    eprintln!(
                        "[WAV2C_MHCCP_SAMPLE] plane={} idx={} ij=({}, {}) a=({},{},{}) y={}",
                        plane_name, count, col, row, vector[0], vector[1], vector[2], y_value
                    );
                }
                for i in 0..MHCCP_NUM_PARAMS {
                    ty[i] += vector[i] * y_value;
                    for j in i..MHCCP_NUM_PARAMS {
                        ata[i][j] += vector[i] * vector[j];
                    }
                }
                count += 1;
            }
        }
    }

    if count == 0 {
        let params = [0, 0, 1 << MHCCP_DECIM_BITS];
        if trace {
            eprintln!(
                "[WAV2C_MHCCP_PARAMS] plane={} uv=({}, {}) count=0 params={:?}",
                plane_name, uv_bx, uv_by, params
            );
        }
        return params;
    }

    let matrix_shift = (MHCCP_DECIM_BITS + 6) - 16 - ceil_log2_usize(count);
    if matrix_shift > 0 {
        for i in 0..MHCCP_NUM_PARAMS {
            ty[i] <<= matrix_shift;
            for j in i..MHCCP_NUM_PARAMS {
                ata[i][j] <<= matrix_shift;
            }
        }
    } else if matrix_shift < 0 {
        let shift = -matrix_shift;
        for i in 0..MHCCP_NUM_PARAMS {
            ty[i] >>= shift;
            for j in i..MHCCP_NUM_PARAMS {
                ata[i][j] >>= shift;
            }
        }
    }

    let params = mhccp_gauss_elimination(&ata, &ty, 8);
    if trace {
        eprintln!(
            "[WAV2C_MHCCP_NORMALS] plane={} ATA=(({}, {}, {}), ({}, {}, {}), ({}, {}, {})) Ty=({}, {}, {})",
            plane_name,
            ata[0][0],
            ata[0][1],
            ata[0][2],
            ata[1][0],
            ata[1][1],
            ata[1][2],
            ata[2][0],
            ata[2][1],
            ata[2][2],
            ty[0],
            ty[1],
            ty[2]
        );
        eprintln!(
            "[WAV2C_MHCCP_PARAMS] plane={} uv=({}, {}) count={} params={:?}",
            plane_name, uv_bx, uv_by, count, params
        );
    }
    params
}

fn mhccp_predict_plane(
    window: &MhccpRefWindow,
    uv_bw: usize,
    uv_bh: usize,
    mh_dir: u8,
    params: [i32; MHCCP_NUM_PARAMS],
    plane_name: char,
    uv_bx: usize,
    uv_by: usize,
) -> Vec<u8> {
    let mut pred = vec![128u8; uv_bw * uv_bh];
    let mid = 1u16 << 7;
    let trace = trace_block_match("WAV2C_TRACE_MHCCP", uv_bx, uv_by);

    for row in 0..uv_bh {
        let src_row = window.above_lines + row;
        for col in 0..uv_bw {
            let src_col = window.left_lines + col;
            let idx = src_row * window.stride + src_col;
            let center = window.luma_q3[idx] >> 3;
            let above = if row == 0 && !window.have_top {
                center
            } else {
                window.luma_q3[idx - window.stride] >> 3
            };
            let left = if col == 0 && !window.have_left {
                center
            } else {
                window.luma_q3[idx - 1] >> 3
            };
            let mut vector = [center, mhccp_non_linear(center, mid, 8), mid];
            if mh_dir == 1 {
                vector[0] = above;
            } else if mh_dir == 2 {
                vector[0] = left;
            }
            let value = mhccp_convolve(&params, &vector).clamp(0, 255) as u8;
            pred[row * uv_bw + col] = value;
            if trace {
                eprintln!(
                    "[WAV2C_MHCCP_PRED] plane={} uv=({}, {}) ij=({}, {}) vec=({}, {}, {}) pred={} params=({}, {}, {})",
                    plane_name,
                    uv_bx,
                    uv_by,
                    col,
                    row,
                    vector[0],
                    vector[1],
                    vector[2],
                    value,
                    params[0],
                    params[1],
                    params[2]
                );
            }
        }
    }

    pred
}

fn reconstruct_tu(
    recon: &mut [u8],
    stride: usize,
    tu_x: usize,
    tu_y: usize,
    tu_w: usize,
    tu_h: usize,
    pred: &[u8],
    pred_w: usize,
    pred_x_offset: usize,
    pred_y_offset: usize,
    residuals: &[i32],
) {
    for row in 0..tu_h {
        for col in 0..tu_w {
            let pred_val = pred[(pred_y_offset + row) * pred_w + pred_x_offset + col] as i32;
            let res_val = residuals[row * tu_w + col];
            let pixel = (pred_val + res_val).clamp(0, 255) as u8;
            recon[(tu_y + row) * stride + tu_x + col] = pixel;
        }
    }
}

fn apply_dpcm_cumulative(residuals: &mut [i32], w: usize, h: usize, vertical: bool) {
    if vertical {
        for col in 0..w {
            let mut acc = 0i32;
            for row in 0..h {
                acc += residuals[row * w + col];
                residuals[row * w + col] = acc;
            }
        }
    } else {
        for row in 0..h {
            let mut acc = 0i32;
            for col in 0..w {
                acc += residuals[row * w + col];
                residuals[row * w + col] = acc;
            }
        }
    }
}

fn lossless_luma_residual(block: &tile::DecodedBlock, tu: &tile::TuData) -> Vec<i32> {
    let mut residuals = transform::lossless_residual(&tu.levels, tu.eob);
    if block.use_dpcm_y && tu.tx_size == crate::block::TxSize::Tx4x4 {
        if block.y_mode == 1 {
            apply_dpcm_cumulative(&mut residuals, 4, 4, true);
        } else if block.y_mode == 2 {
            apply_dpcm_cumulative(&mut residuals, 4, 4, false);
        }
    }
    residuals
}

fn lossless_chroma_residual(block: &tile::DecodedBlock, tu: &tile::TuData) -> Vec<i32> {
    let mut residuals = transform::lossless_residual(&tu.levels, tu.eob);
    if block.use_dpcm_uv && tu.tx_size == crate::block::TxSize::Tx4x4 {
        if block.uv_mode == 1 {
            apply_dpcm_cumulative(&mut residuals, 4, 4, true);
        } else if block.uv_mode == 2 {
            apply_dpcm_cumulative(&mut residuals, 4, 4, false);
        }
    }
    residuals
}

fn trace_uv_recon_tu(
    plane: usize,
    tu_x: usize,
    tu_y: usize,
    tu_w: usize,
    tu_h: usize,
    tx_size: crate::block::TxSize,
    tx_type: usize,
    eob: u16,
    levels: &[i32],
    dqcoeffs: &[i32],
    residuals: &[i32],
    pred: &[u8],
    pred_w: usize,
    pred_x_offset: usize,
    pred_y_offset: usize,
    recon: &[u8],
    recon_stride: usize,
) {
    if !trace_block_match("WAV2C_TRACE_UV_RECON", tu_x, tu_y) {
        return;
    }

    let scan_len = usize::from(eob).min(levels.len());
    eprintln!(
        "[WAV2C_UV_RECON] plane={} uv=({}, {}) size={}x{} tx={:?} tx_type={} eob={} levels={:?}",
        plane,
        tu_x,
        tu_y,
        tu_w,
        tu_h,
        tx_size,
        tx_type,
        eob,
        &levels[..scan_len]
    );
    eprintln!("[WAV2C_UV_RECON] plane={} dq={:?}", plane, dqcoeffs);
    for row in 0..tu_h {
        let pred_row: Vec<u8> = (0..tu_w)
            .map(|col| pred[(pred_y_offset + row) * pred_w + pred_x_offset + col])
            .collect();
        let res_row: Vec<i32> = (0..tu_w).map(|col| residuals[row * tu_w + col]).collect();
        let recon_row: Vec<u8> = (0..tu_w)
            .map(|col| recon[(tu_y + row) * recon_stride + tu_x + col])
            .collect();
        eprintln!(
            "[WAV2C_UV_RECON] plane={} row={} pred={:?} res={:?} recon={:?}",
            plane, row, pred_row, res_row, recon_row
        );
    }
}

fn mark_region_ready(
    ready: &mut [bool],
    stride: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) {
    for row in 0..height {
        let row_start = (y + row) * stride + x;
        ready[row_start..row_start + width].fill(true);
    }
}

fn mark_region_filter(
    filter_map: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    filter_type: u8,
) {
    for row in 0..height {
        let row_start = (y + row) * stride + x;
        filter_map[row_start..row_start + width].fill(filter_type);
    }
}

fn neighbor_filter_types(filter_map: &[u8], stride: usize, x: usize, y: usize) -> (usize, usize) {
    let above = if y > 0 {
        filter_map[(y - 1) * stride + x] as usize
    } else {
        0
    };
    let left = if x > 0 {
        filter_map[y * stride + x - 1] as usize
    } else {
        0
    };
    (above, left)
}


fn uv_mode_filter_type(mode: u8) -> u8 {
    matches!(mode, 9 | 10 | 11) as u8
}

const BLK_SIZE_LOG2_BAWP: [usize; 17] = [0, 0, 0, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4];
const LOG_TO_BLK_SIZE_BAWP: [usize; 5] = [0, 2, 4, 8, 16];

fn derive_number_ref_samples_bawp(
    above_valid: bool,
    left_valid: bool,
    width: usize,
    height: usize,
) -> (usize, usize) {
    let above_available = if width != 0 { above_valid } else { false };
    let left_available = if height != 0 { left_valid } else { false };

    if above_available && left_available {
        if width == 16 && height == 16 {
            (16, 16)
        } else if width > 4 && height > 4 {
            (8, 8)
        } else if width < 16 && height < 16 {
            (4, 4)
        } else if width == 16 {
            (16, 0)
        } else {
            (0, 16)
        }
    } else if above_available {
        (width, 0)
    } else if left_available {
        (0, height)
    } else {
        (0, 0)
    }
}

fn derive_linear_parameters_alpha(
    sum_x: i32,
    sum_y: i32,
    sum_xx: i32,
    sum_xy: i32,
    count: i32,
    shift: i32,
) -> i32 {
    if count == 0 {
        return 0;
    }
    let der = sum_xx as i64 - ((sum_x as i64 * sum_x as i64) / count as i64);
    let nor = sum_xy as i64 - ((sum_x as i64 * sum_y as i64) / count as i64);
    if der == 0 || nor == 0 {
        return 0;
    }
    let mut alpha = (nor << shift) / der;
    let alpha_max = ((2i64) << shift) - 1;
    if alpha > alpha_max {
        alpha = alpha_max;
    }
    if alpha < -alpha_max {
        alpha = -alpha_max;
    }
    alpha as i32
}

fn derive_linear_parameters_beta(
    sum_x: i32,
    sum_y: i32,
    count: i32,
    shift: i32,
    alpha: i32,
) -> i32 {
    if count == 0 {
        return 0;
    }
    ((((sum_y as i64) << shift) - (sum_x as i64 * alpha as i64)) / count as i64) as i32
}

fn apply_intrabc_morph_pred_luma(
    plane: &[u8],
    stride: usize,
    frame_w: usize,
    frame_h: usize,
    bx: usize,
    by: usize,
    bw: usize,
    bh: usize,
    src_x: i32,
    src_y: i32,
    pred: &mut [u8],
) {
    if src_x < 0 || src_y < 0 || bx >= frame_w || by >= frame_h {
        return;
    }
    let sx = src_x as usize;
    let sy = src_y as usize;
    if sx >= frame_w || sy >= frame_h {
        return;
    }

    let mut ref_w = bw.min(frame_w - bx);
    let mut ref_h = bh.min(frame_h - by);
    ref_w = ref_w.min(16);
    ref_h = ref_h.min(16);
    if ref_w == 0 || ref_h == 0 {
        return;
    }

    let width = LOG_TO_BLK_SIZE_BAWP[BLK_SIZE_LOG2_BAWP[ref_w]];
    let height = LOG_TO_BLK_SIZE_BAWP[BLK_SIZE_LOG2_BAWP[ref_h]];
    let (numb_up, numb_left) = derive_number_ref_samples_bawp(by > 0, bx > 0, width, height);

    let mut count = 0i32;
    let mut sum_x = 0i32;
    let mut sum_y = 0i32;
    let mut sum_xy = 0i32;
    let mut sum_xx = 0i32;

    let mut ref_pad = [0i32; 16];
    let mut recon_pad = [0i32; 16];

    if numb_up > 0 {
        if by == 0 || sy == 0 || bx + ref_w > frame_w || sx + ref_w > frame_w {
            return;
        }
        for i in 0..ref_w {
            ref_pad[i] = plane[(sy - 1) * stride + sx + i] as i32;
            recon_pad[i] = plane[(by - 1) * stride + bx + i] as i32;
        }
        let delta_w = width - ref_w;
        for i in 0..delta_w {
            ref_pad[i + ref_w] = ref_pad[i];
            recon_pad[i + ref_w] = recon_pad[i];
        }
        let step = width / numb_up;
        let start = if step == 1 { 0 } else { step >> 1 };
        let mut i = start;
        while i < width {
            sum_x += ref_pad[i];
            sum_y += recon_pad[i];
            sum_xy += ref_pad[i] * recon_pad[i];
            sum_xx += ref_pad[i] * ref_pad[i];
            i += step;
        }
        count += numb_up as i32;
    }

    if numb_left > 0 {
        if bx == 0 || sx == 0 || by + ref_h > frame_h || sy + ref_h > frame_h {
            return;
        }
        for i in 0..ref_h {
            ref_pad[i] = plane[(sy + i) * stride + sx - 1] as i32;
            recon_pad[i] = plane[(by + i) * stride + bx - 1] as i32;
        }
        let delta_h = height - ref_h;
        for i in 0..delta_h {
            ref_pad[i + ref_h] = ref_pad[i];
            recon_pad[i + ref_h] = recon_pad[i];
        }
        let step_left = height / numb_left;
        let start_left = if step_left == 1 { 0 } else { step_left >> 1 };
        let mut i = start_left;
        while i < height {
            sum_x += ref_pad[i];
            sum_y += recon_pad[i];
            sum_xy += ref_pad[i] * recon_pad[i];
            sum_xx += ref_pad[i] * ref_pad[i];
            i += step_left;
        }
        count += numb_left as i32;
    }

    let shift = 8i32;
    let (alpha, beta) = if count > 0 {
        let mut alpha = derive_linear_parameters_alpha(sum_x, sum_y, sum_xx, sum_xy, count, shift);
        if alpha == 0 {
            alpha = 1 << shift;
        }
        let beta = derive_linear_parameters_beta(sum_x, sum_y, count, shift, alpha);
        (alpha, beta)
    } else {
        (1 << shift, -(1 << (shift - 1)))
    };

    for j in 0..bh {
        for i in 0..bw {
            let idx = j * bw + i;
            let v = ((pred[idx] as i32 * alpha + beta) >> shift).clamp(0, 255) as u8;
            pred[idx] = v;
        }
    }
}

fn trace_uv_write(
    block_idx: usize,
    plane: usize,
    stage: &str,
    x: usize,
    y: usize,
    value: u8,
    block: &tile::DecodedBlock,
) {
    if trace_block_match("WAV2C_TRACE_UV_WRITE", x, y) {
        eprintln!(
            "[WAV2C_UV_WRITE] idx={} plane={} stage={} uv=({}, {}) value={} tree={:?} is_chroma_ref={} luma=({}, {}) {}x{} chroma=({}, {}) {}x{} uv_mode={} cfl_idx={} mh_dir={} u_tu={} v_tu={}",
            block_idx,
            plane,
            stage,
            x,
            y,
            value,
            block.tree_type,
            block.is_chroma_ref,
            block.x,
            block.y,
            block.width,
            block.height,
            block.chroma_x,
            block.chroma_y,
            block.chroma_width,
            block.chroma_height,
            block.uv_mode,
            block.cfl_idx,
            block.mhccp_dir,
            block.u_tu.len(),
            block.v_tu.len()
        );
    }
}

fn reconstruct_partitioned_frame(
    blocks: &[tile::DecodedBlock],
    width: u32,
    height: u32,
    dequant: i32,
    uv_dequant: i32,
    tcq_mode: bool,
    coded_lossless: bool,
    cfl_ds_filter_index: u8,
    sb_size: usize,
    enable_ibp: bool,
    disable_edge_filter: bool,
    qm_levels: [u8; 3],
    ccso_params: &frame::CcsoParams,
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let w = width as usize;
    let h = height as usize;
    let uv_w = w / 2;
    let uv_h = h / 2;
    let mut y_plane = vec![128u8; w * h];
    let mut u_plane = vec![128u8; uv_w * uv_h];
    let mut v_plane = vec![128u8; uv_w * uv_h];
    let mut y_ready = vec![false; w * h];
    let mut u_ready = vec![false; uv_w * uv_h];
    let mut v_ready = vec![false; uv_w * uv_h];
    let mut uv_filter = vec![0u8; uv_w * uv_h];
    let mut mhccp_luma_scratch = vec![0u16; MHCCP_BUF_SQUARE];
    let mut mhccp_u_scratch = vec![0u16; MHCCP_BUF_SQUARE];
    let mut mhccp_v_scratch = vec![0u16; MHCCP_BUF_SQUARE];

    let mi_cols = (w + 3) / 4;
    let mi_rows = (h + 3) / 4;
    let mut joint_grid = vec![0u8; mi_cols * mi_rows];

    for (block_idx, block) in blocks.iter().enumerate() {
        let bx = block.x;
        let by = block.y;
        let bw = block.width;
        let bh = block.height;
        let block_uv_x = bx / 2;
        let block_uv_y = by / 2;
        let block_uv_w = bw / 2;
        let block_uv_h = bh / 2;
        let chroma_uv_x = block.chroma_x / 2;
        let chroma_uv_y = block.chroma_y / 2;
        let chroma_uv_w = block.chroma_width / 2;
        let chroma_uv_h = block.chroma_height / 2;
        if trace_point_in_region(
            "WAV2C_TRACE_UV_BLOCK",
            block_uv_x,
            block_uv_y,
            block_uv_w,
            block_uv_h,
        ) || (block.is_chroma_ref
            && trace_point_in_region(
                "WAV2C_TRACE_UV_BLOCK",
                chroma_uv_x,
                chroma_uv_y,
                chroma_uv_w,
                chroma_uv_h,
            ))
        {
            eprintln!(
                "[WAV2C_UV_BLOCK] idx={} tree={:?} is_chroma_ref={} luma=({}, {}) {}x{} block_uv=({}, {}) {}x{} chroma_uv=({}, {}) {}x{} uv_mode={} cfl_idx={} mh_dir={} u_tu={} v_tu={}",
                block_idx,
                block.tree_type,
                block.is_chroma_ref,
                bx,
                by,
                bw,
                bh,
                block_uv_x,
                block_uv_y,
                block_uv_w,
                block_uv_h,
                chroma_uv_x,
                chroma_uv_y,
                chroma_uv_w,
                chroma_uv_h,
                block.uv_mode,
                block.cfl_idx,
                block.mhccp_dir,
                block.u_tu.len(),
                block.v_tu.len()
            );
        }

        if block.tree_type != tile::TreeType::Chroma {
            let mi_x = bx / 4;
            let mi_y = by / 4;
            let mi_w = bw / 4;
            let mi_h = bh / 4;

            let joint = block.joint_y_mode;
            let (pred_mode, angle_delta) = predict::decode_prediction_mode(joint);

            for r in 0..mi_h {
                for c in 0..mi_w {
                    joint_grid[(mi_y + r) * mi_cols + mi_x + c] = joint;
                }
            }

            if block.is_intrabc {
                let src_x = bx as i32 + block.bv_col / 8;
                let src_y = by as i32 + block.bv_row / 8;
                let mut y_pred = vec![128u8; bw * bh];
                for r in 0..bh {
                    for c in 0..bw {
                        let sy = src_y + r as i32;
                        let sx = src_x + c as i32;
                        if sy >= 0 && sx >= 0 && sy < h as i32 && sx < w as i32 {
                            y_pred[r * bw + c] = y_plane[sy as usize * w + sx as usize];
                        }
                    }
                }
                if block.morph_pred {
                    apply_intrabc_morph_pred_luma(
                        &y_plane,
                        w,
                        w,
                        h,
                        bx,
                        by,
                        bw,
                        bh,
                        src_x,
                        src_y,
                        &mut y_pred,
                    );
                }
                if block.y_tu.is_empty() {
                    for r in 0..bh {
                        let dst = (by + r) * w + bx;
                        let src = r * bw;
                        y_plane[dst..dst + bw].copy_from_slice(&y_pred[src..src + bw]);
                    }
                    mark_region_ready(&mut y_ready, w, bx, by, bw, bh);
                } else {
                    for tu in block.y_tu.iter() {
                        let tu_w = tu.tx_size.width();
                        let tu_h = tu.tx_size.height();
                        let tx = bx + tu.col_offset;
                        let ty = by + tu.row_offset;
                        let residuals = if coded_lossless {
                            lossless_luma_residual(block, tu)
                        } else {
                            transform::dequantize_and_inverse_tcq_qm(
                                &tu.levels,
                                tu.eob,
                                dequant,
                                tu.tx_size,
                                tu.tx_type,
                                tcq_mode,
                                qm::iqmatrix(qm_levels[0], 0, tu.tx_size),
                            )
                        };
                        reconstruct_tu(
                            &mut y_plane,
                            w,
                            tx,
                            ty,
                            tu_w,
                            tu_h,
                            &y_pred,
                            bw,
                            tu.col_offset,
                            tu.row_offset,
                            &residuals,
                        );
                        mark_region_ready(&mut y_ready, w, tx, ty, tu_w, tu_h);
                    }
                }
            } else if !block.palette_y_map.is_empty() {
                let mut y_pred = vec![128u8; bw * bh];
                for r in 0..bh {
                    for c in 0..bw {
                        let color_idx = block.palette_y_map[r * bw + c] as usize;
                        y_pred[r * bw + c] = block.palette_y_colors[color_idx] as u8;
                    }
                }
                for tu in block.y_tu.iter() {
                    let tu_w = tu.tx_size.width();
                    let tu_h = tu.tx_size.height();
                    let tx = bx + tu.col_offset;
                    let ty = by + tu.row_offset;
                    let residuals = if coded_lossless {
                        lossless_luma_residual(block, tu)
                    } else {
                        transform::dequantize_and_inverse_tcq_qm(
                            &tu.levels,
                            tu.eob,
                            dequant,
                            tu.tx_size,
                            tu.tx_type,
                            tcq_mode,
                            qm::iqmatrix(qm_levels[0], 0, tu.tx_size),
                        )
                    };
                    reconstruct_tu(
                        &mut y_plane,
                        w,
                        tx,
                        ty,
                        tu_w,
                        tu_h,
                        &y_pred,
                        bw,
                        tu.col_offset,
                        tu.row_offset,
                        &residuals,
                    );
                    mark_region_ready(&mut y_ready, w, tx, ty, tu_w, tu_h);
                }
            } else {
                let sb_y = (by / sb_size) * sb_size;
                for tu in block.y_tu.iter() {
                    let tu_w = tu.tx_size.width();
                    let tu_h = tu.tx_size.height();
                    let tx = bx + tu.col_offset;
                    let ty = by + tu.row_offset;
                    let mut y_pred = predict::predict_intra_with_control_and_availability(
                        &y_plane,
                        w,
                        tx,
                        ty,
                        tu_w,
                        tu_h,
                        pred_mode,
                        angle_delta,
                        block.mrl_index,
                        sb_y,
                        predict::IntraPredictControl {
                            plane: 0,
                            enable_ibp,
                            is_cfl: false,
                            above_filter_type: 0,
                            left_filter_type: 0,
                            disable_edge_filter,
                        },
                        Some(&y_ready),
                    );
                    if block.multi_line_mrl != 0
                        && block.mrl_index != 0
                        && tu.tx_size != crate::block::TxSize::Tx4x4
                    {
                        let line0 = predict::predict_intra_with_control_and_availability(
                            &y_plane,
                            w,
                            tx,
                            ty,
                            tu_w,
                            tu_h,
                            pred_mode,
                            angle_delta,
                            0,
                            sb_y,
                            predict::IntraPredictControl {
                                plane: 0,
                                enable_ibp: false,
                                is_cfl: false,
                                above_filter_type: 0,
                                left_filter_type: 0,
                                disable_edge_filter: true,
                            },
                            Some(&y_ready),
                        );
                        for (dst, other) in y_pred.iter_mut().zip(line0.iter()) {
                            *dst = ((*dst as u16 + *other as u16 + 1) >> 1) as u8;
                        }
                    }
                    let residuals = if coded_lossless {
                        lossless_luma_residual(block, tu)
                    } else {
                        transform::dequantize_and_inverse_tcq_qm(
                            &tu.levels,
                            tu.eob,
                            dequant,
                            tu.tx_size,
                            tu.tx_type,
                            tcq_mode,
                            qm::iqmatrix(qm_levels[0], 0, tu.tx_size),
                        )
                    };
                    reconstruct_tu(
                        &mut y_plane,
                        w,
                        tx,
                        ty,
                        tu_w,
                        tu_h,
                        &y_pred,
                        tu_w,
                        0,
                        0,
                        &residuals,
                    );
                    mark_region_ready(&mut y_ready, w, tx, ty, tu_w, tu_h);
                }
            }
        }

        if block.tree_type == tile::TreeType::Luma || !block.is_chroma_ref {
            continue;
        }

        let chroma_x = block.chroma_x;
        let chroma_y = block.chroma_y;
        let chroma_width = block.chroma_width;
        let chroma_height = block.chroma_height;
        let uv_bx = chroma_x / 2;
        let uv_by = chroma_y / 2;
        let uv_bw = chroma_width / 2;
        let uv_bh = chroma_height / 2;
        let block_uv_filter_type = uv_mode_filter_type(block.uv_mode);

        if uv_bw >= 4 && uv_bh >= 4 {
            let (u_pred, v_pred) = if block.is_intrabc {
                let uv_src_x = uv_bx as i32 + block.bv_col / 16;
                let uv_src_y = uv_by as i32 + block.bv_row / 16;
                let mut u_pred = vec![128u8; uv_bw * uv_bh];
                let mut v_pred = vec![128u8; uv_bw * uv_bh];
                for r in 0..uv_bh {
                    for c in 0..uv_bw {
                        let sy = uv_src_y + r as i32;
                        let sx = uv_src_x + c as i32;
                        if sy >= 0 && sx >= 0 && sy < uv_h as i32 && sx < uv_w as i32 {
                            u_pred[r * uv_bw + c] = u_plane[sy as usize * uv_w + sx as usize];
                            v_pred[r * uv_bw + c] = v_plane[sy as usize * uv_w + sx as usize];
                        }
                    }
                }
                (u_pred, v_pred)
            } else if block.uv_mode == 13 {
                if block.cfl_idx == 2 {
                    let (uv_above_filter_type, uv_left_filter_type) =
                        neighbor_filter_types(&uv_filter, uv_w, uv_bx, uv_by);
                    let u_dc_pred = predict::predict_intra_with_control_and_availability(
                        &u_plane,
                        uv_w,
                        uv_bx,
                        uv_by,
                        uv_bw,
                        uv_bh,
                        predict::PredictionMode::DcPred,
                        0,
                        0,
                        0,
                        predict::IntraPredictControl {
                            plane: 1,
                            enable_ibp,
                            is_cfl: true,
                            above_filter_type: uv_above_filter_type,
                            left_filter_type: uv_left_filter_type,
                            disable_edge_filter,
                        },
                        Some(&u_ready),
                    );
                    let v_dc_pred = predict::predict_intra_with_control_and_availability(
                        &v_plane,
                        uv_w,
                        uv_bx,
                        uv_by,
                        uv_bw,
                        uv_bh,
                        predict::PredictionMode::DcPred,
                        0,
                        0,
                        0,
                        predict::IntraPredictControl {
                            plane: 2,
                            enable_ibp,
                            is_cfl: true,
                            above_filter_type: uv_above_filter_type,
                            left_filter_type: uv_left_filter_type,
                            disable_edge_filter,
                        },
                        Some(&v_ready),
                    );
                    let u_window = mhccp_build_ref_window(
                        &y_plane,
                        &y_ready,
                        w,
                        h,
                        &u_plane,
                        &u_ready,
                        uv_w,
                        uv_h,
                        uv_bx,
                        uv_by,
                        uv_bw,
                        uv_bh,
                        &u_dc_pred,
                        cfl_ds_filter_index,
                        sb_size,
                        'U',
                        &mut mhccp_luma_scratch,
                        &mut mhccp_u_scratch,
                    );
                    let u_params =
                        mhccp_derive_params(&u_window, block.mhccp_dir, 'U', uv_bx, uv_by);
                    let v_window = mhccp_build_ref_window(
                        &y_plane,
                        &y_ready,
                        w,
                        h,
                        &v_plane,
                        &v_ready,
                        uv_w,
                        uv_h,
                        uv_bx,
                        uv_by,
                        uv_bw,
                        uv_bh,
                        &v_dc_pred,
                        cfl_ds_filter_index,
                        sb_size,
                        'V',
                        &mut mhccp_luma_scratch,
                        &mut mhccp_v_scratch,
                    );
                    let v_params =
                        mhccp_derive_params(&v_window, block.mhccp_dir, 'V', uv_bx, uv_by);
                    let u_pred = mhccp_predict_plane(
                        &u_window,
                        uv_bw,
                        uv_bh,
                        block.mhccp_dir,
                        u_params,
                        'U',
                        uv_bx,
                        uv_by,
                    );
                    let v_pred = mhccp_predict_plane(
                        &v_window,
                        uv_bw,
                        uv_bh,
                        block.mhccp_dir,
                        v_params,
                        'V',
                        uv_bx,
                        uv_by,
                    );
                    (u_pred, v_pred)
                } else {
                    let (uv_above_filter_type, uv_left_filter_type) =
                        neighbor_filter_types(&uv_filter, uv_w, uv_bx, uv_by);
                    let u_dc_pred = predict::predict_intra_with_control_and_availability(
                        &u_plane,
                        uv_w,
                        uv_bx,
                        uv_by,
                        uv_bw,
                        uv_bh,
                        predict::PredictionMode::DcPred,
                        0,
                        0,
                        0,
                        predict::IntraPredictControl {
                            plane: 1,
                            enable_ibp,
                            is_cfl: true,
                            above_filter_type: uv_above_filter_type,
                            left_filter_type: uv_left_filter_type,
                            disable_edge_filter,
                        },
                        Some(&u_ready),
                    );
                    let v_dc_pred = predict::predict_intra_with_control_and_availability(
                        &v_plane,
                        uv_w,
                        uv_bx,
                        uv_by,
                        uv_bw,
                        uv_bh,
                        predict::PredictionMode::DcPred,
                        0,
                        0,
                        0,
                        predict::IntraPredictControl {
                            plane: 2,
                            enable_ibp,
                            is_cfl: true,
                            above_filter_type: uv_above_filter_type,
                            left_filter_type: uv_left_filter_type,
                            disable_edge_filter,
                        },
                        Some(&v_ready),
                    );

                    let recon_q3 = cfl_store_luma_q3_420(
                        &y_plane,
                        w,
                        h,
                        chroma_x,
                        chroma_y,
                        chroma_width,
                        chroma_height,
                        cfl_ds_filter_index,
                    );
                    let luma_refs = cfl_fetch_neighbor_luma_refs_420(
                        &y_plane,
                        w,
                        h,
                        chroma_x,
                        chroma_y,
                        chroma_width,
                        chroma_height,
                        sb_size,
                        cfl_ds_filter_index,
                    );
                    let avg_q3 = cfl_calc_luma_dc_q3(&luma_refs, uv_bw, uv_bh);
                    let trace_cfl = trace_block_match("WAV2C_TRACE_CFL", uv_bx, uv_by);
                    let (
                        u_top_refs,
                        u_left_refs,
                        u_alpha_info,
                        v_top_refs,
                        v_left_refs,
                        v_alpha_info,
                    ) = if block.cfl_idx == 0 {
                        (Vec::new(), Vec::new(), None, Vec::new(), Vec::new(), None)
                    } else {
                        let (u_top, u_left) = cfl_fetch_neighbor_chroma_refs(
                            &u_plane, uv_w, uv_h, uv_bx, uv_by, uv_bw, uv_bh,
                        );
                        let (v_top, v_left) = cfl_fetch_neighbor_chroma_refs(
                            &v_plane, uv_w, uv_h, uv_bx, uv_by, uv_bw, uv_bh,
                        );
                        let u_alpha =
                            cfl_derive_implicit_alpha_q3(&luma_refs, &u_top, &u_left, uv_bw, uv_bh);
                        let v_alpha =
                            cfl_derive_implicit_alpha_q3(&luma_refs, &v_top, &v_left, uv_bw, uv_bh);
                        (u_top, u_left, Some(u_alpha), v_top, v_left, Some(v_alpha))
                    };
                    let (alpha_u_q3, alpha_v_q3) = if block.cfl_idx == 0 {
                        (block.cfl_alpha_u as i32 * 32, block.cfl_alpha_v as i32 * 32)
                    } else {
                        (
                            u_alpha_info.as_ref().map(|info| info.alpha_q3).unwrap_or(0),
                            v_alpha_info.as_ref().map(|info| info.alpha_q3).unwrap_or(0),
                        )
                    };
                    if trace_cfl {
                        eprintln!(
                            "[WAV2C_CFL_BLOCK] uv=({}, {}) size={}x{} cfl_idx={} avg_q3={} alpha_u={} alpha_v={}",
                            uv_bx,
                            uv_by,
                            uv_bw,
                            uv_bh,
                            block.cfl_idx,
                            avg_q3,
                            alpha_u_q3,
                            alpha_v_q3
                        );
                        eprintln!(
                            "[WAV2C_CFL_REFS] have_top={} have_left={}",
                            luma_refs.have_top, luma_refs.have_left
                        );
                        eprintln!("[WAV2C_CFL_REFS] luma_top_q3={:?}", luma_refs.luma_top_q3);
                        eprintln!("[WAV2C_CFL_REFS] luma_left_q3={:?}", luma_refs.luma_left_q3);
                        if let Some(alpha) = &u_alpha_info {
                            eprintln!("[WAV2C_CFL_REFS] plane=U top={:?}", u_top_refs);
                            eprintln!("[WAV2C_CFL_REFS] plane=U left={:?}", u_left_refs);
                            eprintln!(
                                "[WAV2C_CFL_DERIVE] plane=U numb_up={} numb_left={} alpha={}",
                                alpha.numb_up, alpha.numb_left, alpha.alpha_q3
                            );
                            for sample in &alpha.samples {
                                eprintln!(
                                    "[WAV2C_CFL_SAMPLE] plane=U side={} idx={} x_q3={} y={}",
                                    sample.side, sample.idx, sample.x_q3, sample.y
                                );
                            }
                        }
                        if let Some(alpha) = &v_alpha_info {
                            eprintln!("[WAV2C_CFL_REFS] plane=V top={:?}", v_top_refs);
                            eprintln!("[WAV2C_CFL_REFS] plane=V left={:?}", v_left_refs);
                            eprintln!(
                                "[WAV2C_CFL_DERIVE] plane=V numb_up={} numb_left={} alpha={}",
                                alpha.numb_up, alpha.numb_left, alpha.alpha_q3
                            );
                            for sample in &alpha.samples {
                                eprintln!(
                                    "[WAV2C_CFL_SAMPLE] plane=V side={} idx={} x_q3={} y={}",
                                    sample.side, sample.idx, sample.x_q3, sample.y
                                );
                            }
                        }
                        eprintln!("[WAV2C_CFL_DC] u_dc={:?}", u_dc_pred);
                        eprintln!("[WAV2C_CFL_DC] v_dc={:?}", v_dc_pred);
                        eprintln!("[WAV2C_CFL_AC] recon_q3={:?}", recon_q3);
                    }

                    let mut u_cfl = vec![0u8; uv_bw * uv_bh];
                    let mut v_cfl = vec![0u8; uv_bw * uv_bh];
                    for r in 0..uv_bh {
                        for c in 0..uv_bw {
                            let ac = recon_q3[r * uv_bw + c] - avg_q3;
                            let u_dc = u_dc_pred[r * uv_bw + c] as i32;
                            let u_scaled = round_power_of_two_signed(alpha_u_q3 * ac, 11);
                            u_cfl[r * uv_bw + c] = (u_dc + u_scaled).clamp(0, 255) as u8;

                            let v_dc = v_dc_pred[r * uv_bw + c] as i32;
                            let v_scaled = round_power_of_two_signed(alpha_v_q3 * ac, 11);
                            v_cfl[r * uv_bw + c] = (v_dc + v_scaled).clamp(0, 255) as u8;
                        }
                    }
                    if trace_cfl {
                        eprintln!("[WAV2C_CFL_PRED] u_pred={:?}", u_cfl);
                        eprintln!("[WAV2C_CFL_PRED] v_pred={:?}", v_cfl);
                    }
                    (u_cfl, v_cfl)
                }
            } else {
                let uv_pred_mode = predict::PredictionMode::from_u8(block.uv_mode);
                let (_, y_angle_delta) = predict::decode_prediction_mode(block.joint_y_mode);
                let uv_angle_delta =
                    if block.uv_mode == block.y_mode && (1..=8).contains(&block.uv_mode) {
                        y_angle_delta
                    } else {
                        0
                    };
                let tu_count = block.u_tu.len().min(block.v_tu.len());
                if tu_count == 0 {
                    let (uv_above_filter_type, uv_left_filter_type) =
                        neighbor_filter_types(&uv_filter, uv_w, uv_bx, uv_by);
                    let u_p = predict::predict_intra_with_control_and_availability(
                        &u_plane,
                        uv_w,
                        uv_bx,
                        uv_by,
                        uv_bw,
                        uv_bh,
                        uv_pred_mode,
                        uv_angle_delta,
                        0,
                        0,
                        predict::IntraPredictControl {
                            plane: 1,
                            enable_ibp,
                            is_cfl: false,
                            above_filter_type: uv_above_filter_type,
                            left_filter_type: uv_left_filter_type,
                            disable_edge_filter,
                        },
                        Some(&u_ready),
                    );
                    let v_p = predict::predict_intra_with_control_and_availability(
                        &v_plane,
                        uv_w,
                        uv_bx,
                        uv_by,
                        uv_bw,
                        uv_bh,
                        uv_pred_mode,
                        uv_angle_delta,
                        0,
                        0,
                        predict::IntraPredictControl {
                            plane: 2,
                            enable_ibp,
                            is_cfl: false,
                            above_filter_type: uv_above_filter_type,
                            left_filter_type: uv_left_filter_type,
                            disable_edge_filter,
                        },
                        Some(&v_ready),
                    );
                    (u_p, v_p)
                } else {
                    let exact_uv_single_tu = tu_count == 1
                        && uv_bw == 16
                        && uv_bh == 16
                        && block.uv_mode <= 12
                        && !block.is_intrabc
                        && block.cfl_idx == 0
                        && block.mhccp_dir == 0
                        && block.u_tu[0].cctx_type == 0
                        && block.v_tu[0].cctx_type == 0
                        && block.u_tu[0].tx_type == 0
                        && block.v_tu[0].tx_type == 0
                        && block.u_tu[0].row_offset == 0
                        && block.u_tu[0].col_offset == 0
                        && block.v_tu[0].row_offset == 0
                        && block.v_tu[0].col_offset == 0
                        && block.u_tu[0].tx_size.width() == uv_bw
                        && block.u_tu[0].tx_size.height() == uv_bh
                        && block.v_tu[0].tx_size.width() == uv_bw
                        && block.v_tu[0].tx_size.height() == uv_bh;

                    if exact_uv_single_tu {
                        let uv_mode = crate::encode::IntraMode::from_av2(block.uv_mode);
                        let u_tu = &block.u_tu[0];
                        let v_tu = &block.v_tu[0];

                        let u_dq = transform::dequantize_only_qm(
                            &u_tu.levels,
                            u_tu.eob,
                            uv_dequant,
                            u_tu.tx_size,
                            u_tu.tx_type,
                            tcq_mode,
                            qm::iqmatrix(qm_levels[1], 1, u_tu.tx_size),
                        );
                        let v_dq = transform::dequantize_only_qm(
                            &v_tu.levels,
                            v_tu.eob,
                            uv_dequant,
                            v_tu.tx_size,
                            v_tu.tx_type,
                            tcq_mode,
                            qm::iqmatrix(qm_levels[2], 2, v_tu.tx_size),
                        );

                        let mut u_dq_arr = [0i32; 256];
                        u_dq_arr.copy_from_slice(&u_dq[..256]);
                        let mut v_dq_arr = [0i32; 256];
                        v_dq_arr.copy_from_slice(&v_dq[..256]);
                        let u_res = crate::dct::inverse_dct2_16x16(&u_dq_arr).to_vec();
                        let v_res = crate::dct::inverse_dct2_16x16(&v_dq_arr).to_vec();

                        let uv_pred_mode = predict::PredictionMode::from_u8(block.uv_mode);
                        let (_, y_angle_delta) =
                            predict::decode_prediction_mode(block.joint_y_mode);
                        let uv_angle_delta = if block.uv_mode == block.y_mode
                            && (1..=8).contains(&block.uv_mode)
                        {
                            y_angle_delta
                        } else {
                            0
                        };
                        let (u_above_filter_type, u_left_filter_type) =
                            neighbor_filter_types(&uv_filter, uv_w, uv_bx, uv_by);
                        let u_pred = predict::predict_intra_with_control_and_availability(
                            &u_plane,
                            uv_w,
                            uv_bx,
                            uv_by,
                            uv_bw,
                            uv_bh,
                            uv_pred_mode,
                            uv_angle_delta,
                            0,
                            0,
                            predict::IntraPredictControl {
                                plane: 1,
                                enable_ibp,
                                is_cfl: false,
                                above_filter_type: u_above_filter_type,
                                left_filter_type: u_left_filter_type,
                                disable_edge_filter,
                            },
                            Some(&u_ready),
                        );
                        let (v_above_filter_type, v_left_filter_type) =
                            neighbor_filter_types(&uv_filter, uv_w, uv_bx, uv_by);
                        let v_pred = predict::predict_intra_with_control_and_availability(
                            &v_plane,
                            uv_w,
                            uv_bx,
                            uv_by,
                            uv_bw,
                            uv_bh,
                            uv_pred_mode,
                            uv_angle_delta,
                            0,
                            0,
                            predict::IntraPredictControl {
                                plane: 2,
                                enable_ibp,
                                is_cfl: false,
                                above_filter_type: v_above_filter_type,
                                left_filter_type: v_left_filter_type,
                                disable_edge_filter,
                            },
                            Some(&v_ready),
                        );

                        reconstruct_tu(
                            &mut u_plane,
                            uv_w,
                            uv_bx,
                            uv_by,
                            uv_bw,
                            uv_bh,
                            &u_pred,
                            uv_bw,
                            0,
                            0,
                            &u_res,
                        );
                        reconstruct_tu(
                            &mut v_plane,
                            uv_w,
                            uv_bx,
                            uv_by,
                            uv_bw,
                            uv_bh,
                            &v_pred,
                            uv_bw,
                            0,
                            0,
                            &v_res,
                        );

                        trace_uv_recon_tu(
                            1,
                            uv_bx,
                            uv_by,
                            uv_bw,
                            uv_bh,
                            u_tu.tx_size,
                            u_tu.tx_type,
                            u_tu.eob,
                            &u_tu.levels,
                            &u_dq,
                            &u_res,
                            &u_pred,
                            uv_bw,
                            0,
                            0,
                            &u_plane,
                            uv_w,
                        );
                        trace_uv_recon_tu(
                            2,
                            uv_bx,
                            uv_by,
                            uv_bw,
                            uv_bh,
                            v_tu.tx_size,
                            v_tu.tx_type,
                            v_tu.eob,
                            &v_tu.levels,
                            &v_dq,
                            &v_res,
                            &v_pred,
                            uv_bw,
                            0,
                            0,
                            &v_plane,
                            uv_w,
                        );

                        mark_region_ready(&mut u_ready, uv_w, uv_bx, uv_by, uv_bw, uv_bh);
                        mark_region_ready(&mut v_ready, uv_w, uv_bx, uv_by, uv_bw, uv_bh);
                        mark_region_filter(
                            &mut uv_filter,
                            uv_w,
                            uv_bx,
                            uv_by,
                            uv_bw,
                            uv_bh,
                            block_uv_filter_type,
                        );
                        continue;
                    }

                    for i in 0..tu_count {
                        let u_tu = &block.u_tu[i];
                        let v_tu = &block.v_tu[i];

                        let mut u_dq = transform::dequantize_only_qm(
                            &u_tu.levels,
                            u_tu.eob,
                            uv_dequant,
                            u_tu.tx_size,
                            u_tu.tx_type,
                            tcq_mode,
                            qm::iqmatrix(qm_levels[1], 1, u_tu.tx_size),
                        );
                        let mut v_dq = transform::dequantize_only_qm(
                            &v_tu.levels,
                            v_tu.eob,
                            uv_dequant,
                            v_tu.tx_size,
                            v_tu.tx_type,
                            tcq_mode,
                            qm::iqmatrix(qm_levels[2], 2, v_tu.tx_size),
                        );

                        if u_tu.cctx_type != 0 {
                            transform::inverse_cctx(&mut u_dq, &mut v_dq, u_tu.cctx_type);
                        }

                        let u_res = if coded_lossless {
                            lossless_chroma_residual(block, u_tu)
                        } else {
                            transform::inverse_transform_only(&u_dq, u_tu.tx_size, u_tu.tx_type)
                        };
                        let v_res = if coded_lossless {
                            lossless_chroma_residual(block, v_tu)
                        } else {
                            transform::inverse_transform_only(&v_dq, v_tu.tx_size, v_tu.tx_type)
                        };

                        let u_tu_w = u_tu.tx_size.width();
                        let u_tu_h = u_tu.tx_size.height();
                        let u_tx = uv_bx + u_tu.col_offset;
                        let u_ty = uv_by + u_tu.row_offset;
                        let (uv_above_filter_type, uv_left_filter_type) =
                            neighbor_filter_types(&uv_filter, uv_w, u_tx, u_ty);
                        let u_pred = predict::predict_intra_with_control_and_availability(
                            &u_plane,
                            uv_w,
                            u_tx,
                            u_ty,
                            u_tu_w,
                            u_tu_h,
                            uv_pred_mode,
                            uv_angle_delta,
                            0,
                            0,
                            predict::IntraPredictControl {
                                plane: 1,
                                enable_ibp,
                                is_cfl: false,
                                above_filter_type: uv_above_filter_type,
                                left_filter_type: uv_left_filter_type,
                                disable_edge_filter,
                            },
                            Some(&u_ready),
                        );
                        reconstruct_tu(
                            &mut u_plane,
                            uv_w,
                            u_tx,
                            u_ty,
                            u_tu_w,
                            u_tu_h,
                            &u_pred,
                            u_tu_w,
                            0,
                            0,
                            &u_res,
                        );
                        trace_uv_recon_tu(
                            1,
                            u_tx,
                            u_ty,
                            u_tu_w,
                            u_tu_h,
                            u_tu.tx_size,
                            u_tu.tx_type,
                            u_tu.eob,
                            &u_tu.levels,
                            &u_dq,
                            &u_res,
                            &u_pred,
                            u_tu_w,
                            0,
                            0,
                            &u_plane,
                            uv_w,
                        );
                        if trace_point_in_region("WAV2C_TRACE_UV_WRITE", u_tx, u_ty, u_tu_w, u_tu_h)
                        {
                            if let Some((px, py)) = trace_point("WAV2C_TRACE_UV_WRITE") {
                                trace_uv_write(
                                    block_idx,
                                    1,
                                    "reconstruct_tu",
                                    px,
                                    py,
                                    u_plane[py * uv_w + px],
                                    block,
                                );
                            }
                        }
                        mark_region_ready(&mut u_ready, uv_w, u_tx, u_ty, u_tu_w, u_tu_h);
                        mark_region_filter(
                            &mut uv_filter,
                            uv_w,
                            u_tx,
                            u_ty,
                            u_tu_w,
                            u_tu_h,
                            block_uv_filter_type,
                        );

                        let v_tu_w = v_tu.tx_size.width();
                        let v_tu_h = v_tu.tx_size.height();
                        let v_tx = uv_bx + v_tu.col_offset;
                        let v_ty = uv_by + v_tu.row_offset;
                        let (uv_above_filter_type, uv_left_filter_type) =
                            neighbor_filter_types(&uv_filter, uv_w, v_tx, v_ty);
                        let v_pred = predict::predict_intra_with_control_and_availability(
                            &v_plane,
                            uv_w,
                            v_tx,
                            v_ty,
                            v_tu_w,
                            v_tu_h,
                            uv_pred_mode,
                            uv_angle_delta,
                            0,
                            0,
                            predict::IntraPredictControl {
                                plane: 2,
                                enable_ibp,
                                is_cfl: false,
                                above_filter_type: uv_above_filter_type,
                                left_filter_type: uv_left_filter_type,
                                disable_edge_filter,
                            },
                            Some(&v_ready),
                        );
                        reconstruct_tu(
                            &mut v_plane,
                            uv_w,
                            v_tx,
                            v_ty,
                            v_tu_w,
                            v_tu_h,
                            &v_pred,
                            v_tu_w,
                            0,
                            0,
                            &v_res,
                        );
                        trace_uv_recon_tu(
                            2,
                            v_tx,
                            v_ty,
                            v_tu_w,
                            v_tu_h,
                            v_tu.tx_size,
                            v_tu.tx_type,
                            v_tu.eob,
                            &v_tu.levels,
                            &v_dq,
                            &v_res,
                            &v_pred,
                            v_tu_w,
                            0,
                            0,
                            &v_plane,
                            uv_w,
                        );
                        if trace_point_in_region("WAV2C_TRACE_UV_WRITE", v_tx, v_ty, v_tu_w, v_tu_h)
                        {
                            if let Some((px, py)) = trace_point("WAV2C_TRACE_UV_WRITE") {
                                trace_uv_write(
                                    block_idx,
                                    2,
                                    "reconstruct_tu",
                                    px,
                                    py,
                                    v_plane[py * uv_w + px],
                                    block,
                                );
                            }
                        }
                        mark_region_ready(&mut v_ready, uv_w, v_tx, v_ty, v_tu_w, v_tu_h);
                        mark_region_filter(
                            &mut uv_filter,
                            uv_w,
                            v_tx,
                            v_ty,
                            v_tu_w,
                            v_tu_h,
                            block_uv_filter_type,
                        );
                    }
                    continue;
                }
            };

            let tu_count = block.u_tu.len().min(block.v_tu.len());
            if tu_count == 0 {
                for r in 0..uv_bh {
                    let dst = (uv_by + r) * uv_w + uv_bx;
                    let src = r * uv_bw;
                    u_plane[dst..dst + uv_bw].copy_from_slice(&u_pred[src..src + uv_bw]);
                    v_plane[dst..dst + uv_bw].copy_from_slice(&v_pred[src..src + uv_bw]);
                }
                if trace_point_in_region("WAV2C_TRACE_UV_WRITE", uv_bx, uv_by, uv_bw, uv_bh) {
                    if let Some((px, py)) = trace_point("WAV2C_TRACE_UV_WRITE") {
                        trace_uv_write(
                            block_idx,
                            1,
                            "copy_pred",
                            px,
                            py,
                            u_plane[py * uv_w + px],
                            block,
                        );
                        trace_uv_write(
                            block_idx,
                            2,
                            "copy_pred",
                            px,
                            py,
                            v_plane[py * uv_w + px],
                            block,
                        );
                    }
                }
                mark_region_ready(&mut u_ready, uv_w, uv_bx, uv_by, uv_bw, uv_bh);
                mark_region_ready(&mut v_ready, uv_w, uv_bx, uv_by, uv_bw, uv_bh);
                mark_region_filter(
                    &mut uv_filter,
                    uv_w,
                    uv_bx,
                    uv_by,
                    uv_bw,
                    uv_bh,
                    block_uv_filter_type,
                );
            } else {
                for i in 0..tu_count {
                    let u_tu = &block.u_tu[i];
                    let v_tu = &block.v_tu[i];

                    let mut u_dq = transform::dequantize_only_qm(
                        &u_tu.levels,
                        u_tu.eob,
                        uv_dequant,
                        u_tu.tx_size,
                        u_tu.tx_type,
                        tcq_mode,
                        qm::iqmatrix(qm_levels[1], 1, u_tu.tx_size),
                    );
                    let mut v_dq = transform::dequantize_only_qm(
                        &v_tu.levels,
                        v_tu.eob,
                        uv_dequant,
                        v_tu.tx_size,
                        v_tu.tx_type,
                        tcq_mode,
                        qm::iqmatrix(qm_levels[2], 2, v_tu.tx_size),
                    );

                    if u_tu.cctx_type != 0 {
                        transform::inverse_cctx(&mut u_dq, &mut v_dq, u_tu.cctx_type);
                    }

                    let u_res = if coded_lossless {
                        lossless_chroma_residual(block, u_tu)
                    } else {
                        transform::inverse_transform_only(&u_dq, u_tu.tx_size, u_tu.tx_type)
                    };
                    let v_res = if coded_lossless {
                        lossless_chroma_residual(block, v_tu)
                    } else {
                        transform::inverse_transform_only(&v_dq, v_tu.tx_size, v_tu.tx_type)
                    };

                    let u_tu_w = u_tu.tx_size.width();
                    let u_tu_h = u_tu.tx_size.height();
                    reconstruct_tu(
                        &mut u_plane,
                        uv_w,
                        uv_bx + u_tu.col_offset,
                        uv_by + u_tu.row_offset,
                        u_tu_w,
                        u_tu_h,
                        &u_pred,
                        uv_bw,
                        u_tu.col_offset,
                        u_tu.row_offset,
                        &u_res,
                    );
                    trace_uv_recon_tu(
                        1,
                        uv_bx + u_tu.col_offset,
                        uv_by + u_tu.row_offset,
                        u_tu_w,
                        u_tu_h,
                        u_tu.tx_size,
                        u_tu.tx_type,
                        u_tu.eob,
                        &u_tu.levels,
                        &u_dq,
                        &u_res,
                        &u_pred,
                        uv_bw,
                        u_tu.col_offset,
                        u_tu.row_offset,
                        &u_plane,
                        uv_w,
                    );
                    if trace_point_in_region(
                        "WAV2C_TRACE_UV_WRITE",
                        uv_bx + u_tu.col_offset,
                        uv_by + u_tu.row_offset,
                        u_tu_w,
                        u_tu_h,
                    ) {
                        if let Some((px, py)) = trace_point("WAV2C_TRACE_UV_WRITE") {
                            trace_uv_write(
                                block_idx,
                                1,
                                "reconstruct_tu",
                                px,
                                py,
                                u_plane[py * uv_w + px],
                                block,
                            );
                        }
                    }
                    mark_region_ready(
                        &mut u_ready,
                        uv_w,
                        uv_bx + u_tu.col_offset,
                        uv_by + u_tu.row_offset,
                        u_tu_w,
                        u_tu_h,
                    );
                    mark_region_filter(
                        &mut uv_filter,
                        uv_w,
                        uv_bx + u_tu.col_offset,
                        uv_by + u_tu.row_offset,
                        u_tu_w,
                        u_tu_h,
                        block_uv_filter_type,
                    );

                    let v_tu_w = v_tu.tx_size.width();
                    let v_tu_h = v_tu.tx_size.height();
                    reconstruct_tu(
                        &mut v_plane,
                        uv_w,
                        uv_bx + v_tu.col_offset,
                        uv_by + v_tu.row_offset,
                        v_tu_w,
                        v_tu_h,
                        &v_pred,
                        uv_bw,
                        v_tu.col_offset,
                        v_tu.row_offset,
                        &v_res,
                    );
                    trace_uv_recon_tu(
                        2,
                        uv_bx + v_tu.col_offset,
                        uv_by + v_tu.row_offset,
                        v_tu_w,
                        v_tu_h,
                        v_tu.tx_size,
                        v_tu.tx_type,
                        v_tu.eob,
                        &v_tu.levels,
                        &v_dq,
                        &v_res,
                        &v_pred,
                        uv_bw,
                        v_tu.col_offset,
                        v_tu.row_offset,
                        &v_plane,
                        uv_w,
                    );
                    if trace_point_in_region(
                        "WAV2C_TRACE_UV_WRITE",
                        uv_bx + v_tu.col_offset,
                        uv_by + v_tu.row_offset,
                        v_tu_w,
                        v_tu_h,
                    ) {
                        if let Some((px, py)) = trace_point("WAV2C_TRACE_UV_WRITE") {
                            trace_uv_write(
                                block_idx,
                                2,
                                "reconstruct_tu",
                                px,
                                py,
                                v_plane[py * uv_w + px],
                                block,
                            );
                        }
                    }
                    mark_region_ready(
                        &mut v_ready,
                        uv_w,
                        uv_bx + v_tu.col_offset,
                        uv_by + v_tu.row_offset,
                        v_tu_w,
                        v_tu_h,
                    );
                    mark_region_filter(
                        &mut uv_filter,
                        uv_w,
                        uv_bx + v_tu.col_offset,
                        uv_by + v_tu.row_offset,
                        v_tu_w,
                        v_tu_h,
                        block_uv_filter_type,
                    );
                }
            }
        }
    }

    apply_ccso_frame(&y_plane, &mut u_plane, &mut v_plane, w, h, ccso_params);

    (y_plane, u_plane, v_plane)
}

const CCSO_PADDING_SIZE: usize = 5;
const CCSO_QUANT_SZ_LOCAL: [[i32; 4]; 4] = [
    [16, 8, 32, 0],
    [56, 40, 64, 128],
    [48, 24, 96, 192],
    [80, 112, 160, 256],
];

fn build_ccso_extended_luma(y_plane: &[u8], width: usize, height: usize) -> (Vec<u16>, usize) {
    let stride = width + (CCSO_PADDING_SIZE << 1);
    let mut ext = vec![0u16; (height + (CCSO_PADDING_SIZE << 1)) * stride];
    for y in 0..height {
        let dst = (y + CCSO_PADDING_SIZE) * stride + CCSO_PADDING_SIZE;
        for x in 0..width {
            ext[dst + x] = y_plane[y * width + x] as u16;
        }
    }

    for y in 0..height {
        let row = (y + CCSO_PADDING_SIZE) * stride + CCSO_PADDING_SIZE;
        let left = ext[row];
        let right = ext[row + width - 1];
        for x in 0..CCSO_PADDING_SIZE {
            ext[row - CCSO_PADDING_SIZE + x] = left;
            ext[row + width + x] = right;
        }
    }

    let full_row = width + (CCSO_PADDING_SIZE << 1);
    let top = CCSO_PADDING_SIZE * stride;
    let bottom = (CCSO_PADDING_SIZE + height - 1) * stride;
    for y in 0..CCSO_PADDING_SIZE {
        let dst_top = y * stride;
        let dst_bottom = (CCSO_PADDING_SIZE + height + y) * stride;
        ext.copy_within(top..top + full_row, dst_top);
        ext.copy_within(bottom..bottom + full_row, dst_bottom);
    }

    (ext, stride)
}

fn derive_ccso_sample_offsets(stride: isize, ext_filter_support: u8) -> [isize; 2] {
    match ext_filter_support {
        0 => [-stride, stride],
        1 => [-1, 1],
        2 => [-stride - 1, stride + 1],
        3 => [-stride + 1, stride - 1],
        4 => [-stride - 2, stride + 2],
        5 => [stride - 2, -stride + 2],
        6 => [2, -2],
        _ => [0, 0],
    }
}

fn ccso_filter_class(
    ext_y: &[u16],
    center_idx: usize,
    offsets: [isize; 2],
    thr: i32,
    edge_clf: u8,
) -> [usize; 2] {
    let center = ext_y[center_idx] as i32;
    let neg_thr = -thr;
    let mut out = [0usize; 2];
    for (i, offset) in offsets.into_iter().enumerate() {
        let sample = ext_y[(center_idx as isize + offset) as usize] as i32;
        let diff = sample - center;
        out[i] = if edge_clf == 0 {
            if diff > thr {
                2
            } else if diff < neg_thr {
                0
            } else {
                1
            }
        } else if diff < neg_thr {
            0
        } else {
            1
        };
    }
    out
}

fn apply_ccso_plane(
    ext_y: &[u16],
    ext_stride: usize,
    dst: &mut [u8],
    dst_width: usize,
    dst_height: usize,
    plane: usize,
    params: &frame::CcsoPlaneParams,
) {
    if !params.enabled {
        return;
    }

    let hscale = usize::from(plane != 0);
    let vscale = usize::from(plane != 0);
    let thr = CCSO_QUANT_SZ_LOCAL[params.scale_idx as usize][params.quant_idx as usize];
    let shift_bits = 8usize.saturating_sub(params.max_band_log2 as usize);
    let is_single_band = params.max_band_log2 == 0;
    let offsets = derive_ccso_sample_offsets(ext_stride as isize, params.ext_filter_support);

    for y in 0..dst_height {
        for x in 0..dst_width {
            let center_y = CCSO_PADDING_SIZE + (y << vscale);
            let center_x = CCSO_PADDING_SIZE + (x << hscale);
            let center_idx = center_y * ext_stride + center_x;
            let [cls0, cls1] = if params.bo_only {
                [0, 0]
            } else {
                ccso_filter_class(ext_y, center_idx, offsets, thr, params.edge_clf)
            };
            let band = if is_single_band {
                0usize
            } else {
                (ext_y[center_idx] as usize) >> shift_bits
            };
            let lut_idx = (band << 4) + (cls0 << 2) + cls1;
            let filtered = dst[y * dst_width + x] as i32 + params.filter_offset[lut_idx] as i32;
            dst[y * dst_width + x] = filtered.clamp(0, 255) as u8;
        }
    }
}

fn apply_ccso_frame(
    y_plane: &[u8],
    u_plane: &mut [u8],
    v_plane: &mut [u8],
    width: usize,
    height: usize,
    ccso_params: &frame::CcsoParams,
) {
    if !ccso_params.frame_enabled {
        return;
    }

    let (ext_y, ext_stride) = build_ccso_extended_luma(y_plane, width, height);
    apply_ccso_plane(
        &ext_y,
        ext_stride,
        u_plane,
        width / 2,
        height / 2,
        1,
        &ccso_params.planes[1],
    );
    apply_ccso_plane(
        &ext_y,
        ext_stride,
        v_plane,
        width / 2,
        height / 2,
        2,
        &ccso_params.planes[2],
    );
}

fn gray_frame(width: u32, height: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let w = width as usize;
    let h = height as usize;
    (
        vec![128u8; w * h],
        vec![128u8; (w / 2) * (h / 2)],
        vec![128u8; (w / 2) * (h / 2)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{encode_video_yuv, encode_video_yuv_with_cdf_update};

    fn avmdec_executable() -> Option<std::path::PathBuf> {
        if let Some(value) = std::env::var_os("AVMDEC") {
            let path = std::path::PathBuf::from(value);
            if path.is_file() {
                return Some(path);
            }
        }
        let local = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../avm_build/avmdec");
        if local.is_file() {
            return Some(local);
        }
        let temp_build = std::path::PathBuf::from("/tmp/avm/build/avmdec");
        if temp_build.is_file() {
            return Some(temp_build);
        }
        if std::process::Command::new("avmdec")
            .arg("--help")
            .output()
            .is_ok()
        {
            return Some(std::path::PathBuf::from("avmdec"));
        }
        None
    }

    fn fnv1a_hash(bytes: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for &byte in bytes {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    fn fixture_decode_hash(name: &str) -> u64 {
        let path = format!("tests/fixtures/{}.obu", name);
        let data = std::fs::read(&path).unwrap();
        let frames = Decoder::decode_all(&data).unwrap();
        let mut planes = Vec::new();
        for frame in &frames {
            planes.extend_from_slice(&frame.y);
            planes.extend_from_slice(&frame.u);
            planes.extend_from_slice(&frame.v);
        }
        fnv1a_hash(&planes)
    }

    fn sequence_obu<'a, 'b>(obus: &'a [obu::RawObu<'b>]) -> &'a obu::RawObu<'b> {
        obus.iter()
            .find(|o| o.obu_type == 1)
            .expect("sequence header OBU")
    }

    fn frame_obu<'a, 'b>(obus: &'a [obu::RawObu<'b>]) -> &'a obu::RawObu<'b> {
        obus.iter()
            .find(|o| o.obu_type == 4)
            .expect("CLK OBU")
    }

    #[test]
    fn decode_solid_gray() {
        let y = vec![128u8; 64 * 64];
        let u = vec![128u8; 32 * 32];
        let v = vec![128u8; 32 * 32];
        let bs = encode_video_yuv(64, 64, 50, &[(y, u, v)]);
        let frames = Decoder::decode_all(&bs).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].width, 64);
        assert_eq!(frames[0].height, 64);
        assert!(frames[0].y.iter().all(|&p| p == 128));
    }

    #[test]
    fn decode_solid_green() {
        let y = vec![149u8; 64 * 64];
        let u = vec![43u8; 32 * 32];
        let v = vec![21u8; 32 * 32];
        let bs = encode_video_yuv(64, 64, 50, &[(y, u, v)]);
        let frames = Decoder::decode_all(&bs).unwrap();
        assert_eq!(frames.len(), 1);
        for (i, &p) in frames[0].y.iter().enumerate() {
            assert!(
                (p as i32 - 149).abs() <= 1,
                "Y pixel {} = {}, expected ~149",
                i,
                p
            );
        }
    }

    #[test]
    fn decode_multi_frame() {
        let frames_in: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> = (0..3)
            .map(|i| {
                let val = 100 + i * 20;
                (
                    vec![val; 64 * 64],
                    vec![128u8; 32 * 32],
                    vec![128u8; 32 * 32],
                )
            })
            .collect();
        let bs = encode_video_yuv(64, 64, 50, &frames_in);
        let frames = Decoder::decode_all(&bs).unwrap();
        assert_eq!(frames.len(), 3);
    }

    #[test]
    fn decode_128x128() {
        let y = vec![200u8; 128 * 128];
        let u = vec![100u8; 64 * 64];
        let v = vec![150u8; 64 * 64];
        let bs = encode_video_yuv(128, 128, 50, &[(y, u, v)]);
        let frames = Decoder::decode_all(&bs).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].width, 128);
        assert_eq!(frames[0].height, 128);
    }

    #[test]
    fn decode_gradient_y() {
        let mut y = vec![0u8; 128 * 128];
        for row in 0..128 {
            let val = (row * 4).min(255) as u8;
            for col in 0..128 {
                y[row * 128 + col] = val;
            }
        }
        let u = vec![128u8; 64 * 64];
        let v = vec![128u8; 64 * 64];
        let bs = encode_video_yuv(128, 128, 50, &[(y.clone(), u, v)]);
        let frames = Decoder::decode_all(&bs).unwrap();
        assert_eq!(frames.len(), 1);
        for row in 0..128 {
            let expected = (row * 4).min(255) as i32;
            for col in 0..128 {
                let actual = frames[0].y[row * 128 + col] as i32;
                assert!(
                    (actual - expected).abs() <= 6,
                    "Y pixel ({},{}) = {}, expected ~{}",
                    row,
                    col,
                    actual,
                    expected
                );
            }
        }
    }

    #[test]
    fn decode_256x192() {
        let y = vec![180u8; 256 * 192];
        let u = vec![80u8; 128 * 96];
        let v = vec![200u8; 128 * 96];
        let bs = encode_video_yuv(256, 192, 50, &[(y, u, v)]);
        let frames = Decoder::decode_all(&bs).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].width, 256);
        assert_eq!(frames[0].height, 192);
        for (i, &p) in frames[0].y.iter().enumerate() {
            assert!(
                (p as i32 - 180).abs() <= 1,
                "Y pixel {} = {}, expected ~180",
                i,
                p
            );
        }
    }

    #[test]
    fn decode_all_planes_exact() {
        let y = vec![128u8; 64 * 64];
        let u = vec![128u8; 32 * 32];
        let v = vec![128u8; 32 * 32];
        let bs = encode_video_yuv(64, 64, 50, &[(y, u, v)]);
        let frames = Decoder::decode_all(&bs).unwrap();
        assert_eq!(frames.len(), 1);
        assert!(frames[0].y.iter().all(|&p| p == 128));
        assert!(frames[0].u.iter().all(|&p| p == 128));
        assert!(frames[0].v.iter().all(|&p| p == 128));
    }

    #[test]
    fn decode_multi_frame_values() {
        let y_values = [80u8, 128u8, 200u8];
        let frames_in: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> = y_values
            .iter()
            .map(|&val| {
                (
                    vec![val; 64 * 64],
                    vec![128u8; 32 * 32],
                    vec![128u8; 32 * 32],
                )
            })
            .collect();
        let bs = encode_video_yuv(64, 64, 50, &frames_in);
        let frames = Decoder::decode_all(&bs).unwrap();
        assert_eq!(frames.len(), 3);
        for (idx, &expected_y) in y_values.iter().enumerate() {
            let avg: f64 =
                frames[idx].y.iter().map(|&p| p as f64).sum::<f64>() / frames[idx].y.len() as f64;
            assert!(
                (avg - expected_y as f64).abs() <= 2.0,
                "Frame {} Y average = {:.1}, expected ~{}",
                idx,
                avg,
                expected_y
            );
        }
    }

    #[test]
    fn decode_low_qindex() {
        let y = vec![100u8; 64 * 64];
        let u = vec![60u8; 32 * 32];
        let v = vec![220u8; 32 * 32];
        let bs = encode_video_yuv(64, 64, 25, &[(y, u, v)]);
        let frames = Decoder::decode_all(&bs).unwrap();
        assert_eq!(frames.len(), 1);
        for (i, &p) in frames[0].y.iter().enumerate() {
            assert!(
                (p as i32 - 100).abs() <= 1,
                "Y pixel {} = {}, expected ~100",
                i,
                p
            );
        }
    }

    #[test]
    fn roundtrip_cdf_adaptation_enabled() {
        let y = vec![128u8; 64 * 64];
        let u = vec![128u8; 32 * 32];
        let v = vec![128u8; 32 * 32];
        let bs = encode_video_yuv_with_cdf_update(64, 64, 50, &[(y.clone(), u.clone(), v.clone())]);
        let frames = Decoder::decode_all(&bs).unwrap();
        assert_eq!(frames.len(), 1);
        assert!(frames[0].y.iter().all(|&p| p == 128));
        assert!(frames[0].u.iter().all(|&p| p == 128));
        assert!(frames[0].v.iter().all(|&p| p == 128));
    }

    #[test]
    fn roundtrip_cdf_adaptation_with_coeffs() {
        let y = vec![200u8; 64 * 64];
        let u = vec![100u8; 32 * 32];
        let v = vec![150u8; 32 * 32];
        let bs = encode_video_yuv_with_cdf_update(64, 64, 50, &[(y, u, v)]);
        let frames = Decoder::decode_all(&bs).unwrap();
        assert_eq!(frames.len(), 1);
        for &p in &frames[0].y {
            assert!(
                (p as i32 - 200).abs() <= 1,
                "Y pixel = {}, expected ~200",
                p
            );
        }
    }

    #[test]
    #[ignore = "CDF adaptation internal roundtrip needs flat-SB decoder update for 13-mode support"]
    fn roundtrip_cdf_adaptation_128x128() {
        let y = vec![160u8; 128 * 128];
        let u = vec![128u8; 64 * 64];
        let v = vec![128u8; 64 * 64];
        let bs = encode_video_yuv_with_cdf_update(128, 128, 50, &[(y, u, v)]);
        let frames = Decoder::decode_all(&bs).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].width, 128);
        assert_eq!(frames[0].height, 128);
        for &p in &frames[0].y {
            assert!(
                (p as i32 - 160).abs() <= 2,
                "Y pixel = {}, expected ~160",
                p
            );
        }
    }

    #[test]
    fn parse_avmenc_minimal_obus() {
        let data = std::fs::read("tests/fixtures/avmenc_minimal.obu").unwrap();
        let obus = obu::parse_obus(&data);
        assert_eq!(obus.first().map(|o| o.obu_type), Some(2));
        assert_eq!(sequence_obu(&obus).obu_type, 1);
        assert_eq!(frame_obu(&obus).obu_type, 4);
    }

    #[test]
    fn parse_avmenc_minimal_seq_header() {
        let data = std::fs::read("tests/fixtures/avmenc_minimal.obu").unwrap();
        let obus = obu::parse_obus(&data);
        let seq = sequence::parse_sequence_header(sequence_obu(&obus).payload).unwrap();
        assert_eq!(seq.width, 256);
        assert_eq!(seq.height, 128);
        assert!(seq.single_picture_header_flag);
        eprintln!("avmenc_minimal sb_size={}", seq.sb_size);
    }

    #[test]
    fn parse_avmenc_minimal_frame_header() {
        let data = std::fs::read("tests/fixtures/avmenc_minimal.obu").unwrap();
        let obus = obu::parse_obus(&data);
        let seq = sequence::parse_sequence_header(sequence_obu(&obus).payload).unwrap();
        let fp = frame::parse_frame_header(frame_obu(&obus).payload, &seq).unwrap();
        assert_eq!(fp.base_qindex, 100);
        assert!(!fp.disable_cdf_update);
        assert!(!fp.tile_data.is_empty());
    }

    #[test]
    fn decode_avmenc_minimal_tile_data() {
        let data = std::fs::read("tests/fixtures/avmenc_minimal.obu").unwrap();
        let obus = obu::parse_obus(&data);
        let seq = sequence::parse_sequence_header(sequence_obu(&obus).payload).unwrap();
        let fp = frame::parse_frame_header(frame_obu(&obus).payload, &seq).unwrap();

        let tile_cfg = tile::TileDecodeConfig {
            enable_cfl_intra: seq.enable_cfl_intra,
            enable_fsc: seq.enable_fsc,
            enable_idtx_intra: seq.enable_idtx_intra,
            enable_mrls: seq.enable_mrls,
            enable_ist: seq.enable_ist,
            enable_cctx: seq.enable_cctx,
            tx_mode_select: fp.tx_mode_select,
            coded_lossless: fp.base_qindex == 0,
            reduced_tx_set_used: fp.reduced_tx_set_used,
            reduced_tx_part_set: seq.reduced_tx_part_set,
            allow_intrabc: fp.allow_intrabc,
            allow_screen_content_tools: fp.allow_screen_content_tools,
            enable_sdp: seq.enable_sdp,
            cdef_frame_enable: fp.cdef_frame_enable,
            nb_cdef_strengths: fp.nb_cdef_strengths,
            ccso_frame_enable: fp.ccso_frame_enable,
            ccso_enable: fp.ccso_enable,
            sb_size: seq.sb_size as usize,
            tcq_mode: fp.tcq_mode,
            allow_parity_hiding: fp.allow_parity_hiding,
            enable_ext_partitions: seq.enable_ext_partitions,
            enable_uneven_4way_partitions: seq.enable_uneven_4way_partitions,
            enable_mhccp: seq.enable_mhccp,
            enable_refmvbank: seq.enable_refmvbank,
            max_bvp_drl_bits: fp.max_bvp_drl_bits,
            cur_frame_force_integer_mv: fp.cur_frame_force_integer_mv,
            allow_intrabc_morph_pred: seq.enable_intra_bawp && fp.allow_screen_content_tools,
        };

        let blocks = tile::decode_tile_data_partitioned_cfg(
            fp.tile_data,
            seq.width as usize,
            seq.height as usize,
            seq.sb_size as usize,
            fp.base_qindex,
            &tile_cfg,
        )
        .unwrap();

        for (i, block) in blocks.iter().enumerate() {
            eprintln!(
                "Block {}: ({},{}) {}x{} y_mode={} uv_mode={}",
                i, block.x, block.y, block.width, block.height, block.y_mode, block.uv_mode
            );
        }

        assert!(!blocks.is_empty(), "Should decode at least one block");
    }

    #[test]
    fn reconstruct_avmenc_minimal_frame() {
        let data = std::fs::read("tests/fixtures/avmenc_minimal.obu").unwrap();
        let obus = obu::parse_obus(&data);
        let seq = sequence::parse_sequence_header(sequence_obu(&obus).payload).unwrap();
        let fp = frame::parse_frame_header(frame_obu(&obus).payload, &seq).unwrap();
        let dequant = crate::encode::ac_qlookup(fp.base_qindex);

        let tile_cfg = tile::TileDecodeConfig {
            enable_cfl_intra: seq.enable_cfl_intra,
            enable_fsc: seq.enable_fsc,
            enable_idtx_intra: seq.enable_idtx_intra,
            enable_mrls: seq.enable_mrls,
            enable_ist: seq.enable_ist,
            enable_cctx: seq.enable_cctx,
            tx_mode_select: fp.tx_mode_select,
            coded_lossless: fp.base_qindex == 0,
            reduced_tx_set_used: fp.reduced_tx_set_used,
            reduced_tx_part_set: seq.reduced_tx_part_set,
            allow_intrabc: fp.allow_intrabc,
            allow_screen_content_tools: fp.allow_screen_content_tools,
            enable_sdp: seq.enable_sdp,
            cdef_frame_enable: fp.cdef_frame_enable,
            nb_cdef_strengths: fp.nb_cdef_strengths,
            ccso_frame_enable: fp.ccso_frame_enable,
            ccso_enable: fp.ccso_enable,
            sb_size: seq.sb_size as usize,
            tcq_mode: fp.tcq_mode,
            allow_parity_hiding: fp.allow_parity_hiding,
            enable_ext_partitions: seq.enable_ext_partitions,
            enable_uneven_4way_partitions: seq.enable_uneven_4way_partitions,
            enable_mhccp: seq.enable_mhccp,
            enable_refmvbank: seq.enable_refmvbank,
            max_bvp_drl_bits: fp.max_bvp_drl_bits,
            cur_frame_force_integer_mv: fp.cur_frame_force_integer_mv,
            allow_intrabc_morph_pred: seq.enable_intra_bawp && fp.allow_screen_content_tools,
        };

        let blocks = tile::decode_tile_data_partitioned_cfg(
            fp.tile_data,
            seq.width as usize,
            seq.height as usize,
            seq.sb_size as usize,
            fp.base_qindex,
            &tile_cfg,
        )
        .unwrap();
        let uv_dequant = crate::encode::ac_qlookup(
            (fp.base_qindex as i32 + fp.u_ac_delta_q).clamp(0, 255) as u8,
        );
        let (y_plane, u_plane, v_plane) = reconstruct_partitioned_frame(
            &blocks,
            seq.width,
            seq.height,
            dequant,
            uv_dequant,
            fp.tcq_mode,
            fp.base_qindex == 0,
            seq.cfl_ds_filter_index,
            seq.sb_size as usize,
            seq.enable_ibp,
            !seq.enable_intra_edge_filter,
            [qm::NUM_QM_LEVELS as u8 - 1; 3],
            &fp.ccso_params,
        );

        let w = seq.width as usize;
        let h = seq.height as usize;
        assert_eq!(y_plane.len(), w * h);
        assert_eq!(u_plane.len(), w / 2 * h / 2);
        assert_eq!(v_plane.len(), w / 2 * h / 2);

        let Some(avmdec) = avmdec_executable() else {
            eprintln!("SKIP: avmdec not found");
            return;
        };
        let ref_path = std::env::temp_dir().join(format!(
            "av2codec_minimal_ref_{}.yuv",
            std::process::id()
        ));
        let output = std::process::Command::new(avmdec)
            .args([
                "--codec=av2",
                "--i420",
                "-o",
                ref_path.to_str().unwrap(),
                "tests/fixtures/avmenc_minimal.obu",
            ])
            .output()
            .expect("failed to run avmdec");
        assert!(
            output.status.success(),
            "avmdec failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let ref_y4m = std::fs::read(&ref_path).unwrap();
        let ref_y = &ref_y4m[..w * h];
        let ref_u = &ref_y4m[w * h..w * h + w / 2 * h / 2];

        let y_mse: f64 = y_plane
            .iter()
            .zip(ref_y.iter())
            .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
            .sum::<f64>()
            / (w * h) as f64;
        let y_psnr = if y_mse > 0.0 {
            10.0 * (255.0f64 * 255.0 / y_mse).log10()
        } else {
            100.0
        };

        let u_mse: f64 = u_plane
            .iter()
            .zip(ref_u.iter())
            .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
            .sum::<f64>()
            / (w / 2 * h / 2) as f64;
        let u_psnr = if u_mse > 0.0 {
            10.0 * (255.0f64 * 255.0 / u_mse).log10()
        } else {
            100.0
        };

        let ref_v = &ref_y4m[w * h + w / 2 * h / 2..w * h + 2 * (w / 2 * h / 2)];
        let v_mse: f64 = v_plane
            .iter()
            .zip(ref_v.iter())
            .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
            .sum::<f64>()
            / (w / 2 * h / 2) as f64;
        let v_psnr = if v_mse > 0.0 {
            10.0 * (255.0f64 * 255.0 / v_mse).log10()
        } else {
            100.0
        };

        eprintln!(
            "PSNR: Y={:.2} dB, U={:.2} dB, V={:.2} dB",
            y_psnr, u_psnr, v_psnr
        );
        assert!(
            y_psnr >= 40.0,
            "Y PSNR={:.2} dB, expected >= 40 dB (entropy sync verified, reconstruction pending)",
            y_psnr
        );
        assert!(u_psnr >= 40.0, "U PSNR={:.2} dB, expected >= 40 dB", u_psnr);
        assert!(v_psnr >= 40.0, "V PSNR={:.2} dB, expected >= 40 dB", v_psnr);
    }

    #[test]
    fn lossless_reference_fixture_is_bit_exact() {
        assert_eq!(
            fixture_decode_hash("avmenc_lossless_128x128"),
            0x4a0a360399574988
        );
    }

    #[test]
    fn parity_hiding_reference_fixture_is_bit_exact() {
        assert_eq!(
            fixture_decode_hash("avmenc_parity_128x128"),
            0x47fb8b4a05dbfe2b
        );
    }

    #[test]
    fn quant_matrix_reference_fixture_is_bit_exact() {
        assert_eq!(
            fixture_decode_hash("avmenc_qm_128x128"),
            0xaf8ff1013db75d62
        );
    }

    #[test]
    fn mrls_reference_fixture_is_bit_exact() {
        assert_eq!(
            fixture_decode_hash("avmenc_mrls_128x128"),
            0xc329e88c199cd4f2
        );
    }

    #[test]
    fn decode_avmenc_128x128_q30() {
        let data = std::fs::read("tests/fixtures/avmenc_128x128_q30.obu").unwrap();
        let obus = obu::parse_obus(&data);
        let seq = sequence::parse_sequence_header(sequence_obu(&obus).payload).unwrap();
        assert_eq!(seq.width, 128);
        assert_eq!(seq.height, 128);

        eprintln!(
            "SEQ: sph={} sb_size={} enable_sdp={} enable_cfl={} oh_bits={} lt_id_bits={} force_sct={} force_imv={} bvp_drl={} tcq={} ph={} sep_uv={} eq_dc={} y_dc={} uv_dc={} uv_ac={} cdef={} gdf={} rest={} ccso={} df_par={} fg={}",
            seq.single_picture_header_flag,
            seq.sb_size,
            seq.enable_sdp,
            seq.enable_cfl_intra,
            seq.order_hint_bits,
            seq.number_of_bits_for_lt_frame_id,
            seq.force_screen_content_tools,
            seq.force_integer_mv,
            seq.allow_frame_max_bvp_drl_bits,
            seq.enable_tcq,
            seq.enable_parity_hiding,
            seq.separate_uv_delta_q,
            seq.equal_ac_dc_q,
            seq.y_dc_delta_q_enabled,
            seq.uv_dc_delta_q_enabled,
            seq.uv_ac_delta_q_enabled,
            seq.enable_cdef,
            seq.enable_gdf,
            seq.enable_restoration,
            seq.enable_ccso,
            seq.df_par_bits,
            seq.film_grain_params_present
        );

        let fp = frame::parse_frame_header(frame_obu(&obus).payload, &seq).unwrap();
        let header_bytes = frame_obu(&obus).payload.len() - fp.tile_data.len();
        eprintln!(
            "FRAME: qindex={} disable_cdf={} oh={} header_bytes={} tile_data_len={} tx_mode_select={} tcq_mode={} reduced_tx_set={}",
            fp.base_qindex,
            fp.disable_cdf_update,
            fp.order_hint,
            header_bytes,
            fp.tile_data.len(),
            fp.tx_mode_select,
            fp.tcq_mode,
            fp.reduced_tx_set_used
        );
        eprintln!(
            "tile_data first 16 bytes: {:02x?}",
            &fp.tile_data[..fp.tile_data.len().min(16)]
        );
        eprintln!("expected tile_data start: f6 ac 00 72 85 91 72 ff (from reference)");
        let dequant = crate::encode::ac_qlookup(fp.base_qindex);

        let tile_cfg = tile::TileDecodeConfig {
            enable_cfl_intra: seq.enable_cfl_intra,
            enable_fsc: seq.enable_fsc,
            enable_idtx_intra: seq.enable_idtx_intra,
            enable_mrls: seq.enable_mrls,
            enable_ist: seq.enable_ist,
            enable_cctx: seq.enable_cctx,
            tx_mode_select: fp.tx_mode_select,
            coded_lossless: fp.base_qindex == 0,
            reduced_tx_set_used: fp.reduced_tx_set_used,
            reduced_tx_part_set: seq.reduced_tx_part_set,
            allow_intrabc: fp.allow_intrabc,
            allow_screen_content_tools: fp.allow_screen_content_tools,
            enable_sdp: seq.enable_sdp,
            cdef_frame_enable: fp.cdef_frame_enable,
            nb_cdef_strengths: fp.nb_cdef_strengths,
            ccso_frame_enable: fp.ccso_frame_enable,
            ccso_enable: fp.ccso_enable,
            sb_size: seq.sb_size as usize,
            tcq_mode: fp.tcq_mode,
            allow_parity_hiding: fp.allow_parity_hiding,
            enable_ext_partitions: seq.enable_ext_partitions,
            enable_uneven_4way_partitions: seq.enable_uneven_4way_partitions,
            enable_mhccp: seq.enable_mhccp,
            enable_refmvbank: seq.enable_refmvbank,
            max_bvp_drl_bits: fp.max_bvp_drl_bits,
            cur_frame_force_integer_mv: fp.cur_frame_force_integer_mv,
            allow_intrabc_morph_pred: seq.enable_intra_bawp && fp.allow_screen_content_tools,
        };

        let blocks = tile::decode_tile_data_partitioned_cfg(
            fp.tile_data,
            seq.width as usize,
            seq.height as usize,
            seq.sb_size as usize,
            fp.base_qindex,
            &tile_cfg,
        )
        .unwrap();

        for (i, block) in blocks.iter().enumerate() {
            let y_eob: u32 = block.y_tu.iter().map(|t| t.eob as u32).sum();
            eprintln!(
                "Block[{:2}]: ({:3},{:3}) {:3}x{:3} y_mode={:2} y_tu={} y_eob={}",
                i,
                block.x,
                block.y,
                block.width,
                block.height,
                block.y_mode,
                block.y_tu.len(),
                y_eob
            );
            for (ti, tu) in block.y_tu.iter().enumerate() {
                let residuals = transform::dequantize_and_inverse_tcq(
                    &tu.levels,
                    tu.eob,
                    dequant,
                    tu.tx_size,
                    tu.tx_type,
                    fp.tcq_mode,
                );
                let nonzero: usize = residuals.iter().filter(|&&v| v != 0).count();
                let dc = if !tu.levels.is_empty() {
                    tu.levels[0]
                } else {
                    0
                };
                eprintln!(
                    "  TU[{}]: tx={:?} eob={} dc_level={} nonzero_residuals={} first_res=[{}, {}, {}, {}]",
                    ti,
                    tu.tx_size,
                    tu.eob,
                    dc,
                    nonzero,
                    residuals[0],
                    residuals[1],
                    residuals[2],
                    residuals[3]
                );
            }
        }

        let luma_pixels: usize = blocks
            .iter()
            .filter(|b| !b.y_tu.is_empty())
            .map(|b| b.width * b.height)
            .sum();
        let chroma_blocks: usize = blocks.iter().filter(|b| b.y_tu.is_empty()).count();
        eprintln!(
            "luma_pixels={} chroma_blocks={} total_blocks={}",
            luma_pixels,
            chroma_blocks,
            blocks.len()
        );
        assert!(
            luma_pixels <= 128 * 128,
            "luma coverage exceeds frame: {}",
            luma_pixels
        );

        let uv_dequant_128 = crate::encode::ac_qlookup(
            (fp.base_qindex as i32 + fp.u_ac_delta_q).clamp(0, 255) as u8,
        );
        let (y_plane, _u_plane, _v_plane) = reconstruct_partitioned_frame(
            &blocks,
            seq.width,
            seq.height,
            dequant,
            uv_dequant_128,
            fp.tcq_mode,
            fp.base_qindex == 0,
            seq.cfl_ds_filter_index,
            seq.sb_size as usize,
            seq.enable_ibp,
            !seq.enable_intra_edge_filter,
            [qm::NUM_QM_LEVELS as u8 - 1; 3],
            &fp.ccso_params,
        );

        let Some(avmdec) = avmdec_executable() else {
            eprintln!("SKIP: avmdec not found");
            return;
        };
        let output = std::process::Command::new(avmdec)
            .args([
                "--codec=av2",
                "-o",
                "/tmp/avmenc_128x128_q30_ref.y4m",
                "tests/fixtures/avmenc_128x128_q30.obu",
            ])
            .output()
            .expect("avmdec not found");
        assert!(output.status.success(), "avmdec failed");

        let ref_y4m = std::fs::read("/tmp/avmenc_128x128_q30_ref.y4m").unwrap();
        let w = seq.width as usize;
        let h = seq.height as usize;
        let y4m_header = format!("YUV4MPEG2 W{} H{} F0:0 Ip C420jpeg\nFRAME\n", w, h);
        let ref_y = &ref_y4m[y4m_header.len()..y4m_header.len() + w * h];

        let y_mse: f64 = y_plane
            .iter()
            .zip(ref_y.iter())
            .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
            .sum::<f64>()
            / (w * h) as f64;
        let y_psnr = if y_mse > 0.0 {
            10.0 * (255.0f64 * 255.0 / y_mse).log10()
        } else {
            100.0
        };
        eprintln!(
            "Y PSNR={:.2} dB (MSE={:.2}) sb_size={}",
            y_psnr, y_mse, seq.sb_size
        );

        for (bi, block) in blocks.iter().enumerate() {
            let (bx, by, bw, bh) = (block.x, block.y, block.width, block.height);
            let mut block_sse = 0.0f64;
            let mut block_count = 0usize;
            for row in by..(by + bh).min(h) {
                for col in bx..(bx + bw).min(w) {
                    let ours = y_plane[row * w + col] as f64;
                    let theirs = ref_y[row * w + col] as f64;
                    block_sse += (ours - theirs).powi(2);
                    block_count += 1;
                }
            }
            let block_mse = block_sse / block_count as f64;
            let block_psnr = if block_mse > 0.0 {
                10.0 * (255.0f64 * 255.0 / block_mse).log10()
            } else {
                100.0
            };

            if block_psnr < 40.0 {
                let sample_row = by;
                let end_col = (bx + 8).min(bx + bw).min(w);
                let our_samples: Vec<u8> =
                    (bx..end_col).map(|c| y_plane[sample_row * w + c]).collect();
                let ref_samples: Vec<u8> =
                    (bx..end_col).map(|c| ref_y[sample_row * w + c]).collect();
                eprintln!(
                    "  BAD Block[{:2}] ({},{}) {}x{}: PSNR={:.1} ours={:?} ref={:?}",
                    bi, bx, by, bw, bh, block_psnr, our_samples, ref_samples
                );
            }
        }
    }

    #[test]
    fn decode_avmenc_nofilter_psnr() {
        let obu_path = "tests/fixtures/avmenc_128x128_q30_nofilter.obu";
        if !std::path::Path::new(obu_path).exists() {
            eprintln!("SKIP: {} not found", obu_path);
            return;
        }
        let data = std::fs::read(obu_path).unwrap();
        let frames = Decoder::decode_all(&data).unwrap();
        assert_eq!(frames.len(), 1);
        let frame = &frames[0];
        let w = frame.width as usize;
        let h = frame.height as usize;

        let Some(avmdec) = avmdec_executable() else {
            eprintln!("SKIP: avmdec not found");
            return;
        };
        let ref_path = "/tmp/avmenc_128x128_q30_nofilter_ref.y4m";
        let output = std::process::Command::new(avmdec)
            .args(["--codec=av2", "-o", ref_path, obu_path])
            .output()
            .expect("avmdec not found");
        assert!(
            output.status.success(),
            "avmdec failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let ref_y4m = std::fs::read(ref_path).unwrap();
        let y4m_header = format!("YUV4MPEG2 W{} H{} F0:0 Ip C420jpeg\nFRAME\n", w, h);
        let hlen = y4m_header.len();
        let ref_y = &ref_y4m[hlen..hlen + w * h];
        let ref_u = &ref_y4m[hlen + w * h..hlen + w * h + w / 2 * h / 2];
        let ref_v = &ref_y4m[hlen + w * h + w / 2 * h / 2..hlen + w * h + 2 * (w / 2 * h / 2)];

        let y_mse: f64 = frame
            .y
            .iter()
            .zip(ref_y.iter())
            .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
            .sum::<f64>()
            / (w * h) as f64;
        let y_psnr = if y_mse > 0.0 {
            10.0 * (255.0f64 * 255.0 / y_mse).log10()
        } else {
            100.0
        };

        let u_mse: f64 = frame
            .u
            .iter()
            .zip(ref_u.iter())
            .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
            .sum::<f64>()
            / (w / 2 * h / 2) as f64;
        let u_psnr = if u_mse > 0.0 {
            10.0 * (255.0f64 * 255.0 / u_mse).log10()
        } else {
            100.0
        };

        let v_mse: f64 = frame
            .v
            .iter()
            .zip(ref_v.iter())
            .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
            .sum::<f64>()
            / (w / 2 * h / 2) as f64;
        let v_psnr = if v_mse > 0.0 {
            10.0 * (255.0f64 * 255.0 / v_mse).log10()
        } else {
            100.0
        };

        eprintln!(
            "No-filter PSNR: Y={:.2} dB  U={:.2} dB  V={:.2} dB",
            y_psnr, u_psnr, v_psnr
        );
        for row_y in [0, 8, 16, 32, 48, 64, 80, 96, 112] {
            if row_y < h {
                let ours: Vec<u8> = (0..16.min(w)).map(|c| frame.y[row_y * w + c]).collect();
                let refs: Vec<u8> = (0..16.min(w)).map(|c| ref_y[row_y * w + c]).collect();
                let row_mse: f64 = (0..w)
                    .map(|c| (frame.y[row_y * w + c] as f64 - ref_y[row_y * w + c] as f64).powi(2))
                    .sum::<f64>()
                    / w as f64;
                eprintln!(
                    "Row {:3}: ours={:?} ref={:?} row_mse={:.1}",
                    row_y, ours, refs, row_mse
                );
            }
        }

        let obus = obu::parse_obus(&data);
        let seq = sequence::parse_sequence_header(sequence_obu(&obus).payload).unwrap();
        let fp = frame::parse_frame_header(frame_obu(&obus).payload, &seq).unwrap();
        let dequant = crate::encode::ac_qlookup(fp.base_qindex);
        eprintln!(
            "SEQ: sb_size={} sdp={} cdef={} gdf={} rest={} ccso={} tcq={}",
            seq.sb_size,
            seq.enable_sdp,
            seq.enable_cdef,
            seq.enable_gdf,
            seq.enable_restoration,
            seq.enable_ccso,
            seq.enable_tcq
        );
        eprintln!(
            "FP: qindex={} dequant={} tx_mode_select={} disable_cdf={}",
            fp.base_qindex, dequant, fp.tx_mode_select, fp.disable_cdf_update
        );

        let tile_cfg = tile::TileDecodeConfig {
            enable_cfl_intra: seq.enable_cfl_intra,
            enable_fsc: seq.enable_fsc,
            enable_idtx_intra: seq.enable_idtx_intra,
            enable_mrls: seq.enable_mrls,
            enable_ist: seq.enable_ist,
            enable_cctx: seq.enable_cctx,
            tx_mode_select: fp.tx_mode_select,
            coded_lossless: fp.base_qindex == 0,
            reduced_tx_set_used: fp.reduced_tx_set_used,
            reduced_tx_part_set: seq.reduced_tx_part_set,
            allow_intrabc: fp.allow_intrabc,
            allow_screen_content_tools: fp.allow_screen_content_tools,
            enable_sdp: seq.enable_sdp,
            cdef_frame_enable: fp.cdef_frame_enable,
            nb_cdef_strengths: fp.nb_cdef_strengths,
            ccso_frame_enable: fp.ccso_frame_enable,
            ccso_enable: fp.ccso_enable,
            sb_size: seq.sb_size as usize,
            tcq_mode: fp.tcq_mode,
            allow_parity_hiding: fp.allow_parity_hiding,
            enable_ext_partitions: seq.enable_ext_partitions,
            enable_uneven_4way_partitions: seq.enable_uneven_4way_partitions,
            enable_mhccp: seq.enable_mhccp,
            enable_refmvbank: seq.enable_refmvbank,
            max_bvp_drl_bits: fp.max_bvp_drl_bits,
            cur_frame_force_integer_mv: fp.cur_frame_force_integer_mv,
            allow_intrabc_morph_pred: seq.enable_intra_bawp && fp.allow_screen_content_tools,
        };
        let blocks = tile::decode_tile_data_partitioned_cfg(
            fp.tile_data,
            w,
            h,
            seq.sb_size as usize,
            fp.base_qindex,
            &tile_cfg,
        )
        .unwrap();
        for (i, block) in blocks.iter().enumerate().take(15) {
            let y_eob: u32 = block.y_tu.iter().map(|t| t.eob as u32).sum();
            eprintln!(
                "NF Block[{:2}]: ({:3},{:3}) {:3}x{:3} y_mode={:2} y_tu={} y_eob={}",
                i,
                block.x,
                block.y,
                block.width,
                block.height,
                block.y_mode,
                block.y_tu.len(),
                y_eob
            );
            for (ti, tu) in block.y_tu.iter().enumerate() {
                let residuals = transform::dequantize_and_inverse_tcq(
                    &tu.levels,
                    tu.eob,
                    dequant,
                    tu.tx_size,
                    tu.tx_type,
                    fp.tcq_mode,
                );
                let dc = if !tu.levels.is_empty() {
                    tu.levels[0]
                } else {
                    0
                };
                eprintln!(
                    "  TU[{}]: tx={:?} eob={} dc_level={} first_res=[{}, {}, {}, {}]",
                    ti,
                    tu.tx_size,
                    tu.eob,
                    dc,
                    residuals[0],
                    residuals[1],
                    residuals[2],
                    residuals[3]
                );
            }
        }
    }

    #[test]
    fn decode_avmenc_noibc_psnr() {
        let obu_path = "tests/fixtures/avmenc_128x128_q30_noibc.obu";
        if !std::path::Path::new(obu_path).exists() {
            eprintln!("SKIP: {} not found", obu_path);
            return;
        }
        let data = std::fs::read(obu_path).unwrap();
        let frames = Decoder::decode_all(&data).unwrap();
        assert_eq!(frames.len(), 1);
        let frame = &frames[0];
        let w = frame.width as usize;
        let h = frame.height as usize;

        let Some(avmdec) = avmdec_executable() else {
            eprintln!("SKIP: avmdec not found");
            return;
        };
        let ref_path = "/tmp/avmenc_128x128_q30_noibc_ref.y4m";
        let output = std::process::Command::new(avmdec)
            .args(["--codec=av2", "-o", ref_path, obu_path])
            .output()
            .expect("avmdec not found");
        assert!(
            output.status.success(),
            "avmdec failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let ref_y4m = std::fs::read(ref_path).unwrap();
        let y4m_header = format!("YUV4MPEG2 W{} H{} F0:0 Ip C420jpeg\nFRAME\n", w, h);
        let hlen = y4m_header.len();
        let ref_y = &ref_y4m[hlen..hlen + w * h];

        let y_mse: f64 = frame
            .y
            .iter()
            .zip(ref_y.iter())
            .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
            .sum::<f64>()
            / (w * h) as f64;
        let y_psnr = if y_mse > 0.0 {
            10.0 * (255.0f64 * 255.0 / y_mse).log10()
        } else {
            100.0
        };

        let ref_u = &ref_y4m[hlen + w * h..hlen + w * h + w / 2 * h / 2];
        let ref_v = &ref_y4m[hlen + w * h + w / 2 * h / 2..hlen + w * h + 2 * (w / 2 * h / 2)];
        let u_mse: f64 = frame
            .u
            .iter()
            .zip(ref_u.iter())
            .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
            .sum::<f64>()
            / (w / 2 * h / 2) as f64;
        let u_psnr = if u_mse > 0.0 {
            10.0 * (255.0f64 * 255.0 / u_mse).log10()
        } else {
            100.0
        };
        let v_mse: f64 = frame
            .v
            .iter()
            .zip(ref_v.iter())
            .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
            .sum::<f64>()
            / (w / 2 * h / 2) as f64;
        let v_psnr = if v_mse > 0.0 {
            10.0 * (255.0f64 * 255.0 / v_mse).log10()
        } else {
            100.0
        };

        eprintln!("=== OVERALL PSNR ===");
        eprintln!(
            "NOIBC PSNR: Y={:.2} dB  U={:.2} dB  V={:.2} dB",
            y_psnr, u_psnr, v_psnr
        );
        eprintln!(
            "[ROW63] ours[0..32]={:?}",
            &frame.y[63 * w..63 * w + 32]
                .iter()
                .map(|&v| v)
                .collect::<Vec<u8>>()
        );
        eprintln!(
            "[ROW63] ours[32..64]={:?}",
            &frame.y[63 * w + 32..63 * w + 64]
                .iter()
                .map(|&v| v)
                .collect::<Vec<u8>>()
        );
        eprintln!(
            "[ROW63] ref[0..32]={:?}",
            &ref_y[63 * w..63 * w + 32]
                .iter()
                .map(|&v| v)
                .collect::<Vec<u8>>()
        );
        eprintln!(
            "[ROW63] ref[32..64]={:?}",
            &ref_y[63 * w + 32..63 * w + 64]
                .iter()
                .map(|&v| v)
                .collect::<Vec<u8>>()
        );
        eprintln!("Y MSE={:.4}  U MSE={:.4}  V MSE={:.4}", y_mse, u_mse, v_mse);

        eprintln!("\n=== PER-SB (64x64 quadrant) Y PSNR ===");
        let sb_regions = [
            ("Top-Left", 0usize, 0usize, 64usize, 64usize),
            ("Top-Right", 64, 0, 64, 64),
            ("Bottom-Left", 0, 64, 64, 64),
            ("Bottom-Right", 64, 64, 64, 64),
        ];
        for (name, sx, sy, sw, sh) in &sb_regions {
            let mut mse_sum = 0.0f64;
            let mut count = 0usize;
            let mut max_err = 0i32;
            let mut max_err_pos = (0usize, 0usize);
            for r in *sy..(*sy + *sh).min(h) {
                for c in *sx..(*sx + *sw).min(w) {
                    let ours = frame.y[r * w + c] as i32;
                    let refs = ref_y[r * w + c] as i32;
                    let diff = (ours - refs).abs();
                    mse_sum += (diff as f64).powi(2);
                    count += 1;
                    if diff > max_err {
                        max_err = diff;
                        max_err_pos = (c, r);
                    }
                }
            }
            let sb_mse = mse_sum / count as f64;
            let sb_psnr = if sb_mse > 0.0 {
                10.0 * (255.0f64 * 255.0 / sb_mse).log10()
            } else {
                100.0
            };
            eprintln!(
                "  {} ({},{})-({},{}) : PSNR={:.2} dB  MSE={:.4}  max_err={} at ({},{})",
                name,
                sx,
                sy,
                sx + sw,
                sy + sh,
                sb_psnr,
                sb_mse,
                max_err,
                max_err_pos.0,
                max_err_pos.1
            );
        }

        eprintln!("\n=== PER-BLOCK Y ANALYSIS ===");
        let obus = obu::parse_obus(&data);
        let seq = sequence::parse_sequence_header(sequence_obu(&obus).payload).unwrap();
        let fp = frame::parse_frame_header(frame_obu(&obus).payload, &seq).unwrap();
        let _dequant = crate::encode::ac_qlookup(fp.base_qindex);
        let tile_cfg = tile::TileDecodeConfig {
            enable_cfl_intra: seq.enable_cfl_intra,
            enable_fsc: seq.enable_fsc,
            enable_idtx_intra: seq.enable_idtx_intra,
            enable_mrls: seq.enable_mrls,
            enable_ist: seq.enable_ist,
            enable_cctx: seq.enable_cctx,
            tx_mode_select: fp.tx_mode_select,
            coded_lossless: fp.base_qindex == 0,
            reduced_tx_set_used: fp.reduced_tx_set_used,
            reduced_tx_part_set: seq.reduced_tx_part_set,
            allow_intrabc: fp.allow_intrabc,
            allow_screen_content_tools: fp.allow_screen_content_tools,
            enable_sdp: seq.enable_sdp,
            cdef_frame_enable: fp.cdef_frame_enable,
            nb_cdef_strengths: fp.nb_cdef_strengths,
            ccso_frame_enable: fp.ccso_frame_enable,
            ccso_enable: fp.ccso_enable,
            sb_size: seq.sb_size as usize,
            tcq_mode: fp.tcq_mode,
            allow_parity_hiding: fp.allow_parity_hiding,
            enable_ext_partitions: seq.enable_ext_partitions,
            enable_uneven_4way_partitions: seq.enable_uneven_4way_partitions,
            enable_mhccp: seq.enable_mhccp,
            enable_refmvbank: seq.enable_refmvbank,
            max_bvp_drl_bits: fp.max_bvp_drl_bits,
            cur_frame_force_integer_mv: fp.cur_frame_force_integer_mv,
            allow_intrabc_morph_pred: seq.enable_intra_bawp && fp.allow_screen_content_tools,
        };
        let blocks = tile::decode_tile_data_partitioned_cfg(
            fp.tile_data,
            w,
            h,
            seq.sb_size as usize,
            fp.base_qindex,
            &tile_cfg,
        )
        .unwrap();

        let mode_name = |joint: u8| -> &str {
            let (_mode, _delta) = predict::decode_prediction_mode(joint);
            match _mode {
                predict::PredictionMode::DcPred => "DC",
                predict::PredictionMode::VPred => "V",
                predict::PredictionMode::HPred => "H",
                predict::PredictionMode::D45Pred => "D45",
                predict::PredictionMode::D135Pred => "D135",
                predict::PredictionMode::D113Pred => "D113",
                predict::PredictionMode::D157Pred => "D157",
                predict::PredictionMode::D203Pred => "D203",
                predict::PredictionMode::D67Pred => "D67",
                predict::PredictionMode::SmoothPred => "SMOOTH",
                predict::PredictionMode::SmoothVPred => "SMOOTH_V",
                predict::PredictionMode::SmoothHPred => "SMOOTH_H",
                predict::PredictionMode::PaethPred => "PAETH",
            }
        };

        let mut error_blocks: Vec<(usize, f64, i32)> = Vec::new();
        for (i, block) in blocks.iter().enumerate() {
            let bx = block.x;
            let by = block.y;
            let bw = block.width;
            let bh = block.height;
            let mut block_mse_sum = 0.0f64;
            let mut block_count = 0usize;
            let mut block_max_err = 0i32;
            let mut _block_max_pos = (0usize, 0usize);

            for r in by..(by + bh).min(h) {
                for c in bx..(bx + bw).min(w) {
                    let ours = frame.y[r * w + c] as i32;
                    let refs = ref_y[r * w + c] as i32;
                    let diff = (ours - refs).abs();
                    block_mse_sum += (diff as f64).powi(2);
                    block_count += 1;
                    if diff > block_max_err {
                        block_max_err = diff;
                        _block_max_pos = (c, r);
                    }
                }
            }
            let block_mse = if block_count > 0 {
                block_mse_sum / block_count as f64
            } else {
                0.0
            };
            if block_mse > 0.0 {
                error_blocks.push((i, block_mse, block_max_err));
            }
        }

        error_blocks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        eprintln!(
            "Total blocks: {}  Blocks with non-zero MSE: {}",
            blocks.len(),
            error_blocks.len()
        );

        for &(i, block_mse, max_err) in error_blocks.iter() {
            let block = &blocks[i];
            let bx = block.x;
            let by = block.y;
            let bw = block.width;
            let bh = block.height;
            let (_mode, angle_delta) = predict::decode_prediction_mode(block.joint_y_mode);
            let y_eob: u32 = block.y_tu.iter().map(|t| t.eob as u32).sum();
            let is_palette = !block.palette_y_map.is_empty();
            let block_psnr = if block_mse > 0.0 {
                10.0 * (255.0f64 * 255.0 / block_mse).log10()
            } else {
                100.0
            };

            eprintln!(
                "  Block[{:3}] ({:3},{:3}) {:3}x{:3} mode={:<8} delta={:2} palette={} y_tu={} y_eob={:4} | MSE={:.2} PSNR={:.1}dB max_err={}",
                i,
                bx,
                by,
                bw,
                bh,
                mode_name(block.joint_y_mode),
                angle_delta,
                is_palette,
                block.y_tu.len(),
                y_eob,
                block_mse,
                block_psnr,
                max_err
            );

            if block_mse > 10.0 {
                eprintln!("    Sample pixels (first row of block):");
                let r = by;
                let end_c = (bx + bw).min(w).min(bx + 16);
                let ours_row: Vec<u8> = (bx..end_c).map(|c| frame.y[r * w + c]).collect();
                let refs_row: Vec<u8> = (bx..end_c).map(|c| ref_y[r * w + c]).collect();
                let diffs: Vec<i32> = (bx..end_c)
                    .map(|c| frame.y[r * w + c] as i32 - ref_y[r * w + c] as i32)
                    .collect();
                eprintln!("      ours: {:?}", ours_row);
                eprintln!("      ref:  {:?}", refs_row);
                eprintln!("      diff: {:?}", diffs);
                if bh > 1 && by + 1 < h {
                    let r2 = by + 1;
                    let ours_row2: Vec<u8> = (bx..end_c).map(|c| frame.y[r2 * w + c]).collect();
                    let refs_row2: Vec<u8> = (bx..end_c).map(|c| ref_y[r2 * w + c]).collect();
                    let diffs2: Vec<i32> = (bx..end_c)
                        .map(|c| frame.y[r2 * w + c] as i32 - ref_y[r2 * w + c] as i32)
                        .collect();
                    eprintln!("    Row+1:");
                    eprintln!("      ours: {:?}", ours_row2);
                    eprintln!("      ref:  {:?}", refs_row2);
                    eprintln!("      diff: {:?}", diffs2);
                }
            }
        }

        eprintln!("\n=== PER-ROW Y MSE (all 128 rows) ===");
        let mut worst_rows: Vec<(usize, f64)> = Vec::new();
        for row in 0..h {
            let row_mse: f64 = (0..w)
                .map(|c| {
                    let d = frame.y[row * w + c] as f64 - ref_y[row * w + c] as f64;
                    d * d
                })
                .sum::<f64>()
                / w as f64;
            if row_mse > 0.0 {
                worst_rows.push((row, row_mse));
            }
        }
        worst_rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        for &(row, row_mse) in worst_rows.iter().take(20) {
            let row_psnr = if row_mse > 0.0 {
                10.0 * (255.0f64 * 255.0 / row_mse).log10()
            } else {
                100.0
            };
            eprintln!(
                "  Row {:3}: MSE={:.2}  PSNR={:.1}dB",
                row, row_mse, row_psnr
            );
        }
        if worst_rows.len() > 20 {
            eprintln!(
                "  ... and {} more rows with non-zero MSE",
                worst_rows.len() - 20
            );
        }

        eprintln!("\n=== PER-BLOCK UV ANALYSIS (worst 20) ===");
        let uv_w = w / 2;
        let uv_h = h / 2;
        let mut uv_error_blocks: Vec<(usize, f64, f64, &str)> = Vec::new();
        for (i, block) in blocks.iter().enumerate() {
            let uv_bx = block.x / 2;
            let uv_by = block.y / 2;
            let uv_bw = block.width / 2;
            let uv_bh = block.height / 2;
            if uv_bw < 4 || uv_bh < 4 {
                continue;
            }

            let mut u_mse_sum = 0.0f64;
            let mut v_mse_sum = 0.0f64;
            let mut count = 0usize;
            for r in uv_by..(uv_by + uv_bh).min(uv_h) {
                for c in uv_bx..(uv_bx + uv_bw).min(uv_w) {
                    let ou = frame.u[r * uv_w + c] as f64;
                    let ru = ref_u[r * uv_w + c] as f64;
                    u_mse_sum += (ou - ru).powi(2);
                    let ov = frame.v[r * uv_w + c] as f64;
                    let rv = ref_v[r * uv_w + c] as f64;
                    v_mse_sum += (ov - rv).powi(2);
                    count += 1;
                }
            }
            let u_mse = if count > 0 {
                u_mse_sum / count as f64
            } else {
                0.0
            };
            let v_mse = if count > 0 {
                v_mse_sum / count as f64
            } else {
                0.0
            };
            let cfl_type = if block.uv_mode == 13 {
                "CFL"
            } else {
                "non-CFL"
            };
            if u_mse > 0.01 || v_mse > 0.01 {
                uv_error_blocks.push((i, u_mse, v_mse, cfl_type));
            }
        }
        uv_error_blocks.sort_by(|a, b| (b.1 + b.2).partial_cmp(&(a.1 + a.2)).unwrap());
        for &(i, u_mse, v_mse, cfl_type) in uv_error_blocks.iter().take(20) {
            let block = &blocks[i];
            let uv_mode_str = match block.uv_mode {
                0 => "DC",
                1 => "V",
                2 => "H",
                3 => "D45",
                4 => "D135",
                5 => "D113",
                6 => "D157",
                7 => "D203",
                8 => "D67",
                9 => "SMOOTH",
                10 => "SMOOTH_V",
                11 => "SMOOTH_H",
                12 => "PAETH",
                13 => "CFL",
                _ => "?",
            };
            let tree_str = match block.tree_type {
                tile::TreeType::Shared => "S",
                tile::TreeType::Luma => "L",
                tile::TreeType::Chroma => "C",
            };
            eprintln!(
                "  Block[{:3}] ({:3},{:3}) {:3}x{:3} tree={} uv_mode={:<8} {} cfl_idx={} mh_dir={} alpha=({},{}) | U_MSE={:.1} V_MSE={:.1} u_tu={} v_tu={}",
                i,
                block.x,
                block.y,
                block.width,
                block.height,
                tree_str,
                uv_mode_str,
                cfl_type,
                block.cfl_idx,
                block.mhccp_dir,
                block.cfl_alpha_u,
                block.cfl_alpha_v,
                u_mse,
                v_mse,
                block.u_tu.len(),
                block.v_tu.len()
            );
        }
        eprintln!("Total UV error blocks: {}", uv_error_blocks.len());
    }
}
