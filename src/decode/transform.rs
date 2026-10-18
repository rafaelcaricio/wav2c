use crate::block::TxSize;
use crate::dct;
use crate::scan::{DEFAULT_SCAN_32X32, TxClass, get_scan_for_tx, get_tx_class};

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

fn tcq_quant(state: u8) -> i32 {
    ((state & 2) != 0) as i32
}

fn tcq_next(state: u8, level: i32) -> u8 {
    let parity = (level.unsigned_abs() & 1) as usize;
    let s = if state >= 8 { 0 } else { state as usize };
    TCQ_NEXT_STATE[s][parity]
}

pub fn dequantize_coeff(level: i32, log_scale: u32, dequant: i32) -> i32 {
    let abs_level = level.unsigned_abs() as i64;
    let dq = dequant as i64;
    let dqcoeff = ((abs_level * dq + 4) >> 3) >> log_scale;
    if level < 0 {
        -(dqcoeff as i32)
    } else {
        dqcoeff as i32
    }
}

fn dequantize_coeff_tcq(level: i32, log_scale: u32, dequant: i32, qx: i32) -> i32 {
    let abs_level = level.unsigned_abs() as i64;
    let q_idx = (abs_level * 2 - qx as i64).max(0);
    let dq = dequant as i64;
    let dq_coeff_hp = (q_idx * dq) & 0xffffff;
    let dqcoeff = ((dq_coeff_hp + 4) >> 3) >> (log_scale + 1);
    if level < 0 {
        -(dqcoeff as i32)
    } else {
        dqcoeff as i32
    }
}

pub fn dequantize_block(
    levels: &[i32; 1024],
    eob: u16,
    dequant: i32,
    log_scale: u32,
) -> [i32; 1024] {
    let mut coeffs = [0i32; 1024];
    for scan_idx in 0..(eob as usize) {
        let coeff_idx = DEFAULT_SCAN_32X32[scan_idx] as usize;
        coeffs[coeff_idx] = dequantize_coeff(levels[scan_idx], log_scale, dequant);
    }
    coeffs
}

pub fn dequantize_and_inverse(
    levels: &[i32; 1024],
    eob: u16,
    dequant: i32,
    log_scale: u32,
    is_64x64: bool,
) -> Vec<i32> {
    if eob == 0 {
        if is_64x64 {
            return vec![0i32; 4096];
        } else {
            return vec![0i32; 1024];
        }
    }

    let coeffs = dequantize_block(levels, eob, dequant, log_scale);

    if is_64x64 {
        let residuals = dct::inverse_dct2_64x64_av2(&coeffs);
        residuals.to_vec()
    } else {
        let residuals = dct::inverse_dct2_32x32(&coeffs);
        residuals.to_vec()
    }
}

fn inverse_wht_1d(src: [i32; 4], shift: u32) -> [i32; 4] {
    let mut a = src[0] >> shift;
    let mut c = src[1] >> shift;
    let mut d = src[2] >> shift;
    let mut b = src[3] >> shift;

    a += c;
    d -= b;
    let e = (a - d) >> 1;
    b = e - b;
    c = e - c;
    a -= b;
    d += c;

    [a, b, c, d]
}

pub fn lossless_residual(levels: &[i32], eob: u16) -> Vec<i32> {
    let mut coeffs = [0i32; 16];
    let scan = get_scan_for_tx(TxSize::Tx4x4, 0);
    for scan_idx in 0..(eob as usize) {
        let coeff_idx = scan[scan_idx] as usize;
        coeffs[coeff_idx] = levels[scan_idx] * 8;
    }

    let mut rows = [0i32; 16];
    for row in 0..4 {
        let src = [
            coeffs[row * 4],
            coeffs[row * 4 + 1],
            coeffs[row * 4 + 2],
            coeffs[row * 4 + 3],
        ];
        let out = inverse_wht_1d(src, 3);
        rows[row * 4..row * 4 + 4].copy_from_slice(&out);
    }

    let mut residual = [0i32; 16];
    for col in 0..4 {
        let src = [rows[col], rows[4 + col], rows[8 + col], rows[12 + col]];
        let out = inverse_wht_1d(src, 0);
        for row in 0..4 {
            residual[row * 4 + col] = out[row];
        }
    }

    residual.to_vec()
}

