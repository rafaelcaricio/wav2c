use crate::entropy::{ilog_nz, prob_scale};

const OD_EC_WINDOW_SIZE: i32 = 64;
const OD_EC_MIN_BITS: i32 = 8;
const OD_EC_LOTS_OF_BITS: i32 = 0x4000;

pub struct DaalaDecoder<'a> {
    buf: &'a [u8],
    bptr: usize,
    dif: u64,
    rng: u32,
    cnt: i32,
}

impl<'a> DaalaDecoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        let mut dec = Self {
            buf,
            bptr: 0,
            dif: (1u64 << (OD_EC_WINDOW_SIZE as u64 - 1)) - 1,
            rng: 0x8000,
            cnt: -15,
        };
        dec.refill();
        dec
    }

    fn refill(&mut self) {
        let mut s = OD_EC_WINDOW_SIZE - 9 - (self.cnt + 15);
        while s >= 0 && self.bptr < self.buf.len() {
            debug_assert!(s <= OD_EC_WINDOW_SIZE - 8);
            self.dif ^= (self.buf[self.bptr] as u64) << s as u64;
            self.cnt += 8;
            self.bptr += 1;
            s -= 8;
        }
        if self.bptr >= self.buf.len() {
            self.cnt = OD_EC_LOTS_OF_BITS;
        }
    }

    fn normalize(&mut self, dif: u64, rng: u32) {
        debug_assert!(rng <= 65535);
        let d = 16 - ilog_nz(rng) as i32;
        self.cnt -= d;
        self.dif = dif.wrapping_add(1).wrapping_shl(d as u32).wrapping_sub(1);
        self.rng = rng << d as u32;
        if self.cnt < OD_EC_MIN_BITS {
            self.refill();
        }
    }

    fn bypass_normalize(&mut self, dif: u64, n_bypass: i32) {
        self.cnt -= n_bypass;
        self.dif = dif
            .wrapping_add(1)
            .wrapping_shl(n_bypass as u32)
            .wrapping_sub(1);
        if self.cnt < OD_EC_MIN_BITS {
            self.refill();
        }
    }


    pub fn decode_bool(&mut self, f: u16) -> bool {
        debug_assert!(f > 0 && f < 32768);
        let dif = self.dif;
        let r = self.rng;
        debug_assert!(r >= 0x8000);

        let v = prob_scale(f, r, 0, 2);
        let vw = (v as u64) << (OD_EC_WINDOW_SIZE as u64 - 16);

        if dif >= vw {
            self.normalize(dif - vw, r - v);
            false
        } else {
            self.normalize(dif, v);
            true
        }
    }

    pub fn decode_symbol(&mut self, icdf: &[u16]) -> usize {
        let nsyms = icdf.len();
        let dif = self.dif;
        let r = self.rng;

        debug_assert!(dif >> (OD_EC_WINDOW_SIZE as u64 - 16) < r as u64);
        debug_assert!(r >= 0x8000);

        let c = (dif >> (OD_EC_WINDOW_SIZE as u64 - 16)) as u32;
        let mut v = r;
        let mut ret: usize = 0;
        let mut u;
        loop {
            u = v;
            v = prob_scale(icdf[ret], r, ret, nsyms);
            if c >= v {
                break;
            }
            ret += 1;
        }
        debug_assert!(v < u);
        debug_assert!(u <= r);

        let new_rng = u - v;
        let new_dif = dif - ((v as u64) << (OD_EC_WINDOW_SIZE as u64 - 16));
        self.normalize(new_dif, new_rng);
        ret
    }

    pub fn decode_symbol_mut(&mut self, cdf: &mut [u16]) -> usize {
        let nsyms = cdf.len() - 2;
        let dif = self.dif;
        let r = self.rng;

        debug_assert!(dif >> (OD_EC_WINDOW_SIZE as u64 - 16) < r as u64);
        debug_assert!(r >= 0x8000);

        let c = (dif >> (OD_EC_WINDOW_SIZE as u64 - 16)) as u32;
        let mut v = r;
        let mut ret: usize = 0;
        let mut u;
        loop {
            u = v;
            v = prob_scale(cdf[ret], r, ret, nsyms);
            if c >= v {
                break;
            }
            ret += 1;
        }
        debug_assert!(v < u);
        debug_assert!(u <= r);

        let new_rng = u - v;
        let new_dif = dif - ((v as u64) << (OD_EC_WINDOW_SIZE as u64 - 16));
        self.normalize(new_dif, new_rng);

        crate::entropy::update_cdf(cdf, ret, nsyms);

        ret
    }

    pub fn decode_bit(&mut self) -> u32 {
        self.decode_literal(1)
    }

    pub fn decode_unary(&mut self, max_bits: u32) -> u32 {
        if self.cnt < max_bits as i32 - 1 {
            self.refill();
        }

        let dif = self.dif;
        let r = self.rng;
        debug_assert!(r & 1 == 0);
        debug_assert!(r >= 0x8000);
        debug_assert!(max_bits > 0 && max_bits <= 32);

        let mut vw = (r as u64) << (OD_EC_WINDOW_SIZE as u64 - 16);
        let mut current_dif = dif;
        let mut ret: u32 = 0;
        let mut bits_consumed: u32 = 0;

        for _ in 0..max_bits {
            vw >>= 1;
            if current_dif >= vw {
                current_dif -= vw;
                ret += 1;
                bits_consumed += 1;
            } else {
                bits_consumed += 1;
                break;
            }
        }

        self.bypass_normalize(current_dif, bits_consumed as i32);
        ret
    }

    pub fn decode_uniform(&mut self, n: u32) -> u32 {
        if n <= 1 {
            return 0;
        }
        let l = 32 - n.leading_zeros();
        let m = (1u32 << l) - n;
        let v = self.decode_literal(l - 1);
        if v < m {
            v
        } else {
            (v << 1) - m + self.decode_literal(1)
        }
    }

    pub fn decode_literal(&mut self, n_bits: u32) -> u32 {
        let dif = self.dif;
        let r = self.rng;
        debug_assert!(r & 1 == 0);
        debug_assert!(r >= 0x8000);
        debug_assert!(n_bits > 0 && n_bits <= 32);

        let mut vw = (r as u64) << (OD_EC_WINDOW_SIZE as u64 - 16);
        let mut current_dif = dif;
        let mut ret: u32 = 0;

        for _ in 0..n_bits {
            vw >>= 1;
            ret <<= 1;
            if current_dif >= vw {
                current_dif -= vw;
            } else {
                ret |= 1;
            }
        }
        self.bypass_normalize(current_dif, n_bits as i32);
        ret
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entropy::DaalaCoder;

    #[test]
    fn roundtrip_bool_false() {
        let mut enc = DaalaCoder::new();
        enc.encode_bool(false, 16384);
        let buf = enc.done();

        let mut dec = DaalaDecoder::new(&buf);
        assert!(!dec.decode_bool(16384));
    }

    #[test]
    fn roundtrip_bool_true() {
        let mut enc = DaalaCoder::new();
        enc.encode_bool(true, 16384);
        let buf = enc.done();

        let mut dec = DaalaDecoder::new(&buf);
        assert!(dec.decode_bool(16384));
    }

    #[test]
    fn roundtrip_symbol_first() {
        let icdf = [12276u16, 0];
        let mut enc = DaalaCoder::new();
        enc.encode_symbol(0, &icdf);
        let buf = enc.done();

        let mut dec = DaalaDecoder::new(&buf);
        assert_eq!(dec.decode_symbol(&icdf), 0);
    }

    #[test]
    fn roundtrip_symbol_second() {
        let icdf = [12276u16, 0];
        let mut enc = DaalaCoder::new();
        enc.encode_symbol(1, &icdf);
        let buf = enc.done();

        let mut dec = DaalaDecoder::new(&buf);
        assert_eq!(dec.decode_symbol(&icdf), 1);
    }

    #[test]
    fn roundtrip_literal() {
        let mut enc = DaalaCoder::new();
        enc.encode_literal(42, 8);
        let buf = enc.done();

        let mut dec = DaalaDecoder::new(&buf);
        assert_eq!(dec.decode_literal(8), 42);
    }

    #[test]
    fn roundtrip_multiple() {
        let icdf2 = [12276u16, 0];
        let icdf4 = [3905u16, 1746, 1044, 0];

        let mut enc = DaalaCoder::new();
        enc.encode_bool(true, 16384);
        enc.encode_symbol(1, &icdf2);
        enc.encode_literal(7, 3);
        enc.encode_bool(false, 8000);
        enc.encode_symbol(2, &icdf4);
        enc.encode_literal(200, 8);
        let buf = enc.done();

        let mut dec = DaalaDecoder::new(&buf);
        assert!(dec.decode_bool(16384));
        assert_eq!(dec.decode_symbol(&icdf2), 1);
        assert_eq!(dec.decode_literal(3), 7);
        assert!(!dec.decode_bool(8000));
        assert_eq!(dec.decode_symbol(&icdf4), 2);
        assert_eq!(dec.decode_literal(8), 200);
    }

    #[test]
    fn roundtrip_tile_data_symbols() {
        let do_split_icdf = [12276u16, 0];
        let y_mode_set_icdf = [6903u16, 0];
        let y_mode_idx_icdf = [3905u16, 1746, 1044, 0];
        let uv_mode_icdf = [17593u16, 12693, 11040, 8670, 6363, 5113, 3908, 0];
        let txb_skip_icdf = [23405u16, 11811, 9903, 8015, 6357, 4785, 2340, 0];

        let mut enc = DaalaCoder::new();
        enc.encode_symbol(0, &do_split_icdf);
        enc.encode_symbol(1, &y_mode_set_icdf);
        enc.encode_symbol(0, &y_mode_idx_icdf);
        enc.encode_symbol(0, &uv_mode_icdf);
        enc.encode_symbol(0, &txb_skip_icdf);
        enc.encode_symbol(0, &txb_skip_icdf);
        enc.encode_symbol(0, &txb_skip_icdf);
        let buf = enc.done();

        let mut dec = DaalaDecoder::new(&buf);
        assert_eq!(dec.decode_symbol(&do_split_icdf), 0);
        assert_eq!(dec.decode_symbol(&y_mode_set_icdf), 1);
        assert_eq!(dec.decode_symbol(&y_mode_idx_icdf), 0);
        assert_eq!(dec.decode_symbol(&uv_mode_icdf), 0);
        assert_eq!(dec.decode_symbol(&txb_skip_icdf), 0);
        assert_eq!(dec.decode_symbol(&txb_skip_icdf), 0);
        assert_eq!(dec.decode_symbol(&txb_skip_icdf), 0);
    }

    #[test]
    fn roundtrip_all_literal_values() {
        let mut enc = DaalaCoder::new();
        for val in 0..=255u32 {
            enc.encode_literal(val, 8);
        }
        let buf = enc.done();

        let mut dec = DaalaDecoder::new(&buf);
        for val in 0..=255u32 {
            assert_eq!(dec.decode_literal(8), val, "mismatch at value {}", val);
        }
    }

    #[test]
    fn roundtrip_symbol_mut_adapts_cdf() {
        let init_cdf = vec![16384u16, 0, 0, 0];

        let mut enc_cdf = init_cdf.clone();
        let mut enc = DaalaCoder::new();
        enc.encode_symbol_mut(0, &mut enc_cdf);
        enc.encode_symbol_mut(0, &mut enc_cdf);
        enc.encode_symbol_mut(1, &mut enc_cdf);
        let buf = enc.done();

        let mut dec_cdf = init_cdf.clone();
        let mut dec = DaalaDecoder::new(&buf);
        assert_eq!(dec.decode_symbol_mut(&mut dec_cdf), 0);
        assert_eq!(dec.decode_symbol_mut(&mut dec_cdf), 0);
        assert_eq!(dec.decode_symbol_mut(&mut dec_cdf), 1);

        assert_eq!(enc_cdf, dec_cdf);
    }

    #[test]
    fn roundtrip_symbol_mut_many_symbols() {
        let init_cdf = vec![24576u16, 16384, 8192, 0, 0, 0];

        let symbols = [0usize, 1, 2, 3, 0, 0, 1, 3, 2, 0, 1, 1, 0, 2, 3, 0];
        let mut enc_cdf = init_cdf.clone();
        let mut enc = DaalaCoder::new();
        for &s in &symbols {
            enc.encode_symbol_mut(s, &mut enc_cdf);
        }
        let buf = enc.done();

        let mut dec_cdf = init_cdf.clone();
        let mut dec = DaalaDecoder::new(&buf);
        for &expected in &symbols {
            assert_eq!(dec.decode_symbol_mut(&mut dec_cdf), expected);
        }
        assert_eq!(enc_cdf, dec_cdf);
    }
}
