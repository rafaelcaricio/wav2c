use crate::cdf_context::FrameCdfContext;
use crate::cdf_tables::*;
use crate::entropy::DaalaCoder;

fn encode_sym(ec: &mut DaalaCoder, sym: usize, cdf: &mut Vec<u16>, update: bool) {
    if update {
        ec.encode_symbol_mut(sym, cdf);
    } else {
        let n = cdf.len() - 2;
        ec.encode_symbol(sym, &cdf[..n]);
    }
}

fn scan_for_num_coeffs(num_coeffs: usize) -> &'static [u16] {
    match num_coeffs {
        16 => &crate::scan::DEFAULT_SCAN_4X4,
        64 => &crate::scan::DEFAULT_SCAN_8X8,
        256 => &crate::scan::DEFAULT_SCAN_16X16,
        1024 => &crate::scan::DEFAULT_SCAN_32X32,
        _ => panic!("unsupported num_coeffs: {}", num_coeffs),
    }
}

fn encode_eob_var_size(
    ec: &mut DaalaCoder,
    eob: u16,
    num_coeffs: usize,
    pl_ctx: usize,
    cdf_ctx: &mut FrameCdfContext,
    update: bool,
) {
    let mut eob_pt = 0usize;
    for i in (0..EOB_GROUP_START.len()).rev() {
        if eob >= EOB_GROUP_START[i] {
            if i == 0 && eob == 0 { continue; }
            eob_pt = i;
            break;
        }
    }

    let eob_token = eob_pt - 1;

    match num_coeffs {
        16 => {
            encode_sym(ec, eob_token, &mut cdf_ctx.eob_multi_16[pl_ctx], update);
        }
        64 => {
            encode_sym(ec, eob_token, &mut cdf_ctx.eob_multi_64[pl_ctx], update);
        }
        256 => {
            if eob_token < EOB_PT_INDEX_COUNT - 1 {
                encode_sym(ec, eob_token, &mut cdf_ctx.eob_multi_256[pl_ctx], update);
            } else {
                encode_sym(ec, EOB_PT_INDEX_COUNT - 1, &mut cdf_ctx.eob_multi_256[pl_ctx], update);
                let extra_token = eob_token - (EOB_PT_INDEX_COUNT - 1);
                ec.encode_literal(extra_token as u32, 1);
            }
        }
        1024 => {
            if eob_token < EOB_PT_INDEX_COUNT - 1 {
                encode_sym(ec, eob_token, &mut cdf_ctx.eob_multi_1024[pl_ctx], update);
            } else {
                encode_sym(ec, EOB_PT_INDEX_COUNT - 1, &mut cdf_ctx.eob_multi_1024[pl_ctx], update);
                let extra_token = eob_token - (EOB_PT_INDEX_COUNT - 1);
                ec.encode_literal(extra_token as u32, 2);
            }
        }
        _ => panic!("unsupported num_coeffs for EOB: {}", num_coeffs),
    };

    let eob_offset_bits = EOB_OFFSET_BITS[eob_pt] as u32;
    if eob_offset_bits > 0 {
        let extra = eob - EOB_GROUP_START[eob_pt];
        let msb = (extra >> (eob_offset_bits - 1)) & 1;
        encode_sym(ec, msb as usize, &mut cdf_ctx.eob_extra, update);
        if eob_offset_bits > 1 {
            let rest = extra & ((1 << (eob_offset_bits - 1)) - 1);
            ec.encode_literal(rest as u32, eob_offset_bits - 1);
        }
    }
}

const ADAPTIVE_TABLE: [usize; 5] = [4, 8, 16, 32, 64];

fn get_adaptive_param(ctx: usize) -> u32 {
    let mut m = 0;
    while m < ADAPTIVE_TABLE.len() && ctx >= ADAPTIVE_TABLE[m] {
        m += 1;
    }
    (m + 1) as u32
}

fn encode_exp_golomb(ec: &mut DaalaCoder, mut x: u32, mut k: u32) {
    while x >= (1 << k) {
        ec.encode_literal(0, 1);
        x -= 1 << k;
        k += 1;
    }
    ec.encode_literal(1, 1);
    if k > 0 {
        ec.encode_literal(x, k);
    }
}

fn encode_truncated_rice(ec: &mut DaalaCoder, level: u32, m: u32, k: u32, cmax: u32) {
    let q = level >> m;
    if q >= cmax {
        ec.encode_literal(0, cmax);
        encode_exp_golomb(ec, level - (cmax << m), k);
    } else {
        let mask = (1u32 << m) - 1;
        if q > 0 {
            ec.encode_literal(0, q);
        }
        ec.encode_literal(1, 1);
        if m > 0 {
            ec.encode_literal(level & mask, m);
        }
    }
}

fn encode_adaptive_hr(ec: &mut DaalaCoder, hr: usize, hr_ctx: usize) {
    let m = get_adaptive_param(hr_ctx);
    let k = m + 1;
    let cmax = (m + 4).min(6);
    encode_truncated_rice(ec, hr as u32, m, k, cmax);
}

fn get_padded_idx(idx: usize, bwl: u32) -> usize {
    idx + ((idx >> bwl) << TX_PAD_HOR_LOG2)
}