pub fn dequantize_and_inverse_var(
    levels: &[i32],
    eob: u16,
    dequant: i32,
    tx_size: TxSize,
) -> Vec<i32> {
    dequantize_and_inverse_var_typed(levels, eob, dequant, tx_size, 0)
}

pub fn dequantize_and_inverse_var_typed(
    levels: &[i32],
    eob: u16,
    dequant: i32,
    tx_size: TxSize,
    tx_type: usize,
) -> Vec<i32> {
    dequantize_and_inverse_tcq(levels, eob, dequant, tx_size, tx_type, false)
}

pub fn dequantize_and_inverse_tcq(
    levels: &[i32],
    eob: u16,
    dequant: i32,
    tx_size: TxSize,
    tx_type: usize,
    tcq_enabled: bool,
) -> Vec<i32> {
    dequantize_and_inverse_tcq_qm(levels, eob, dequant, tx_size, tx_type, tcq_enabled, None)
}

pub fn dequantize_and_inverse_tcq_qm(
    levels: &[i32],
    eob: u16,
    dequant: i32,
    tx_size: TxSize,
    tx_type: usize,
    tcq_enabled: bool,
    qm: Option<&[u8]>,
) -> Vec<i32> {
    let pixel_count = tx_size.width() * tx_size.height();

    if eob == 0 {
        return vec![0i32; pixel_count];
    }

    let tx_class = get_tx_class(tx_type);
    let use_tcq = tcq_enabled && tx_class == TxClass::TwoD;

    let num_coeffs = tx_size.num_coeffs();
    let log_scale = tx_size.log_scale();
    let scan = get_scan_for_tx(tx_size, tx_type);

    let mut dqcoeffs = vec![0i32; num_coeffs];
    if use_tcq {
        let mut state: u8 = 0;
        for c in (0..(eob as usize)).rev() {
            let coeff_idx = scan[c] as usize;
            let qx = tcq_quant(state);
            let dqv = qm.map_or(dequant, |m| {
                ((m[coeff_idx] as i32 * dequant + 16) >> 5)
            });
            dqcoeffs[coeff_idx] = dequantize_coeff_tcq(levels[c], log_scale, dqv, qx);
            state = tcq_next(state, levels[c]);
        }
    } else {
        for scan_idx in 0..(eob as usize) {
            let coeff_idx = scan[scan_idx] as usize;
            let dqv = qm.map_or(dequant, |m| {
                ((m[coeff_idx] as i32 * dequant + 16) >> 5)
            });
            dqcoeffs[coeff_idx] = dequantize_coeff(levels[scan_idx], log_scale, dqv);
        }
    }

    if tx_type == 0 {
        match tx_size {
            TxSize::Tx4x4 => {
                let mut arr = [0i32; 16];
                arr.copy_from_slice(&dqcoeffs);
                return dct::inverse_dct2_4x4(&arr).to_vec();
            }
            TxSize::Tx8x8 => {
                let mut arr = [0i32; 64];
                arr.copy_from_slice(&dqcoeffs);
                return dct::inverse_dct2_8x8(&arr).to_vec();
            }
            TxSize::Tx16x16 => {
                let mut arr = [0i32; 256];
                arr.copy_from_slice(&dqcoeffs);
                return dct::inverse_dct2_16x16(&arr).to_vec();
            }
            TxSize::Tx32x32 => {
                let mut arr = [0i32; 1024];
                arr.copy_from_slice(&dqcoeffs);
                return dct::inverse_dct2_32x32(&arr).to_vec();
            }
            TxSize::Tx64x64 => {
                let mut arr = [0i32; 1024];
                arr.copy_from_slice(&dqcoeffs);
                return dct::inverse_dct2_64x64_av2(&arr).to_vec();
            }
            _ => {}
        }
    }

    let (shift1, shift2) = tx_size.inv_shifts();
    dct::inverse_transform_2d_rect(
        &dqcoeffs,
        tx_size.width(),
        tx_size.height(),
        tx_size.coded_width(),
        tx_size.coded_height(),
        tx_type,
        shift1,
        shift2,
        tx_size.needs_sqrt2(),
    )
}

const CCTX_MTX: [[i32; 2]; 6] = [
    [181, 181],
    [222, 128],
    [128, 222],
    [181, -181],
    [222, -128],
    [128, -222],
];

const CCTX_PREC_BITS: u32 = 8;

fn round_shift_signed_64(value: i64, n: u32) -> i64 {
    if value < 0 {
        -((-value + (1i64 << (n - 1))) >> n)
    } else {
        (value + (1i64 << (n - 1))) >> n
    }
}

pub fn dequantize_only(
    levels: &[i32],
    eob: u16,
    dequant: i32,
    tx_size: TxSize,
    tx_type: usize,
    tcq_enabled: bool,
) -> Vec<i32> {
    dequantize_only_qm(levels, eob, dequant, tx_size, tx_type, tcq_enabled, None)
}

pub fn dequantize_only_qm(
    levels: &[i32],
    eob: u16,
    dequant: i32,
    tx_size: TxSize,
    tx_type: usize,
    tcq_enabled: bool,
    qm: Option<&[u8]>,
) -> Vec<i32> {
    let num_coeffs = tx_size.num_coeffs();
    if eob == 0 {
        return vec![0i32; num_coeffs];
    }

    let tx_class = get_tx_class(tx_type);
    let use_tcq = tcq_enabled && tx_class == TxClass::TwoD;
    let log_scale = tx_size.log_scale();
    let scan = get_scan_for_tx(tx_size, tx_type);

    let mut dqcoeffs = vec![0i32; num_coeffs];
    if use_tcq {
        let mut state: u8 = 0;
        for c in (0..(eob as usize)).rev() {
            let coeff_idx = scan[c] as usize;
            let qx = tcq_quant(state);
            let dqv = qm.map_or(dequant, |m| {
                ((m[coeff_idx] as i32 * dequant + 16) >> 5)
            });
            dqcoeffs[coeff_idx] = dequantize_coeff_tcq(levels[c], log_scale, dqv, qx);
            state = tcq_next(state, levels[c]);
        }
    } else {
        for scan_idx in 0..(eob as usize) {
            let coeff_idx = scan[scan_idx] as usize;
            let dqv = qm.map_or(dequant, |m| {
                ((m[coeff_idx] as i32 * dequant + 16) >> 5)
            });
            dqcoeffs[coeff_idx] = dequantize_coeff(levels[scan_idx], log_scale, dqv);
        }
    }
    dqcoeffs
}

pub fn inverse_transform_only(dqcoeffs: &[i32], tx_size: TxSize, tx_type: usize) -> Vec<i32> {
    let pixel_count = tx_size.width() * tx_size.height();
    if dqcoeffs.iter().all(|&c| c == 0) {
        return vec![0i32; pixel_count];
    }

    if tx_type == 0 {
        match tx_size {
            TxSize::Tx4x4 => {
                let mut arr = [0i32; 16];
                arr.copy_from_slice(dqcoeffs);
                return dct::inverse_dct2_4x4(&arr).to_vec();
            }
            TxSize::Tx8x8 => {
                let mut arr = [0i32; 64];
                arr.copy_from_slice(dqcoeffs);
                return dct::inverse_dct2_8x8(&arr).to_vec();
            }
            TxSize::Tx16x16 => {
                let mut arr = [0i32; 256];
                arr.copy_from_slice(dqcoeffs);
                return dct::inverse_dct2_16x16(&arr).to_vec();
            }
            TxSize::Tx32x32 => {
                let mut arr = [0i32; 1024];
                arr.copy_from_slice(dqcoeffs);
                return dct::inverse_dct2_32x32(&arr).to_vec();
            }
            TxSize::Tx64x64 => {
                let mut arr = [0i32; 1024];
                arr.copy_from_slice(dqcoeffs);
                return dct::inverse_dct2_64x64_av2(&arr).to_vec();
            }
            _ => {}
        }
    }

    let (shift1, shift2) = tx_size.inv_shifts();
    dct::inverse_transform_2d_rect(
        dqcoeffs,
        tx_size.width(),
        tx_size.height(),
        tx_size.coded_width(),
        tx_size.coded_height(),
        tx_type,
        shift1,
        shift2,
        tx_size.needs_sqrt2(),
    )
}