fn is_lf_2d(row: usize, col: usize, plane: usize) -> bool {
    if plane == 0 {
        (row + col) < LF_2D_LIM
    } else {
        (row + col) < LF_2D_LIM_UV
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

fn get_lower_levels_ctx_lf_2d(levels: &[u8], coeff_idx: usize, bwl: u32) -> usize {
    let pos = get_padded_idx(coeff_idx, bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let mag = (levels[pos + 1] as usize).min(5)
        + (levels[pos + stride] as usize).min(5)
        + (levels[pos + stride + 1] as usize).min(5)
        + (levels[pos + 2] as usize).min(5)
        + (levels[pos + 2 * stride] as usize).min(5);
    let ctx = (mag + 1) >> 1;
    let row = coeff_idx >> bwl;
    let col = coeff_idx - (row << bwl);
    if coeff_idx == 0 {
        return ctx.min(8);
    }
    if row + col < 2 {
        return ctx.min(6) + 9;
    }
    ctx.min(4) + 16
}

fn get_lower_levels_ctx_lf_2d_chroma(
    levels: &[u8],
    coeff_idx: usize,
    bwl: u32,
    plane: usize,
) -> usize {
    let pos = get_padded_idx(coeff_idx, bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let mag = (levels[pos + 1] as usize).min(5)
        + (levels[pos + stride] as usize).min(5)
        + (levels[pos + stride + 1] as usize).min(5);
    let ctx = ((mag + 1) >> 1).min(3);
    if plane == 1 { ctx } else { ctx + 4 }
}

fn get_lower_levels_ctx_2d(levels: &[u8], coeff_idx: usize, bwl: u32, plane: usize) -> usize {
    let pos = get_padded_idx(coeff_idx, bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let mag = (levels[pos + 1] as usize).min(3)
        + (levels[pos + stride] as usize).min(3)
        + (levels[pos + stride + 1] as usize).min(3)
        + (levels[pos + 2] as usize).min(3)
        + (levels[pos + 2 * stride] as usize).min(3);
    let ctx = ((mag + 1) >> 1).min(4);
    if plane > 0 {
        return ctx;
    }
    let row = coeff_idx >> bwl;
    let col = coeff_idx - (row << bwl);
    if row + col < 6 {
        return ctx;
    }
    if row + col < 8 {
        return ctx + 5;
    }
    ctx + 10
}

fn get_lower_levels_ctx_2d_chroma(
    levels: &[u8],
    coeff_idx: usize,
    bwl: u32,
    plane: usize,
) -> usize {
    let pos = get_padded_idx(coeff_idx, bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let mag = (levels[pos + 1] as usize).min(3)
        + (levels[pos + stride] as usize).min(3)
        + (levels[pos + stride + 1] as usize).min(3);
    let ctx = ((mag + 1) >> 1).min(3);
    if plane == 1 { ctx } else { ctx + 4 }
}

fn get_br_ctx_lf_eob_2d(coeff_idx: usize) -> usize {
    if coeff_idx == 0 { 0 } else { 7 }
}

fn get_br_lf_ctx_2d(levels: &[u8], coeff_idx: usize, bwl: u32) -> usize {
    let row = coeff_idx >> bwl;
    let col = coeff_idx - (row << bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let pos = row * stride + col;
    let mag = (levels[pos + 1] as usize).min(MAX_VAL_BR_CTX as usize)
        + (levels[pos + stride] as usize).min(MAX_VAL_BR_CTX as usize)
        + (levels[pos + stride + 1] as usize).min(MAX_VAL_BR_CTX as usize);
    ((mag + 1) >> 1).min(6) + 7
}

fn get_br_lf_ctx_dc_2d(levels: &[u8], coeff_idx: usize, bwl: u32) -> usize {
    let stride = (1 << bwl) + TX_PAD_HOR;
    let pos = (coeff_idx >> bwl) * stride + (coeff_idx - ((coeff_idx >> bwl) << bwl));
    let mag = (levels[pos + 1] as usize).min(MAX_VAL_BR_CTX as usize)
        + (levels[pos + stride] as usize).min(MAX_VAL_BR_CTX as usize)
        + (levels[pos + stride + 1] as usize).min(MAX_VAL_BR_CTX as usize);
    ((mag + 1) >> 1).min(6)
}

fn get_br_ctx_2d(levels: &[u8], coeff_idx: usize, bwl: u32) -> usize {
    let row = coeff_idx >> bwl;
    let col = coeff_idx - (row << bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let pos = row * stride + col;
    let mag = (levels[pos + 1] as usize).min(MAX_VAL_BR_CTX as usize)
        + (levels[pos + stride] as usize).min(MAX_VAL_BR_CTX as usize)
        + (levels[pos + stride + 1] as usize).min(MAX_VAL_BR_CTX as usize);
    ((mag + 1) >> 1).min(6)
}

fn get_br_ctx_2d_chroma(levels: &[u8], coeff_idx: usize, bwl: u32) -> usize {
    let row = coeff_idx >> bwl;
    let col = coeff_idx - (row << bwl);
    let stride = (1 << bwl) + TX_PAD_HOR;
    let pos = row * stride + col;
    let mag = (levels[pos + 1] as usize).min(MAX_VAL_BR_CTX as usize)
        + (levels[pos + stride] as usize).min(MAX_VAL_BR_CTX as usize)
        + (levels[pos + stride + 1] as usize).min(MAX_VAL_BR_CTX as usize);
    ((mag + 1) >> 1).min(3)
}

fn encode_low_range(
    ec: &mut DaalaCoder,
    level: usize,
    base_levels: usize,
    cdf: &mut Vec<u16>,
    update: bool,
) {
    if level > base_levels {
        let remainder = level - 1 - base_levels;
        let sym = remainder.min(BR_CDF_SIZE - 1);
        encode_sym(ec, sym, cdf, update);
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

fn encode_high_range_full(
    ec: &mut DaalaCoder,
    level: usize,
    lf: bool,
    plane: usize,
    hr_avg: &mut usize,
) {
    let max_br = if lf {
        if plane == 0 {
            LF_MAX_BASE_BR_RANGE
        } else {
            LF_NUM_BASE_LEVELS + 1
        }
    } else {
        MAX_BASE_BR_RANGE
    };
    if level >= max_br {
        let hr = level - max_br;
        encode_adaptive_hr(ec, hr, *hr_avg);
        *hr_avg = (*hr_avg + hr) >> 1;
    }
}

struct PlaneCoeffs<'a> {
    levels: &'a [i32],
    eob: u16,
}

struct PlaneEncodeCtx {
    plane: usize,
    txs_ctx_idx: usize,
    bwl: u32,
    height: usize,
    num_coeffs: usize,
    dc_sign_ctx: usize,
    entropy_ctx: usize,
    u_has_coeffs: bool,
    tx_size_sqr: usize,
    lossless: bool,
}

fn encode_plane_full(
    ec: &mut DaalaCoder,
    coeffs: &PlaneCoeffs,
    ctx: &PlaneEncodeCtx,
    cdf: &mut FrameCdfContext,
    update: bool,
) {
    let plane = ctx.plane;
    let txs_ctx_idx = ctx.txs_ctx_idx;
    let bwl = ctx.bwl;
    let height = ctx.height;
    let dc_sign_ctx = ctx.dc_sign_ctx;
    let eob = coeffs.eob;

    let skip_sym = if eob == 0 { 1 } else { 0 };
    let txb_skip_cdf = match plane {
        0 => &mut cdf.txb_skip_y[txs_ctx_idx][ctx.entropy_ctx],
        1 => &mut cdf.txb_skip_y[txs_ctx_idx][ctx.entropy_ctx + 6],
        _ => {
            if ctx.u_has_coeffs {
                &mut cdf.txb_skip_v_u_nz[ctx.entropy_ctx]
            } else {
                &mut cdf.txb_skip_v[ctx.entropy_ctx]
            }
        }
    };
    encode_sym(ec, skip_sym, txb_skip_cdf, update);

    if eob == 0 {
        return;
    }

    let pl_ctx = if plane == 0 { 0 } else { 2 };
    let num_coeffs = ctx.num_coeffs;
    encode_eob_var_size(ec, eob, num_coeffs, pl_ctx, cdf, update);

    if plane == 0 && eob > 1 && ctx.tx_size_sqr < 3 && !ctx.lossless {
        encode_sym(ec, 0, &mut cdf.intra_ext_tx[ctx.tx_size_sqr.min(3)], update);
    }

    let scan = scan_for_num_coeffs(num_coeffs);
    let width = 1usize << bwl;
    let padded_stride = width + TX_PAD_HOR;
    let padded_height = height + 2;
    let mut levels_buf = vec![0u8; padded_stride * padded_height];

    let eob_scan_idx = (eob - 1) as usize;
    let eob_pos = scan[eob_scan_idx] as usize;
    let eob_row = eob_pos >> bwl;
    let eob_col = eob_pos - (eob_row << bwl);
    let eob_lf = is_lf_2d(eob_row, eob_col, plane);
    let eob_abs_level = coeffs.levels[eob_scan_idx].unsigned_abs() as usize;

    let coeff_ctx = get_lower_levels_ctx_eob(bwl, height, eob_scan_idx);

    if plane == 0 {
        if eob_lf {
            let sym = eob_abs_level.min(LF_BASE_SYMBOLS - 1) - 1;
            encode_sym(ec, sym, &mut cdf.coeff_base_lf_eob_y[txs_ctx_idx][coeff_ctx], update);
            if eob_abs_level > LF_NUM_BASE_LEVELS {
                let br_ctx = get_br_ctx_lf_eob_2d(eob_pos);
                encode_low_range(ec, eob_abs_level, LF_NUM_BASE_LEVELS, &mut cdf.coeff_br_lf_y[br_ctx], update);
            }
        } else {
            let sym = eob_abs_level.min(NUM_BASE_LEVELS + 1) - 1;
            encode_sym(ec, sym, &mut cdf.coeff_base_eob_y[txs_ctx_idx][coeff_ctx], update);
            if eob_abs_level > NUM_BASE_LEVELS {
                encode_low_range(ec, eob_abs_level, NUM_BASE_LEVELS, &mut cdf.coeff_br_y[0], update);
            }
        }
    } else if eob_lf {
        let sym = eob_abs_level.min(LF_BASE_SYMBOLS - 1) - 1;
        encode_sym(ec, sym, &mut cdf.coeff_base_lf_eob_uv[coeff_ctx], update);
    } else {
        let sym = eob_abs_level.min(NUM_BASE_LEVELS + 1) - 1;
        encode_sym(ec, sym, &mut cdf.coeff_base_eob_uv[coeff_ctx], update);
        if eob_abs_level > NUM_BASE_LEVELS {
            encode_low_range(ec, eob_abs_level, NUM_BASE_LEVELS, &mut cdf.coeff_br_uv[0], update);
        }
    }

    levels_buf[get_padded_idx(eob_pos, bwl)] = stored_level(eob_abs_level, eob_lf, plane);

    if eob > 1 {
        for c in (1..eob_scan_idx).rev() {
            let pos = scan[c] as usize;
            let abs_level = coeffs.levels[c].unsigned_abs() as usize;
            let row = pos >> bwl;
            let col = pos - (row << bwl);
            let lf = is_lf_2d(row, col, plane);

            if plane == 0 {
                if lf {
                    let lctx = get_lower_levels_ctx_lf_2d(&levels_buf, pos, bwl);
                    encode_sym(ec, abs_level.min(LF_BASE_SYMBOLS - 1), &mut cdf.coeff_base_lf_y[txs_ctx_idx][lctx][0], update);
                    if abs_level > LF_NUM_BASE_LEVELS {
                        let br_ctx = get_br_lf_ctx_2d(&levels_buf, pos, bwl);
                        encode_low_range(ec, abs_level, LF_NUM_BASE_LEVELS, &mut cdf.coeff_br_lf_y[br_ctx], update);
                    }
                } else {
                    let lctx = get_lower_levels_ctx_2d(&levels_buf, pos, bwl, plane);
                    encode_sym(ec, abs_level.min(NUM_BASE_LEVELS + 1), &mut cdf.coeff_base_y[txs_ctx_idx][lctx][0], update);
                    if abs_level > NUM_BASE_LEVELS {
                        let br_ctx = get_br_ctx_2d(&levels_buf, pos, bwl);
                        encode_low_range(ec, abs_level, NUM_BASE_LEVELS, &mut cdf.coeff_br_y[br_ctx], update);
                    }
                }
            } else if lf {
                let lctx = get_lower_levels_ctx_lf_2d_chroma(&levels_buf, pos, bwl, plane);
                encode_sym(ec, abs_level.min(LF_BASE_SYMBOLS - 1), &mut cdf.coeff_base_lf_uv[lctx][0], update);
            } else {
                let lctx = get_lower_levels_ctx_2d_chroma(&levels_buf, pos, bwl, plane);
                encode_sym(ec, abs_level.min(NUM_BASE_LEVELS + 1), &mut cdf.coeff_base_uv[lctx][0], update);
                if abs_level > NUM_BASE_LEVELS {
                    let br_ctx = get_br_ctx_2d_chroma(&levels_buf, pos, bwl);
                    encode_low_range(ec, abs_level, NUM_BASE_LEVELS, &mut cdf.coeff_br_uv[br_ctx], update);
                }
            }

            levels_buf[get_padded_idx(pos, bwl)] = stored_level(abs_level, lf, plane);
        }

        let dc_pos = scan[0] as usize;
        let dc_abs_level = coeffs.levels[0].unsigned_abs() as usize;
        let dc_row = dc_pos >> bwl;
        let dc_col = dc_pos - (dc_row << bwl);
        let dc_lf = is_lf_2d(dc_row, dc_col, plane);

        if plane == 0 {
            if dc_lf {
                let lctx = get_lower_levels_ctx_lf_2d(&levels_buf, dc_pos, bwl);
                encode_sym(ec, dc_abs_level.min(LF_BASE_SYMBOLS - 1), &mut cdf.coeff_base_lf_y[txs_ctx_idx][lctx][0], update);
                if dc_abs_level > LF_NUM_BASE_LEVELS {
                    let br_ctx = get_br_lf_ctx_dc_2d(&levels_buf, dc_pos, bwl);
                    encode_low_range(ec, dc_abs_level, LF_NUM_BASE_LEVELS, &mut cdf.coeff_br_lf_y[br_ctx], update);
                }
            } else {
                let lctx = get_lower_levels_ctx_2d(&levels_buf, dc_pos, bwl, plane);
                encode_sym(ec, dc_abs_level.min(NUM_BASE_LEVELS + 1), &mut cdf.coeff_base_y[txs_ctx_idx][lctx][0], update);
                if dc_abs_level > NUM_BASE_LEVELS {
                    let br_ctx = get_br_ctx_2d(&levels_buf, dc_pos, bwl);
                    encode_low_range(ec, dc_abs_level, NUM_BASE_LEVELS, &mut cdf.coeff_br_y[br_ctx], update);
                }
            }
        } else if dc_lf {
            let lctx = get_lower_levels_ctx_lf_2d_chroma(&levels_buf, dc_pos, bwl, plane);
            encode_sym(ec, dc_abs_level.min(LF_BASE_SYMBOLS - 1), &mut cdf.coeff_base_lf_uv[lctx][0], update);
        } else {
            let lctx = get_lower_levels_ctx_2d_chroma(&levels_buf, dc_pos, bwl, plane);
            encode_sym(ec, dc_abs_level.min(NUM_BASE_LEVELS + 1), &mut cdf.coeff_base_uv[lctx][0], update);
            if dc_abs_level > NUM_BASE_LEVELS {
                let br_ctx = get_br_ctx_2d_chroma(&levels_buf, dc_pos, bwl);
                encode_low_range(ec, dc_abs_level, NUM_BASE_LEVELS, &mut cdf.coeff_br_uv[br_ctx], update);
            }
        }
        levels_buf[get_padded_idx(dc_pos, bwl)] = stored_level(dc_abs_level, dc_lf, plane);
    }

    let mut hr_avg = 0usize;
    for c in (0..eob as usize).rev() {
        let pos = scan[c] as usize;
        let level = coeffs.levels[c];
        let abs_level = level.unsigned_abs() as usize;

        if abs_level == 0 {
            continue;
        }

        let row = pos >> bwl;
        let col = pos - (row << bwl);
        let is_dc_2d = c == 0;

        if is_dc_2d {
            if plane == 0 {
                encode_sym(ec, if level < 0 { 1 } else { 0 }, &mut cdf.dc_sign_y[dc_sign_ctx], update);
            } else {
                ec.encode_literal(if level < 0 { 1 } else { 0 }, 1);
            }
        } else {
            ec.encode_literal(if level < 0 { 1 } else { 0 }, 1);
        }

        let lf = is_lf_2d(row, col, plane);
        encode_high_range_full(ec, abs_level, lf, plane, &mut hr_avg);
    }
}

fn encode_y_mode(
    ec: &mut DaalaCoder,
    mode_idx: usize,
    y_mode_idx_ctx: usize,
    lossless: bool,
    cdf: &mut FrameCdfContext,
    update: bool,
) {
    if lossless {
        encode_sym(ec, 0, &mut cdf.dpcm_y, update);
    }
    if mode_idx < 13 {
        encode_sym(ec, 0, &mut cdf.y_mode_set, update);
        if mode_idx < 7 {
            encode_sym(ec, mode_idx, &mut cdf.y_mode_idx[y_mode_idx_ctx], update);
        } else {
            encode_sym(ec, 7, &mut cdf.y_mode_idx[y_mode_idx_ctx], update);
            encode_sym(
                ec,
                mode_idx - 7,
                &mut cdf.y_mode_idx_offset[y_mode_idx_ctx],
                update,
            );
        }
    } else {
        let set_offset = mode_idx - 13;
        let set_index = set_offset / 16 + 1;
        let literal = set_offset % 16;
        encode_sym(ec, set_index, &mut cdf.y_mode_set, update);
        ec.encode_literal(literal as u32, 4);
    }
}

fn encode_uv_mode(
    ec: &mut DaalaCoder,
    uv_mode_idx: usize,
    uv_ctx: usize,
    lossless: bool,
    cdf: &mut FrameCdfContext,
    update: bool,
) {
    if lossless {
        encode_sym(ec, 0, &mut cdf.dpcm_uv, update);
    }
    if uv_mode_idx < 7 {
        encode_sym(ec, uv_mode_idx, &mut cdf.uv_mode[uv_ctx], update);
    } else {
        encode_sym(ec, 7, &mut cdf.uv_mode[uv_ctx], update);
        ec.encode_literal((uv_mode_idx - 7) as u32, 3);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn encode_tile_data_with_coeffs(
    y_levels: &[i32],
    y_eob: u16,
    u_levels: &[i32],
    u_eob: u16,
    v_levels: &[i32],
    v_eob: u16,
    y_mode: u8,
    uv_mode: u8,
) -> Vec<u8> {
    let mut ec = DaalaCoder::new();
    let mut cdf = FrameCdfContext::new();

    let y_intra = crate::encode::IntraMode::from_av2(y_mode);
    let uv_intra = crate::encode::IntraMode::from_av2(uv_mode);

    let y_list = crate::encode::build_y_mode_list(None, None, true, false);
    let y_joint = y_intra.joint_y_index();
    let y_mode_idx = crate::encode::find_mode_idx_in_list(&y_list, y_joint);

    let uv_list = crate::encode::build_uv_mode_list(y_intra);
    let uv_mode_idx = crate::encode::find_uv_mode_idx(&uv_list, uv_intra);
    let uv_ctx = if y_intra.is_directional() { 1 } else { 0 };

    encode_sym(&mut ec, 0, &mut cdf.do_split, false);
    encode_y_mode(&mut ec, y_mode_idx, 0, false, &mut cdf, false);
    encode_uv_mode(&mut ec, uv_mode_idx, uv_ctx, false, &mut cdf, false);

    let y_coeffs = PlaneCoeffs {
        levels: y_levels,
        eob: y_eob,
    };
    encode_plane_full(
        &mut ec,
        &y_coeffs,
        &PlaneEncodeCtx {
            plane: 0,
            txs_ctx_idx: 4,
            bwl: 5,
            height: 32,
            num_coeffs: 1024,

            dc_sign_ctx: 0,
            entropy_ctx: 0,
            u_has_coeffs: false,
            tx_size_sqr: 3,
            lossless: false,
        },
        &mut cdf,
        false,
    );

    let u_coeffs = PlaneCoeffs {
        levels: u_levels,
        eob: u_eob,
    };
    encode_plane_full(
        &mut ec,
        &u_coeffs,
        &PlaneEncodeCtx {
            plane: 1,
            txs_ctx_idx: 3,
            bwl: 5,
            height: 32,
            num_coeffs: 1024,

            dc_sign_ctx: 0,
            entropy_ctx: 0,
            u_has_coeffs: false,
            tx_size_sqr: 3,
            lossless: false,
        },
        &mut cdf,
        false,
    );

    let v_coeffs = PlaneCoeffs {
        levels: v_levels,
        eob: v_eob,
    };
    encode_plane_full(
        &mut ec,
        &v_coeffs,
        &PlaneEncodeCtx {
            plane: 2,
            txs_ctx_idx: 3,
            bwl: 5,
            height: 32,
            num_coeffs: 1024,

            dc_sign_ctx: 0,
            entropy_ctx: 0,
            u_has_coeffs: u_eob > 0,
            tx_size_sqr: 3,
            lossless: false,
        },
        &mut cdf,
        false,
    );

    ec.done()
}

pub fn encode_tile_data_with_dc(y_dc_level: i32, u_dc_level: i32, v_dc_level: i32) -> Vec<u8> {
    let mut y_levels = [0i32; 1024];
    let mut u_levels = [0i32; 1024];
    let mut v_levels = [0i32; 1024];
    y_levels[0] = y_dc_level;
    u_levels[0] = u_dc_level;
    v_levels[0] = v_dc_level;
    let y_eob = if y_dc_level != 0 { 1u16 } else { 0u16 };
    let u_eob = if u_dc_level != 0 { 1u16 } else { 0u16 };
    let v_eob = if v_dc_level != 0 { 1u16 } else { 0u16 };
    encode_tile_data_with_coeffs(&y_levels, y_eob, &u_levels, u_eob, &v_levels, v_eob, 0, 0)
}

pub fn encode_tile_data() -> Vec<u8> {
    encode_tile_data_with_dc(0, 0, 0)
}

const COEFF_CONTEXT_BITS: u32 = 3;
const COEFF_CONTEXT_MASK: u8 = (1 << COEFF_CONTEXT_BITS) - 1;

pub struct SbCoeffs {
    pub y_levels: [i32; 1024],
    pub y_eob: u16,
    pub u_levels: [i32; 1024],
    pub u_eob: u16,
    pub v_levels: [i32; 1024],
    pub v_eob: u16,
    pub y_mode: u8,
    pub uv_mode: u8,
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

pub fn encode_tile_data_multi_sb(
    sb_cols: usize,
    sb_rows: usize,
    sb_grid: &[SbCoeffs],
    allow_update_cdf: bool,
    base_qindex: u8,
) -> Vec<u8> {
    let mut ec = DaalaCoder::new();
    let mut cdf_ctx = FrameCdfContext::new_with_qindex(base_qindex);
    let update = allow_update_cdf;

    let mut above_y = vec![0u8; sb_cols * 16];
    let mut above_u = vec![0u8; sb_cols * 8];
    let mut above_v = vec![0u8; sb_cols * 8];
    let mut above_joint = vec![0usize; sb_cols];

    for sb_row in 0..sb_rows {
        let mut left_y = [0u8; 16];
        let mut left_u = [0u8; 8];
        let mut left_v = [0u8; 8];
        let mut left_joint: usize = 0;

        for sb_col in 0..sb_cols {
            let sb = &sb_grid[sb_row * sb_cols + sb_col];

            let y_intra = crate::encode::IntraMode::from_av2(sb.y_mode);
            let uv_intra = crate::encode::IntraMode::from_av2(sb.uv_mode);

            let left_j = if sb_col > 0 { Some(left_joint) } else { None };
            let above_j = if sb_row > 0 {
                Some(above_joint[sb_col])
            } else {
                None
            };

            let y_list =
                crate::encode::build_y_mode_list(left_j, above_j, true, false);
            let y_joint = y_intra.joint_y_index();
            let y_mode_idx =
                crate::encode::find_mode_idx_in_list(&y_list, y_joint);

            let uv_list = crate::encode::build_uv_mode_list(y_intra);
            let uv_mode_idx = crate::encode::find_uv_mode_idx(&uv_list, uv_intra);
            let uv_ctx = if y_intra.is_directional() { 1 } else { 0 };

            let is_left_dir = left_j
                .map(|j| j >= 5)
                .unwrap_or(false);
            let is_above_dir = above_j
                .map(|j| j >= 5)
                .unwrap_or(false);
            let y_mode_idx_ctx = is_left_dir as usize + is_above_dir as usize;

            encode_sym(&mut ec, 0, &mut cdf_ctx.do_split, update);
            encode_y_mode(&mut ec, y_mode_idx, y_mode_idx_ctx, false, &mut cdf_ctx, update);
            encode_uv_mode(&mut ec, uv_mode_idx, uv_ctx, false, &mut cdf_ctx, update);

            left_joint = y_joint;
            above_joint[sb_col] = y_joint;

            let y_above = &above_y[sb_col * 16..(sb_col + 1) * 16];
            let y_dc_sign_ctx = compute_dc_sign_ctx(y_above, &left_y);
            let y_coeffs = PlaneCoeffs {
                levels: &sb.y_levels,
                eob: sb.y_eob,
            };
            encode_plane_full(
                &mut ec,
                &y_coeffs,
                &PlaneEncodeCtx {
                    plane: 0,
                    txs_ctx_idx: 4,
                    bwl: 5,
                    height: 32,
                    num_coeffs: 1024,
        
                    dc_sign_ctx: y_dc_sign_ctx,
                    entropy_ctx: 0,
                    u_has_coeffs: false,
                    tx_size_sqr: 3,
                    lossless: false,
                },
                &mut cdf_ctx,
                update,
            );

            let y_cul = compute_cul_level(&sb.y_levels, sb.y_eob);
            for v in &mut above_y[sb_col * 16..(sb_col + 1) * 16] {
                *v = y_cul;
            }
            left_y.fill(y_cul);

            let u_above = &above_u[sb_col * 8..(sb_col + 1) * 8];
            let u_entropy_ctx = compute_entropy_ctx(u_above, &left_u);
            let u_coeffs = PlaneCoeffs {
                levels: &sb.u_levels,
                eob: sb.u_eob,
            };
            encode_plane_full(
                &mut ec,
                &u_coeffs,
                &PlaneEncodeCtx {
                    plane: 1,
                    txs_ctx_idx: 3,
                    bwl: 5,
                    height: 32,
                    num_coeffs: 1024,
                    dc_sign_ctx: 0,
                    entropy_ctx: u_entropy_ctx,
                    u_has_coeffs: false,
                    tx_size_sqr: 3,
                    lossless: false,
                },
                &mut cdf_ctx,
                update,
            );

            let u_cul = compute_cul_level(&sb.u_levels, sb.u_eob);
            for v in &mut above_u[sb_col * 8..(sb_col + 1) * 8] {
                *v = u_cul;
            }
            left_u.fill(u_cul);

            let v_above = &above_v[sb_col * 8..(sb_col + 1) * 8];
            let v_entropy_ctx = compute_entropy_ctx(v_above, &left_v);
            let v_coeffs = PlaneCoeffs {
                levels: &sb.v_levels,
                eob: sb.v_eob,
            };
            encode_plane_full(
                &mut ec,
                &v_coeffs,
                &PlaneEncodeCtx {
                    plane: 2,
                    txs_ctx_idx: 3,
                    bwl: 5,
                    height: 32,
                    num_coeffs: 1024,
                    dc_sign_ctx: 0,
                    entropy_ctx: v_entropy_ctx,
                    u_has_coeffs: sb.u_eob > 0,
                    tx_size_sqr: 3,
                    lossless: false,
                },
                &mut cdf_ctx,
                update,
            );

            let v_cul = compute_cul_level(&sb.v_levels, sb.v_eob);
            for v in &mut above_v[sb_col * 8..(sb_col + 1) * 8] {
                *v = v_cul;
            }
            left_v.fill(v_cul);
        }
    }

    ec.done()
}

use crate::encode::EncodedSuperblock;

fn do_split_ctx(above_part: &[u8], left_part: &[u8], bw_log2: usize, bh_log2: usize, bsize_group: usize) -> usize {
    let shift_w = if bw_log2 > 1 { bw_log2 - 1 } else { 0 };
    let shift_h = if bh_log2 > 1 { bh_log2 - 1 } else { 0 };
    let above_val = above_part.first().copied().unwrap_or(0);
    let left_val = left_part.first().copied().unwrap_or(0);
    let above_bit = (above_val >> shift_w) & 1;
    let left_bit = (left_val >> shift_h) & 1;
    bsize_group * 4 + (left_bit as usize) * 2 + (above_bit as usize)
}

fn rect_type_ctx(above_part: &[u8], left_part: &[u8], bw_log2: usize, bh_log2: usize, bsize_rect_group: usize) -> usize {
    let shift_w = if bw_log2 > 1 { bw_log2 - 1 } else { 0 };
    let shift_h = if bh_log2 > 1 { bh_log2 - 1 } else { 0 };
    let above_val = above_part.first().copied().unwrap_or(0);
    let left_val = left_part.first().copied().unwrap_or(0);
    let above_bit = (above_val >> shift_w) & 1;
    let left_bit = (left_val >> shift_h) & 1;
    bsize_rect_group * 4 + (left_bit as usize) * 2 + (above_bit as usize)
}

fn part_ctx_above(w: usize) -> u8 {
    match w {
        4 => 63, 8 => 62, 16 => 60, 32 => 56, 64 => 48, 128 => 32, _ => 0,
    }
}

fn part_ctx_left(h: usize) -> u8 {
    match h {
        4 => 63, 8 => 62, 16 => 60, 32 => 56, 64 => 48, 128 => 32, _ => 0,
    }
}

fn split_group(w: usize, h: usize) -> usize {
    if w <= 8 && h <= 8 { return 0; }
    let mi_area = (w / 4) * (h / 4);
    if mi_area <= 16 { return 1; }
    if mi_area <= 64 { return 2; }
    3
}

fn rect_split_group(w: usize, h: usize) -> usize {
    match (w, h) {
        (64, 64) => 6,
        (64, 32) => 5,
        (32, 64) => 4,
        (32, 32) => 3,
        (32, 16) | (16, 8) => 2,
        (16, 32) | (8, 16) => 1,
        _ => 0,
    }
}

fn mi_log2(pixels: usize) -> usize {
    match pixels / 4 {
        1 => 0, 2 => 1, 4 => 2, 8 => 3, 16 => 4, 32 => 5, _ => 0,
    }
}

struct PartCtx<'a> {
    above_part: &'a mut [u8],
    left_part: &'a mut [u8],
    above_y: &'a mut [u8],
    left_y: &'a mut [u8],
    above_u: &'a mut [u8],
    left_u: &'a mut [u8],
    above_v: &'a mut [u8],
    left_v: &'a mut [u8],
    above_mode: &'a mut [Option<usize>],
    left_mode: &'a mut [Option<usize>],
}

#[allow(clippy::too_many_arguments)]
const SKIP_CONTEXTS_Y: [[u8; 5]; 5] = [
    [1, 2, 2, 3, 3],
    [2, 2, 3, 3, 4],
    [2, 3, 3, 4, 4],
    [3, 3, 4, 4, 5],
    [3, 4, 4, 5, 5],
];

fn lossless_skip_ctx_y(above: u8, left: u8) -> usize {
    let top = (above & COEFF_CONTEXT_MASK).min(4) as usize;
    let left = (left & COEFF_CONTEXT_MASK).min(4) as usize;
    SKIP_CONTEXTS_Y[top][left] as usize
}

fn scan_4x4_levels(coeffs: &[i32; 16]) -> (Vec<i32>, u16) {
    let scan = &crate::scan::DEFAULT_SCAN_4X4;
    let mut levels = vec![0i32; 16];
    let mut eob = 0u16;
    for scan_idx in 0..16 {
        let coeff_idx = scan[scan_idx] as usize;
        levels[scan_idx] = coeffs[coeff_idx];
        if coeffs[coeff_idx] != 0 {
            eob = scan_idx as u16 + 1;
        }
    }
    (levels, eob)
}

fn encode_lossless_plane(
    ec: &mut DaalaCoder,
    plane: usize,
    residual: &[i32],
    plane_w: usize,
    plane_h: usize,
    base_above: usize,
    base_left: usize,
    above: &mut [u8],
    left: &mut [u8],
    v_u_has_coeffs: bool,
    cdf: &mut FrameCdfContext,
    update: bool,
) {
    let blocks_w = plane_w / 4;
    if blocks_w == 0 {
        return;
    }
    let blocks_h = plane_h / 4;
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let mut residual_4x4 = [0i32; 16];
            for y in 0..4 {
                for x in 0..4 {
                    residual_4x4[y * 4 + x] = residual[(by * 4 + y) * plane_w + bx * 4 + x];
                }
            }

            let coeffs = crate::dct::forward_wht4x4(&residual_4x4);
            let (levels, eob) = scan_4x4_levels(&coeffs);

            let mi_x = base_above + bx;
            let mi_y = base_left + by;
            let entropy_ctx = if plane == 0 {
                lossless_skip_ctx_y(above[mi_x], left[mi_y])
            } else {
                let base = compute_entropy_ctx(&above[mi_x..mi_x + 1], &left[mi_y..mi_y + 1]);
                if plane == 2 && plane_w * plane_h > 16 {
                    base + 3
                } else {
                    base
                }
            };
            let dc_sign_ctx = if plane == 0 {
                compute_dc_sign_ctx(&above[mi_x..mi_x + 1], &left[mi_y..mi_y + 1])
            } else {
                0
            };
            let u_has_coeffs = plane == 2 && v_u_has_coeffs;

            let coeffs_ref = PlaneCoeffs {
                levels: &levels,
                eob,
            };
            encode_plane_full(
                ec,
                &coeffs_ref,
                &PlaneEncodeCtx {
                    plane,
                    txs_ctx_idx: 0,
                    bwl: 2,
                    height: 4,
                    num_coeffs: 16,
                    dc_sign_ctx,
                    entropy_ctx,
                    u_has_coeffs,
                    tx_size_sqr: 0,
                    lossless: true,
                },
                cdf,
                update,
            );

            let cul = compute_cul_level_vec(&levels, eob);
            above[mi_x] = cul;
            left[mi_y] = cul;
        }
    }
}

fn encode_leaf_block(
    ec: &mut DaalaCoder,
    block: &crate::encode::EncodedBlock,
    abs_mi_col: usize,
    sb_mi_row: usize,
    bsize: usize,
    ctx: &mut PartCtx,
    sb_row: usize,
    sb_col: usize,
    lossless: bool,
    cdf: &mut FrameCdfContext,
    update: bool,
) {
    let mi_size = bsize / 4;
    let uv_bsize = bsize / 2;
    let uv_mi_size = uv_bsize / 4;
    let uv_abs_col = abs_mi_col / 2;
    let uv_sb_row = sb_mi_row / 2;
    let mi_col = abs_mi_col;
    let mi_row = sb_mi_row;

    let left_j = if mi_col > 0 || sb_col > 0 {
        ctx.left_mode[mi_row + mi_size - 1]
    } else {
        None
    };
    let above_j = if mi_row > 0 || sb_row > 0 {
        ctx.above_mode[mi_col + mi_size - 1]
    } else {
        None
    };

    let is_large = bsize * bsize > 64;
    let is_small = bsize < 8;
    let y_list = crate::encode::build_y_mode_list(left_j, above_j, is_large, is_small);
    let y_joint = block.mode.joint_y_index();
    let y_mode_idx = crate::encode::find_mode_idx_in_list(&y_list, y_joint);

    let uv_list = crate::encode::build_uv_mode_list(block.mode);
    let uv_mode_idx = crate::encode::find_uv_mode_idx(&uv_list, block.uv_mode);
    let uv_ctx = if block.mode.is_directional() { 1 } else { 0 };

    let is_left_dir = left_j.map(|j| j >= 5).unwrap_or(false);
    let is_above_dir = above_j.map(|j| j >= 5).unwrap_or(false);
    let y_mode_idx_ctx = is_left_dir as usize + is_above_dir as usize;

    encode_y_mode(ec, y_mode_idx, y_mode_idx_ctx, lossless, cdf, update);
    encode_uv_mode(ec, uv_mode_idx, uv_ctx, lossless, cdf, update);

    for i in 0..mi_size { ctx.above_mode[mi_col + i] = Some(y_joint); }
    for i in 0..mi_size { ctx.left_mode[mi_row + i] = Some(y_joint); }

    if lossless {
        encode_lossless_plane(
            ec,
            0,
            &block.y_residual,
            bsize,
            bsize,
            mi_col,
            mi_row,
            &mut ctx.above_y,
            &mut ctx.left_y,
            false,
            cdf,
            update,
        );

        let mut u_eobs = vec![0u16; (uv_bsize / 4) * (uv_bsize / 4)];
        encode_lossless_plane(
            ec,
            1,
            &block.u_residual,
            uv_bsize,
            uv_bsize,
            uv_abs_col,
            uv_sb_row,
            &mut ctx.above_u,
            &mut ctx.left_u,
            false,
            cdf,
            update,
        );
        let mut idx = 0;
        for by in 0..(uv_bsize / 4) {
            for bx in 0..(uv_bsize / 4) {
                let mut residual_4x4 = [0i32; 16];
                for y in 0..4 {
                    for x in 0..4 {
                        residual_4x4[y * 4 + x] =
                            block.u_residual[(by * 4 + y) * uv_bsize + bx * 4 + x];
                    }
                }
                let coeffs = crate::dct::forward_wht4x4(&residual_4x4);
                let (_, eob) = scan_4x4_levels(&coeffs);
                u_eobs[idx] = eob;
                idx += 1;
            }
        }
        let v_u_has_coeffs = u_eobs.last().map(|&eob| eob > 0).unwrap_or(false);
        encode_lossless_plane(
            ec,
            2,
            &block.v_residual,
            uv_bsize,
            uv_bsize,
            uv_abs_col,
            uv_sb_row,
            &mut ctx.above_v,
            &mut ctx.left_v,
            v_u_has_coeffs,
            cdf,
            update,
        );
        return;
    }

    let y_txs_ctx = txs_ctx_idx_for_bsize(bsize);
    let y_bwl = bwl_for_bsize(bsize);
    let y_h = height_for_bsize(bsize);
    let y_nc = num_coeffs_for_bsize(bsize);
    let y_tx_size_sqr = tx_size_sqr_for_bsize(bsize);

    let y_above_slice = &ctx.above_y[mi_col..mi_col + mi_size];
    let y_left_slice: Vec<u8> = ctx.left_y[mi_row..mi_row + mi_size].to_vec();
    let y_dc_sign_ctx = compute_dc_sign_ctx(y_above_slice, &y_left_slice);
    let y_entropy_ctx = 0;

    let y_coeffs = PlaneCoeffs { levels: &block.levels, eob: block.eob };
    encode_plane_full(ec, &y_coeffs, &PlaneEncodeCtx {
        plane: 0, txs_ctx_idx: y_txs_ctx, bwl: y_bwl, height: y_h, num_coeffs: y_nc,
        dc_sign_ctx: y_dc_sign_ctx, entropy_ctx: y_entropy_ctx, u_has_coeffs: false,
        tx_size_sqr: y_tx_size_sqr,
        lossless: false,
    }, cdf, update);

    let y_cul = compute_cul_level_vec(&block.levels, block.eob);
    for i in 0..mi_size { ctx.above_y[mi_col + i] = y_cul; }
    for i in 0..mi_size { ctx.left_y[mi_row + i] = y_cul; }

    let uv_txs_ctx = txs_ctx_idx_for_bsize(uv_bsize);
    let uv_bwl = bwl_for_bsize(uv_bsize);
    let uv_h = height_for_bsize(uv_bsize);
    let uv_nc = num_coeffs_for_bsize(uv_bsize);

    let u_above_slice = &ctx.above_u[uv_abs_col..uv_abs_col + uv_mi_size];
    let u_left_slice: Vec<u8> = ctx.left_u[uv_sb_row..uv_sb_row + uv_mi_size].to_vec();
    let u_entropy_ctx = compute_entropy_ctx(u_above_slice, &u_left_slice);

    let uv_tx_size_sqr = tx_size_sqr_for_bsize(uv_bsize);

    let u_coeffs = PlaneCoeffs { levels: &block.u_levels, eob: block.u_eob };
    encode_plane_full(ec, &u_coeffs, &PlaneEncodeCtx {
        plane: 1, txs_ctx_idx: uv_txs_ctx, bwl: uv_bwl, height: uv_h, num_coeffs: uv_nc,
        dc_sign_ctx: 0, entropy_ctx: u_entropy_ctx, u_has_coeffs: false,
        tx_size_sqr: uv_tx_size_sqr,
        lossless: false,
    }, cdf, update);

    let u_cul = compute_cul_level_vec(&block.u_levels, block.u_eob);
    for i in 0..uv_mi_size { ctx.above_u[uv_abs_col + i] = u_cul; }
    for i in 0..uv_mi_size { ctx.left_u[uv_sb_row + i] = u_cul; }

    let v_above_slice = &ctx.above_v[uv_abs_col..uv_abs_col + uv_mi_size];
    let v_left_slice: Vec<u8> = ctx.left_v[uv_sb_row..uv_sb_row + uv_mi_size].to_vec();
    let v_entropy_ctx = compute_entropy_ctx(v_above_slice, &v_left_slice);

    let v_coeffs = PlaneCoeffs { levels: &block.v_levels, eob: block.v_eob };
    encode_plane_full(ec, &v_coeffs, &PlaneEncodeCtx {
        plane: 2, txs_ctx_idx: uv_txs_ctx, bwl: uv_bwl, height: uv_h, num_coeffs: uv_nc,
        dc_sign_ctx: 0, entropy_ctx: v_entropy_ctx, u_has_coeffs: block.u_eob > 0,
        tx_size_sqr: uv_tx_size_sqr,
        lossless: false,
    }, cdf, update);

    let v_cul = compute_cul_level_vec(&block.v_levels, block.v_eob);
    for i in 0..uv_mi_size { ctx.above_v[uv_abs_col + i] = v_cul; }
    for i in 0..uv_mi_size { ctx.left_v[uv_sb_row + i] = v_cul; }
}

#[allow(clippy::too_many_arguments)]
fn encode_sq_part(
    ec: &mut DaalaCoder,
    blocks: &[crate::encode::EncodedBlock],
    x: usize,
    y: usize,
    bsize: usize,
    ctx: &mut PartCtx,
    sb_row: usize,
    sb_col: usize,
    lossless: bool,
    cdf: &mut FrameCdfContext,
    update: bool,
) {
    let mi_col = x / 4;
    let mi_row = (y % 64) / 4;
    let mi_size = bsize / 4;

    let is_leaf = blocks.iter().any(|b| b.x == x && b.y == y && b.bsize == bsize);

    if is_leaf {
        let bw_log2 = mi_log2(bsize);
        let group = split_group(bsize, bsize);
        let blk_above = ctx.above_part[mi_col..mi_col + mi_size].to_vec();
        let blk_left = ctx.left_part[mi_row..mi_row + mi_size].to_vec();
        let split_ctx = do_split_ctx(&blk_above, &blk_left, bw_log2, bw_log2, group);
        encode_sym(ec, 0, &mut cdf.do_split_part[0][split_ctx.min(63)], update);

        let block = blocks.iter().find(|b| b.x == x && b.y == y && b.bsize == bsize).unwrap();
        encode_leaf_block(ec, block, mi_col, mi_row, bsize, ctx, sb_row, sb_col, lossless, cdf, update);

        let pctx_a = part_ctx_above(bsize);
        let pctx_l = part_ctx_left(bsize);
        for i in 0..mi_size { ctx.above_part[mi_col + i] = pctx_a; }
        for i in 0..mi_size { ctx.left_part[mi_row + i] = pctx_l; }
    } else {
        assert!(bsize >= 16, "cannot split block smaller than 16x16");
        let half = bsize / 2;

        let bw_log2 = mi_log2(bsize);
        let group = split_group(bsize, bsize);
        let blk_above = ctx.above_part[mi_col..mi_col + mi_size].to_vec();
        let blk_left = ctx.left_part[mi_row..mi_row + mi_size].to_vec();

        let split_ctx = do_split_ctx(&blk_above, &blk_left, bw_log2, bw_log2, group);
        encode_sym(ec, 1, &mut cdf.do_split_part[0][split_ctx.min(63)], update);

        let rgroup = rect_split_group(bsize, bsize);
        let rctx = rect_type_ctx(&blk_above, &blk_left, bw_log2, bw_log2, rgroup);
        encode_sym(ec, 0, &mut cdf.rect_type[0][rctx.min(63)], update);

        encode_rect_vert(ec, blocks, x, y, bsize, half, ctx, sb_row, sb_col, lossless, cdf, update);
        encode_rect_vert(ec, blocks, x, y + half, bsize, half, ctx, sb_row, sb_col, lossless, cdf, update);
    }
}

#[allow(clippy::too_many_arguments)]
fn encode_rect_vert(
    ec: &mut DaalaCoder,
    blocks: &[crate::encode::EncodedBlock],
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    ctx: &mut PartCtx,
    sb_row: usize,
    sb_col: usize,
    lossless: bool,
    cdf: &mut FrameCdfContext,
    update: bool,
) {
    let mi_col = x / 4;
    let mi_row = (y % 64) / 4;
    let mi_w = w / 4;
    let mi_h = h / 4;
    let half_w = w / 2;

    let bw_log2 = mi_log2(w);
    let bh_log2 = mi_log2(h);
    let group = split_group(w, h);
    let rect_above = ctx.above_part[mi_col..mi_col + mi_w].to_vec();
    let rect_left = ctx.left_part[mi_row..mi_row + mi_h].to_vec();

    let split_ctx = do_split_ctx(&rect_above, &rect_left, bw_log2, bh_log2, group);
    encode_sym(ec, 1, &mut cdf.do_split_part[0][split_ctx.min(63)], update);

    let rgroup = rect_split_group(w, h);
    let rctx = rect_type_ctx(&rect_above, &rect_left, bw_log2, bh_log2, rgroup);
    encode_sym(ec, 1, &mut cdf.rect_type[0][rctx.min(63)], update);

    encode_sq_part(ec, blocks, x, y, half_w, ctx, sb_row, sb_col, lossless, cdf, update);
    encode_sq_part(ec, blocks, x + half_w, y, half_w, ctx, sb_row, sb_col, lossless, cdf, update);
}

pub fn encode_tile_data_split4(
    sb_cols: usize,
    sb_rows: usize,
    sbs: &[EncodedSuperblock],
    allow_update_cdf: bool,
    base_qindex: u8,
) -> Vec<u8> {
    let mut ec = DaalaCoder::new();
    let mut cdf = FrameCdfContext::new_with_qindex(base_qindex);
    let update = allow_update_cdf;
    let lossless = base_qindex == 0;

    let mi_cols = sb_cols * 16;
    let mut above_part = vec![0u8; mi_cols];
    let mut above_y = vec![0u8; mi_cols];
    let mut above_u = vec![0u8; mi_cols / 2];
    let mut above_v = vec![0u8; mi_cols / 2];
    let mut above_mode: Vec<Option<usize>> = vec![None; mi_cols];

    for sb_row in 0..sb_rows {
        let mut left_part = vec![0u8; 16];
        let mut left_y = vec![0u8; 16];
        let mut left_u = vec![0u8; 8];
        let mut left_v = vec![0u8; 8];
        let mut left_mode: Vec<Option<usize>> = vec![None; 16];

        for sb_col in 0..sb_cols {
            let sb = &sbs[sb_row * sb_cols + sb_col];
            let mi_col = sb_col * 16;

            let sb_above = above_part[mi_col..mi_col + 16].to_vec();
            let sb_left = left_part[..16].to_vec();

            let ctx_64 = do_split_ctx(&sb_above, &sb_left, 4, 4, 3);
            encode_sym(&mut ec, 1, &mut cdf.do_split_part[0][ctx_64.min(63)], update);

            let rctx_64 = rect_type_ctx(&sb_above, &sb_left, 4, 4, 6);
            encode_sym(&mut ec, 0, &mut cdf.rect_type[0][rctx_64.min(63)], update);

            for half_idx in 0..2u32 {
                let half_mi_row = half_idx as usize * 8;

                let half_above: Vec<u8> = above_part[mi_col..mi_col + 16].to_vec();
                let half_left: Vec<u8> = left_part[half_mi_row..half_mi_row + 8].to_vec();

                let ctx_rect = do_split_ctx(&half_above, &half_left, 4, 3, 3);
                encode_sym(&mut ec, 1, &mut cdf.do_split_part[0][ctx_rect.min(63)], update);

                let rctx_rect = rect_type_ctx(&half_above, &half_left, 4, 3, 5);
                encode_sym(&mut ec, 1, &mut cdf.rect_type[0][rctx_rect.min(63)], update);

                let sb_x = sb_col * 64;
                let sb_y = sb_row * 64;
                for sub_idx in 0..2u32 {
                    let bx = sb_x + sub_idx as usize * 32;
                    let by = sb_y + half_idx as usize * 32;

                    let blk_blocks: Vec<crate::encode::EncodedBlock> = sb.y_blocks.iter()
                        .filter(|b| b.x >= bx && b.x < bx + 32 && b.y >= by && b.y < by + 32)
                        .cloned()
                        .collect();

                    {
                        let mut pctx = PartCtx {
                            above_part: &mut above_part,
                            left_part: &mut left_part,
                            above_y: &mut above_y,
                            left_y: &mut left_y,
                            above_u: &mut above_u,
                            left_u: &mut left_u,
                            above_v: &mut above_v,
                            left_v: &mut left_v,
                            above_mode: &mut above_mode,
                            left_mode: &mut left_mode,
                        };
                        encode_sq_part(
                            &mut ec, &blk_blocks, bx, by, 32,
                            &mut pctx, sb_row, sb_col, lossless, &mut cdf, update,
                        );
                    }
                }

            }
        }
    }

    ec.done()
}

fn bwl_for_bsize(bsize: usize) -> u32 {
    match bsize {
        4 => 2, 8 => 3, 16 => 4, 32 | 64 => 5,
        _ => panic!("unsupported bsize {}", bsize),
    }
}

fn height_for_bsize(bsize: usize) -> usize {
    match bsize {
        4 => 4, 8 => 8, 16 => 16, 32 | 64 => 32,
        _ => panic!("unsupported bsize {}", bsize),
    }
}

fn txs_ctx_idx_for_bsize(bsize: usize) -> usize {
    match bsize {
        4 => 0, 8 => 1, 16 => 2, 32 => 3, 64 => 4,
        _ => panic!("unsupported bsize {}", bsize),
    }
}

fn num_coeffs_for_bsize(bsize: usize) -> usize {
    match bsize {
        4 => 16, 8 => 64, 16 => 256, 32 | 64 => 1024,
        _ => panic!("unsupported bsize {}", bsize),
    }
}

fn tx_size_sqr_for_bsize(bsize: usize) -> usize {
    match bsize {
        4 => 0, 8 => 1, 16 => 2, 32 | 64 => 3,
        _ => 3,
    }
}

enum PartitionNode {
    Leaf(usize),
    Split([Box<PartitionNode>; 4]),
}

fn build_partition_tree(blocks: &[crate::encode::EncodedBlock], sb_x: usize, sb_y: usize) -> PartitionNode {
    build_partition_subtree(blocks, sb_x, sb_y, 64)
}

fn build_partition_subtree(
    blocks: &[crate::encode::EncodedBlock],
    x: usize,
    y: usize,
    bsize: usize,
) -> PartitionNode {
    if let Some(idx) = blocks.iter().position(|b| b.x == x && b.y == y && b.bsize == bsize) {
        return PartitionNode::Leaf(idx);
    }
    let half = bsize / 2;
    PartitionNode::Split([
        Box::new(build_partition_subtree(blocks, x, y, half)),
        Box::new(build_partition_subtree(blocks, x + half, y, half)),
        Box::new(build_partition_subtree(blocks, x, y + half, half)),
        Box::new(build_partition_subtree(blocks, x + half, y + half, half)),
    ])
}

fn partition_split_ctx(above_part: &[u8], left_part: &[u8], bsize: usize) -> usize {
    let bsize_group = match bsize {
        64 => 3, 32 => 2, 16 => 1, _ => 0,
    };
    let shift = match bsize {
        64 => 3, 32 => 2, 16 => 1, _ => 0,
    };
    let above_val = above_part.first().copied().unwrap_or(0);
    let left_val = left_part.first().copied().unwrap_or(0);
    let above_bit = (above_val >> shift) & 1;
    let left_bit = (left_val >> shift) & 1;
    bsize_group * 4 + (left_bit as usize) * 2 + (above_bit as usize)
}

fn sq_split_ctx(above_part: &[u8], left_part: &[u8], bsize: usize) -> usize {
    let shift = match bsize { 64 => 3, 32 => 2, 16 => 1, _ => 0 };
    let above_val = above_part.first().copied().unwrap_or(0);
    let left_val = left_part.first().copied().unwrap_or(0);
    let above_bit = (above_val >> shift) & 1;
    let left_bit = (left_val >> shift) & 1;
    (left_bit as usize) * 2 + (above_bit as usize)
}

fn encode_partition_and_coeffs(
    ec: &mut DaalaCoder,
    node: &PartitionNode,
    blocks: &[crate::encode::EncodedBlock],
    bsize: usize,
    above_part: &[u8],
    left_part: &[u8],
    above_y: &[u8],
    left_y: &[u8; 16],
    above_u: &[u8],
    left_u: &[u8; 8],
    above_v: &[u8],
    left_v: &[u8; 8],
    cdf: &mut FrameCdfContext,
    update: bool,
) {
    match node {
        PartitionNode::Leaf(idx) => {
            let block = &blocks[*idx];
            let txs_ctx = txs_ctx_idx_for_bsize(bsize);
            let bwl = bwl_for_bsize(bsize);
            let h = height_for_bsize(bsize);
            let nc = num_coeffs_for_bsize(bsize);

            let uv_bsize = bsize / 2;
            let uv_txs_ctx = txs_ctx_idx_for_bsize(uv_bsize);
            let uv_bwl = bwl_for_bsize(uv_bsize);
            let uv_h = height_for_bsize(uv_bsize);
            let uv_nc = num_coeffs_for_bsize(uv_bsize);

            if bsize > 8 {
                let ctx = partition_split_ctx(above_part, left_part, bsize);
                encode_sym(ec, 0, &mut cdf.do_split_part[0][ctx.min(63)], update);
            }
            {
                let y_list = crate::encode::build_y_mode_list(None, None, true, false);
                let y_joint = block.mode.joint_y_index();
                let y_mode_idx = crate::encode::find_mode_idx_in_list(&y_list, y_joint);
                let uv_list = crate::encode::build_uv_mode_list(block.mode);
                let uv_mode_idx = crate::encode::find_uv_mode_idx(&uv_list, block.uv_mode);
                let uv_ctx = if block.mode.is_directional() { 1 } else { 0 };
                encode_y_mode(ec, y_mode_idx, 0, false, cdf, update);
                encode_uv_mode(ec, uv_mode_idx, uv_ctx, false, cdf, update);
            }

            let y_dc_sign_ctx = compute_dc_sign_ctx(above_y, left_y);
            let y_tx_sqr = tx_size_sqr_for_bsize(bsize);
            let coeffs = PlaneCoeffs { levels: &block.levels, eob: block.eob };
            encode_plane_full(ec, &coeffs, &PlaneEncodeCtx {
                plane: 0, txs_ctx_idx: txs_ctx, bwl, height: h, num_coeffs: nc,
                dc_sign_ctx: y_dc_sign_ctx, entropy_ctx: 0, u_has_coeffs: false,
                tx_size_sqr: y_tx_sqr, lossless: false,
            }, cdf, update);

            let uv_tx_sqr = tx_size_sqr_for_bsize(uv_bsize);
            let u_entropy_ctx = compute_entropy_ctx(above_u, left_u);
            let u_coeffs = PlaneCoeffs { levels: &block.u_levels, eob: block.u_eob };
            encode_plane_full(ec, &u_coeffs, &PlaneEncodeCtx {
                plane: 1, txs_ctx_idx: uv_txs_ctx, bwl: uv_bwl, height: uv_h, num_coeffs: uv_nc,
                dc_sign_ctx: 0, entropy_ctx: u_entropy_ctx, u_has_coeffs: false,
                tx_size_sqr: uv_tx_sqr, lossless: false,
            }, cdf, update);

            let v_entropy_ctx = compute_entropy_ctx(above_v, left_v);
            let v_coeffs = PlaneCoeffs { levels: &block.v_levels, eob: block.v_eob };
            encode_plane_full(ec, &v_coeffs, &PlaneEncodeCtx {
                plane: 2, txs_ctx_idx: uv_txs_ctx, bwl: uv_bwl, height: uv_h, num_coeffs: uv_nc,
                dc_sign_ctx: 0, entropy_ctx: v_entropy_ctx, u_has_coeffs: block.u_eob > 0,
                tx_size_sqr: uv_tx_sqr, lossless: false,
            }, cdf, update);
        }
        PartitionNode::Split(children) => {
            let ctx = partition_split_ctx(above_part, left_part, bsize);
            let sq_ctx = sq_split_ctx(above_part, left_part, bsize);
            let half = bsize / 2;
            let mi_half = half / 4;
            let empty = vec![0u8; mi_half.max(1)];

            encode_sym(ec, 1, &mut cdf.do_split_part[0][ctx.min(63)], update);
            encode_sym(ec, 1, &mut cdf.do_square_split[0][sq_ctx.min(7)], update);
            for (i, child) in children.iter().enumerate() {
                let child_above = if i < 2 { above_part } else { &empty };
                let child_left = if i % 2 == 0 { left_part } else { &empty };
                encode_partition_and_coeffs(ec, child, blocks, half, child_above, child_left, above_y, left_y, above_u, left_u, above_v, left_v, cdf, update);
            }
        }
    }
}

fn compute_cul_level_vec(levels: &[i32], eob: u16) -> u8 {
    if eob == 0 { return 0; }
    let sum: u32 = levels[..eob as usize].iter().map(|&l| l.unsigned_abs()).sum();
    let mut cul = sum.min(COEFF_CONTEXT_MASK as u32) as u8;
    let dc = levels[0];
    if dc < 0 {
        cul |= 1 << COEFF_CONTEXT_BITS;
    } else if dc > 0 {
        cul += 2 << COEFF_CONTEXT_BITS;
    }
    cul
}

pub fn encode_tile_data_partitioned(
    sb_cols: usize,
    sb_rows: usize,
    sbs: &[EncodedSuperblock],
    allow_update_cdf: bool,
    base_qindex: u8,
) -> Vec<u8> {
    let mut ec = DaalaCoder::new();
    let mut cdf_ctx = FrameCdfContext::new_with_qindex(base_qindex);
    let update = allow_update_cdf;

    let mi_cols = sb_cols * 16;
    let mut above_part = vec![0u8; mi_cols];
    let mut above_y = vec![0u8; sb_cols * 16];
    let mut above_u = vec![0u8; sb_cols * 8];
    let mut above_v = vec![0u8; sb_cols * 8];

    for sb_row in 0..sb_rows {
        let mut left_part = vec![0u8; 16];
        let mut left_y = [0u8; 16];
        let mut left_u = [0u8; 8];
        let mut left_v = [0u8; 8];

        for sb_col in 0..sb_cols {
            let sb = &sbs[sb_row * sb_cols + sb_col];
            let sb_x = sb_col * 64;
            let sb_y_pos = sb_row * 64;

            let tree = build_partition_tree(&sb.y_blocks, sb_x, sb_y_pos);
            let above_slice = &above_part[sb_col * 16..(sb_col + 1) * 16];
            let y_above = &above_y[sb_col * 16..(sb_col + 1) * 16];

            encode_partition_and_coeffs(
                &mut ec, &tree, &sb.y_blocks, 64,
                above_slice, &left_part,
                y_above, &left_y,
                &above_u[sb_col * 8..(sb_col + 1) * 8], &left_u,
                &above_v[sb_col * 8..(sb_col + 1) * 8], &left_v,
                &mut cdf_ctx,
                update,
            );

            let y_cul = {
                let total: u32 = sb.y_blocks.iter().map(|b| compute_cul_level_vec(&b.levels, b.eob) as u32).sum();
                (total / sb.y_blocks.len() as u32) as u8
            };
            for v in &mut above_y[sb_col * 16..(sb_col + 1) * 16] { *v = y_cul; }
            left_y.fill(y_cul);

            let part_val = if sb.y_blocks.len() == 1 { 0u8 } else {
                let min_bsize = sb.y_blocks.iter().map(|b| b.bsize).min().unwrap_or(64);
                match min_bsize { 32 => 1, 16 => 3, 8 => 7, _ => 0 }
            };
            for v in &mut above_part[sb_col * 16..(sb_col + 1) * 16] { *v = part_val; }
            left_part.fill(part_val);

            let u_cul = {
                let total: u32 = sb.y_blocks.iter().map(|b| compute_cul_level_vec(&b.u_levels, b.u_eob) as u32).sum();
                (total / sb.y_blocks.len() as u32) as u8
            };
            for v in &mut above_u[sb_col * 8..(sb_col + 1) * 8] { *v = u_cul; }
            left_u.fill(u_cul);

            let v_cul = {
                let total: u32 = sb.y_blocks.iter().map(|b| compute_cul_level_vec(&b.v_levels, b.v_eob) as u32).sum();
                (total / sb.y_blocks.len() as u32) as u8
            };
            for v in &mut above_v[sb_col * 8..(sb_col + 1) * 8] { *v = v_cul; }
            left_v.fill(v_cul);
        }
    }

    ec.done()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{EncodedBlock, EncodedSuperblock, IntraMode};

    #[test]
    fn partitioned_matches_flat_sb_for_none() {
        let mut flat_sb = SbCoeffs {
            y_levels: [0i32; 1024],
            y_eob: 0,
            u_levels: [0i32; 1024],
            u_eob: 0,
            v_levels: [0i32; 1024],
            v_eob: 0,
            y_mode: 0,
            uv_mode: 0,
        };
        flat_sb.y_levels[0] = 100;
        flat_sb.y_eob = 1;

        let flat_sb_data = encode_tile_data_multi_sb(1, 1, &[flat_sb], false, 50);

        let partitioned_sb = EncodedSuperblock {
            y_blocks: vec![EncodedBlock {
                x: 0, y: 0, bsize: 64,
                mode: IntraMode::DcPred,
                levels: {
                    let mut v = vec![0i32; 1024];
                    v[0] = 100;
                    v
                },
                eob: 1,
                log_scale: 2,
                uv_mode: IntraMode::DcPred,
                u_levels: vec![0i32; 1024],
                u_eob: 0,
                v_levels: vec![0i32; 1024],
                v_eob: 0,
                y_residual: Vec::new(),
                u_residual: Vec::new(),
                v_residual: Vec::new(),
            }],
            u_levels: [0i32; 1024],
            u_eob: 0,
            v_levels: [0i32; 1024],
            v_eob: 0,
            uv_mode: IntraMode::DcPred,
        };

        let partitioned_data = encode_tile_data_partitioned(1, 1, &[partitioned_sb], false, 50);

        assert_eq!(
            flat_sb_data, partitioned_data,
            "PART_NONE should match flat-SB encoding\nflat-SB len={} partitioned len={}\nflat-SB: {:02x?}\npartitioned: {:02x?}",
            flat_sb_data.len(), partitioned_data.len(),
            &flat_sb_data[..flat_sb_data.len().min(20)],
            &partitioned_data[..partitioned_data.len().min(20)],
        );
    }

    #[test]
    fn partitioned_matches_flat_sb_with_uv() {
        let mut flat_sb = SbCoeffs {
            y_levels: [0i32; 1024],
            y_eob: 0,
            u_levels: [0i32; 1024],
            u_eob: 0,
            v_levels: [0i32; 1024],
            v_eob: 0,
            y_mode: 0,
            uv_mode: 0,
        };
        flat_sb.y_levels[0] = 100;
        flat_sb.y_eob = 1;
        flat_sb.u_levels[0] = 50;
        flat_sb.u_eob = 1;
        flat_sb.v_levels[0] = -30;
        flat_sb.v_eob = 1;

        let flat_sb_data = encode_tile_data_multi_sb(1, 1, &[flat_sb], false, 50);

        let partitioned_sb = EncodedSuperblock {
            y_blocks: vec![EncodedBlock {
                x: 0, y: 0, bsize: 64,
                mode: IntraMode::DcPred,
                levels: {
                    let mut v = vec![0i32; 1024];
                    v[0] = 100;
                    v
                },
                eob: 1,
                log_scale: 2,
                uv_mode: IntraMode::DcPred,
                u_levels: {
                    let mut v = vec![0i32; 1024];
                    v[0] = 50;
                    v
                },
                u_eob: 1,
                v_levels: {
                    let mut v = vec![0i32; 1024];
                    v[0] = -30;
                    v
                },
                v_eob: 1,
                y_residual: Vec::new(),
                u_residual: Vec::new(),
                v_residual: Vec::new(),
            }],
            u_levels: { let mut l = [0i32; 1024]; l[0] = 50; l },
            u_eob: 1,
            v_levels: { let mut l = [0i32; 1024]; l[0] = -30; l },
            v_eob: 1,
            uv_mode: IntraMode::DcPred,
        };

        let partitioned_data = encode_tile_data_partitioned(1, 1, &[partitioned_sb], false, 50);

        assert_eq!(
            flat_sb_data, partitioned_data,
            "PART_NONE with UV should match flat-SB encoding\nflat-SB len={} partitioned len={}\nflat-SB: {:02x?}\npartitioned: {:02x?}",
            flat_sb_data.len(), partitioned_data.len(),
            &flat_sb_data[..flat_sb_data.len().min(20)],
            &partitioned_data[..partitioned_data.len().min(20)],
        );
    }

    #[test]
    fn tile_data_is_nonempty() {
        let data = encode_tile_data();
        assert!(!data.is_empty());
    }

    #[test]
    fn tile_data_reasonable_size() {
        let data = encode_tile_data();
        assert!(
            data.len() <= 20,
            "tile data too large: {} bytes",
            data.len()
        );
    }

    #[test]
    fn tile_data_with_dc_coefficient() {
        let data = encode_tile_data_with_dc(5, 0, 0);
        assert!(!data.is_empty());
        let zero_data = encode_tile_data_with_dc(0, 0, 0);
        assert_ne!(data, zero_data);
    }

    #[test]
    fn tile_data_dc_zero_matches_original() {
        let original = encode_tile_data();
        let with_dc = encode_tile_data_with_dc(0, 0, 0);
        assert_eq!(original, with_dc);
    }

    #[test]
    fn tile_data_negative_dc() {
        let pos = encode_tile_data_with_dc(5, 0, 0);
        let neg = encode_tile_data_with_dc(-5, 0, 0);
        assert_ne!(pos, neg);
    }

    #[test]
    fn tile_data_with_ac_coefficients() {
        let mut levels = [0i32; 1024];
        levels[0] = 100;
        levels[1] = -20;
        levels[2] = 10;
        let data =
            encode_tile_data_with_coeffs(&levels, 3, &[0i32; 1024], 0, &[0i32; 1024], 0, 0, 0);
        assert!(!data.is_empty());
    }

    #[test]
    fn tile_data_eob_1_produces_data() {
        let mut levels = [0i32; 1024];
        levels[0] = 50;
        let data =
            encode_tile_data_with_coeffs(&levels, 1, &[0i32; 1024], 0, &[0i32; 1024], 0, 0, 0);
        assert!(!data.is_empty());
    }

    #[test]
    fn context_padded_idx_dc() {
        assert_eq!(get_padded_idx(0, 5), 0);
    }

    #[test]
    fn context_padded_idx_second_row() {
        assert_eq!(get_padded_idx(32, 5), 32 + 4);
    }

    #[test]
    fn context_eob_scan_0() {
        assert_eq!(get_lower_levels_ctx_eob(5, 32, 0), 0);
    }

    #[test]
    fn is_lf_dc_is_lf() {
        assert!(is_lf_2d(0, 0, 0));
        assert!(is_lf_2d(0, 0, 1));
    }

    #[test]
    fn compute_cul_level_zero() {
        let levels = [0i32; 1024];
        assert_eq!(compute_cul_level(&levels, 0), 0);
    }

    #[test]
    fn compute_cul_level_positive_dc() {
        let mut levels = [0i32; 1024];
        levels[0] = 5;
        let cul = compute_cul_level(&levels, 1);
        assert_eq!(cul & COEFF_CONTEXT_MASK, 5);
        assert_eq!(cul >> COEFF_CONTEXT_BITS, 2);
    }

    #[test]
    fn compute_cul_level_negative_dc() {
        let mut levels = [0i32; 1024];
        levels[0] = -3;
        let cul = compute_cul_level(&levels, 1);
        assert_eq!(cul & COEFF_CONTEXT_MASK, 3);
        assert_eq!(cul >> COEFF_CONTEXT_BITS, 1);
    }

    #[test]
    fn compute_cul_level_clamped() {
        let mut levels = [0i32; 1024];
        levels[0] = 100;
        let cul = compute_cul_level(&levels, 1);
        assert_eq!(cul & COEFF_CONTEXT_MASK, 7);
    }

    #[test]
    fn entropy_ctx_both_zero() {
        assert_eq!(compute_entropy_ctx(&[0, 0, 0, 0], &[0, 0, 0, 0]), 0);
    }

    #[test]
    fn entropy_ctx_above_nonzero() {
        assert_eq!(compute_entropy_ctx(&[1, 0, 0, 0], &[0, 0, 0, 0]), 1);
    }

    #[test]
    fn entropy_ctx_both_nonzero() {
        assert_eq!(compute_entropy_ctx(&[1, 0, 0, 0], &[0, 0, 1, 0]), 2);
    }

    #[test]
    fn dc_sign_ctx_all_zero() {
        assert_eq!(compute_dc_sign_ctx(&[0; 16], &[0; 16]), 0);
    }

    #[test]
    fn dc_sign_ctx_positive() {
        let mut above = [0u8; 16];
        above.fill(2 << COEFF_CONTEXT_BITS | 1);
        assert_eq!(compute_dc_sign_ctx(&above, &[0; 16]), 2);
    }

    #[test]
    fn dc_sign_ctx_negative() {
        let mut left = [0u8; 16];
        left.fill(1 << COEFF_CONTEXT_BITS | 1);
        assert_eq!(compute_dc_sign_ctx(&[0; 16], &left), 1);
    }

    #[test]
    fn multi_sb_1x1_matches_single_sb() {
        let mut sb = SbCoeffs {
            y_levels: [0i32; 1024],
            y_eob: 0,
            u_levels: [0i32; 1024],
            u_eob: 0,
            v_levels: [0i32; 1024],
            v_eob: 0,
            y_mode: 0,
            uv_mode: 0,
        };
        sb.y_levels[0] = 50;
        sb.y_eob = 1;
        sb.u_levels[0] = -10;
        sb.u_eob = 1;

        let single = encode_tile_data_with_coeffs(
            &sb.y_levels,
            sb.y_eob,
            &sb.u_levels,
            sb.u_eob,
            &sb.v_levels,
            sb.v_eob,
            0,
            0,
        );
        let multi = encode_tile_data_multi_sb(1, 1, &[sb], false, 50);
        assert_eq!(
            single, multi,
            "1x1 multi-SB should match single-SB encoding"
        );
    }

    #[test]
    fn multi_sb_2x2_nonempty() {
        let sbs: Vec<SbCoeffs> = (0..4)
            .map(|_| SbCoeffs {
                y_levels: [0i32; 1024],
                y_eob: 0,
                u_levels: [0i32; 1024],
                u_eob: 0,
                v_levels: [0i32; 1024],
                v_eob: 0,
                y_mode: 0,
                uv_mode: 0,
            })
            .collect();
        let data = encode_tile_data_multi_sb(2, 2, &sbs, false, 50);
        assert!(!data.is_empty());
    }

    #[test]
    fn multi_sb_2x2_with_coeffs() {
        let sbs: Vec<SbCoeffs> = (0..4)
            .map(|i| {
                let mut sb = SbCoeffs {
                    y_levels: [0i32; 1024],
                    y_eob: 0,
                    u_levels: [0i32; 1024],
                    u_eob: 0,
                    v_levels: [0i32; 1024],
                    v_eob: 0,
                    y_mode: 0,
                    uv_mode: 0,
                };
                sb.y_levels[0] = (i + 1) * 10;
                sb.y_eob = 1;
                sb.u_levels[0] = (i + 1) * 5;
                sb.u_eob = 1;
                sb.v_levels[0] = (i + 1) * 3;
                sb.v_eob = 1;
                sb
            })
            .collect();
        let data = encode_tile_data_multi_sb(2, 2, &sbs, false, 50);
        assert!(!data.is_empty());
    }
}