pub fn inverse_cctx(u_dqcoeffs: &mut [i32], v_dqcoeffs: &mut [i32], cctx_type: u8) {
    if cctx_type == 0 {
        return;
    }
    let idx = (cctx_type - 1) as usize;
    let c = CCTX_MTX[idx][0] as i64;
    let s = CCTX_MTX[idx][1] as i64;
    let n = u_dqcoeffs.len().min(v_dqcoeffs.len());
    for i in 0..n {
        let tmp_u = u_dqcoeffs[i] as i64;
        let tmp_v = v_dqcoeffs[i] as i64;
        let new_u = round_shift_signed_64(c * tmp_u - s * tmp_v, CCTX_PREC_BITS);
        let new_v = round_shift_signed_64(s * tmp_u + c * tmp_v, CCTX_PREC_BITS);
        u_dqcoeffs[i] = (new_u as i32).clamp(-32768, 32767);
        v_dqcoeffs[i] = (new_v as i32).clamp(-32768, 32767);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dct;
    use crate::encode::{ac_qlookup, compute_residual_block, quantize_block};

    #[test]
    fn dequantize_dc_roundtrip() {
        let dqcoeff = dequantize_coeff(524, 2, 164);
        let expected = ((524i64 * 164 + 4) >> 3) >> 2;
        assert_eq!(dqcoeff as i64, expected);
    }

    #[test]
    fn dequantize_zero() {
        assert_eq!(dequantize_coeff(0, 2, 164), 0);
        assert_eq!(dequantize_coeff(0, 1, 164), 0);
        assert_eq!(dequantize_coeff(0, 0, 100), 0);
    }

    #[test]
    fn dequantize_negative() {
        let pos = dequantize_coeff(524, 2, 164);
        let neg = dequantize_coeff(-524, 2, 164);
        assert!(pos > 0);
        assert_eq!(neg, -pos);
    }

    #[test]
    fn dequantize_block_solid_32x32() {
        let pixel_val = 200u8;
        let pixels = vec![pixel_val; 32 * 32];
        let pred_block = vec![128u8; 32 * 32];
        let residual = compute_residual_block(&pixels, &pred_block);
        let mut residual_arr = [0i32; 1024];
        residual_arr.copy_from_slice(&residual);
        let dct_coeffs = dct::forward_dct2_32x32(&residual_arr);
        let dequant = ac_qlookup(50);
        let (levels, eob) = quantize_block(&dct_coeffs, dequant, 1);

        let reconstructed = dequantize_and_inverse(&levels, eob, dequant, 1, false);
        assert_eq!(reconstructed.len(), 1024);

        let expected_residual = pixel_val as i32 - 128;
        for (i, &r) in reconstructed.iter().enumerate() {
            let reconstructed_pixel = (r + 128).clamp(0, 255);
            assert!(
                (reconstructed_pixel - pixel_val as i32).abs() <= 1,
                "pixel {} = {}, expected ~{}",
                i,
                reconstructed_pixel,
                pixel_val
            );
            let _ = expected_residual;
        }
    }

    #[test]
    fn dequantize_block_solid_64x64() {
        let pixel_val = 149u8;
        let pixels = vec![pixel_val; 64 * 64];
        let pred_block = vec![128u8; 64 * 64];
        let residual = compute_residual_block(&pixels, &pred_block);
        let mut residual_arr = [0i32; 4096];
        residual_arr.copy_from_slice(&residual);
        let dct_coeffs = dct::forward_dct2_64x64(&residual_arr);
        let dequant = ac_qlookup(50);
        let (levels, eob) = quantize_block(&dct_coeffs, dequant, 2);

        let reconstructed = dequantize_and_inverse(&levels, eob, dequant, 2, true);
        assert_eq!(reconstructed.len(), 4096);

        for (i, &r) in reconstructed.iter().enumerate() {
            let reconstructed_pixel = (r + 128).clamp(0, 255);
            assert!(
                (reconstructed_pixel - pixel_val as i32).abs() <= 1,
                "pixel {} = {}, expected ~{}",
                i,
                reconstructed_pixel,
                pixel_val
            );
        }
    }

    #[test]
    fn dequantize_block_zero_eob() {
        let levels = [0i32; 1024];
        let residuals_32 = dequantize_and_inverse(&levels, 0, 164, 1, false);
        assert_eq!(residuals_32.len(), 1024);
        assert!(residuals_32.iter().all(|&r| r == 0));

        let residuals_64 = dequantize_and_inverse(&levels, 0, 164, 2, true);
        assert_eq!(residuals_64.len(), 4096);
        assert!(residuals_64.iter().all(|&r| r == 0));
    }

    #[test]
    fn var_4x4_zero_eob() {
        let levels = vec![0i32; 16];
        let result = dequantize_and_inverse_var(&levels, 0, 164, TxSize::Tx4x4);
        assert_eq!(result.len(), 16);
        assert!(result.iter().all(|&v| v == 0));
    }

    #[test]
    fn var_8x8_zero_eob() {
        let levels = vec![0i32; 64];
        let result = dequantize_and_inverse_var(&levels, 0, 164, TxSize::Tx8x8);
        assert_eq!(result.len(), 64);
        assert!(result.iter().all(|&v| v == 0));
    }

    #[test]
    fn var_16x16_zero_eob() {
        let levels = vec![0i32; 256];
        let result = dequantize_and_inverse_var(&levels, 0, 164, TxSize::Tx16x16);
        assert_eq!(result.len(), 256);
        assert!(result.iter().all(|&v| v == 0));
    }

    #[test]
    fn var_32x32_matches_fixed_size_path() {
        let mut levels_arr = [0i32; 1024];
        levels_arr[0] = 100;
        let fixed_size = dequantize_and_inverse(&levels_arr, 1, 164, 1, false);
        let levels_vec: Vec<i32> = levels_arr.to_vec();
        let variable_size = dequantize_and_inverse_var(&levels_vec, 1, 164, TxSize::Tx32x32);
        assert_eq!(fixed_size.len(), variable_size.len());
        for (i, (&a, &b)) in fixed_size.iter().zip(variable_size.iter()).enumerate() {
            assert_eq!(a, b, "mismatch at {}", i);
        }
    }

    #[test]
    fn var_64x64_matches_fixed_size_path() {
        let mut levels_arr = [0i32; 1024];
        levels_arr[0] = 500;
        let fixed_size = dequantize_and_inverse(&levels_arr, 1, 164, 2, true);
        let levels_vec: Vec<i32> = levels_arr.to_vec();
        let variable_size = dequantize_and_inverse_var(&levels_vec, 1, 164, TxSize::Tx64x64);
        assert_eq!(fixed_size.len(), variable_size.len());
        for (i, (&a, &b)) in fixed_size.iter().zip(variable_size.iter()).enumerate() {
            assert_eq!(a, b, "mismatch at {}", i);
        }
    }

    #[test]
    fn var_32x32_multi_coeff_matches_fixed_size_path() {
        let mut levels_arr = [0i32; 1024];
        levels_arr[0] = 100;
        levels_arr[1] = -50;
        levels_arr[2] = 30;
        levels_arr[3] = -10;
        let fixed_size = dequantize_and_inverse(&levels_arr, 4, 164, 1, false);
        let levels_vec: Vec<i32> = levels_arr.to_vec();
        let variable_size = dequantize_and_inverse_var(&levels_vec, 4, 164, TxSize::Tx32x32);
        assert_eq!(fixed_size.len(), variable_size.len());
        for (i, (&a, &b)) in fixed_size.iter().zip(variable_size.iter()).enumerate() {
            assert_eq!(a, b, "mismatch at {}", i);
        }
    }

    #[test]
    fn var_4x4_dc_produces_uniform() {
        let mut levels = vec![0i32; 16];
        levels[0] = 100;
        let result = dequantize_and_inverse_var(&levels, 1, 164, TxSize::Tx4x4);
        assert_eq!(result.len(), 16);
        let first = result[0];
        assert_ne!(first, 0);
        for &v in &result {
            assert!((v - first).abs() <= 1);
        }
    }

    #[test]
    fn var_8x8_dc_produces_uniform() {
        let mut levels = vec![0i32; 64];
        levels[0] = 100;
        let result = dequantize_and_inverse_var(&levels, 1, 164, TxSize::Tx8x8);
        assert_eq!(result.len(), 64);
        let first = result[0];
        assert_ne!(first, 0);
        for &v in &result {
            assert!((v - first).abs() <= 1);
        }
    }

    #[test]
    fn var_16x16_dc_produces_uniform() {
        let mut levels = vec![0i32; 256];
        levels[0] = 100;
        let result = dequantize_and_inverse_var(&levels, 1, 164, TxSize::Tx16x16);
        assert_eq!(result.len(), 256);
        let first = result[0];
        assert_ne!(first, 0);
        for &v in &result {
            assert!((v - first).abs() <= 1);
        }
    }
}
