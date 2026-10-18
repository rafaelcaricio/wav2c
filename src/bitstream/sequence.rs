use super::writer::BitWriter;

const PROFILE_BITS: u8 = 5;
const LEVEL_BITS: u8 = 5;
const SEQ_LEVEL_TIER_THRESHOLD: u32 = 3;
const DELTA_DCQUANT_BITS: u8 = 5;
const DELTA_DCQUANT_MAX: i32 = 1 << (DELTA_DCQUANT_BITS - 2);
const DELTA_DCQUANT_MIN: i32 = DELTA_DCQUANT_MAX - (1 << DELTA_DCQUANT_BITS) + 1;

pub struct SequenceHeaderConfig {
    pub width: u32,
    pub height: u32,
    pub lcr_id: u8,
}

pub fn write_sequence_header(config: &SequenceHeaderConfig) -> Vec<u8> {
    let mut w = BitWriter::new();

    w.write_uvlc(0);
    w.write_literal(0, PROFILE_BITS);
    w.write_bit(false);

    let level = compute_level(config.width, config.height);
    w.write_literal(level, LEVEL_BITS);
    if level > SEQ_LEVEL_TIER_THRESHOLD {
        w.write_bit(false);
    }

    w.write_uvlc(0);
    w.write_uvlc(1);

    w.write_literal(config.lcr_id as u32, 3);
    w.write_bit(false);
    w.write_literal(0, 2);
    w.write_literal(0, 3);
    w.write_bit(false);

    let num_bits_width = bits_needed(config.width);
    let num_bits_height = bits_needed(config.height);
    w.write_literal(num_bits_width - 1, 4);
    w.write_literal(num_bits_height - 1, 4);
    w.write_literal(config.width - 1, num_bits_width as u8);
    w.write_literal(config.height - 1, num_bits_height as u8);

    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);

    write_partition_group(&mut w);
    write_segment_group(&mut w);
    write_intra_group(&mut w);
    write_inter_group(&mut w);
    write_scc_group(&mut w);
    write_transform_quant_entropy_group(&mut w);
    write_filter_group(&mut w);

    w.write_bit(false);

    w.write_bit(false);
    w.write_bit(false);

    w.write_trailing_bits();
    w.finalize()
}

pub(crate) fn compute_level(width: u32, height: u32) -> u32 {
    let pic_size = (width as u64) * (height as u64);
    let max_dim = width.max(height);
    const LEVELS: [(u32, u64, u32); 9] = [
        (640, 147456, 0),
        (880, 278784, 1),
        (1360, 665856, 2),
        (1720, 1065024, 3),
        (2560, 2359296, 4),
        (4975, 8912896, 6),
        (9951, 35651584, 10),
        (19902, 142606336, 14),
        (38400, 530841600, 18),
    ];
    for &(dim, pic, level_idx) in &LEVELS {
        if width <= dim && height <= dim && pic_size <= pic && max_dim <= dim {
            return level_idx;
        }
    }
    21
}

fn bits_needed(value: u32) -> u32 {
    if value <= 1 {
        return 1;
    }
    32 - (value - 1).leading_zeros()
}

fn write_quniform(w: &mut BitWriter, n: u16, v: u16) {
    if n <= 1 {
        return;
    }
    assert!(v < n);
    let l = 16 - (n - 1).leading_zeros();
    let m = (1u16 << l) - n;
    if v < m {
        w.write_literal(v as u32, (l - 1) as u8);
    } else {
        w.write_literal((m + ((v - m) >> 1)) as u32, (l - 1) as u8);
        w.write_bit((v - m) & 1 == 1);
    }
}

fn write_partition_group(w: &mut BitWriter) {
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
}

fn write_segment_group(w: &mut BitWriter) {
    w.write_bit(false);
    w.write_bit(false);
}

fn write_intra_group(w: &mut BitWriter) {
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_literal(0, 2);
    w.write_bit(false);
    w.write_bit(false);
}

fn write_inter_group(w: &mut BitWriter) {
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);

    w.write_bit(false);
    w.write_bit(false);

    w.write_literal(5, 4);

    w.write_bit(false);

    w.write_bit(true);

    w.write_bit(false);

    w.write_bit(false);

    w.write_literal(3, 3);

    write_quniform(w, 5, 0);
    w.write_bit(false);

    write_quniform(w, 3, 0);
    w.write_bit(false);

    w.write_literal(0, 2);
    w.write_bit(false);

    w.write_bit(false);

    w.write_bit(false);

    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);

    w.write_literal(0, 2);
    w.write_bit(false);

    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
}

fn write_scc_group(w: &mut BitWriter) {
    w.write_bit(false);
    w.write_bit(false);
}

fn write_transform_quant_entropy_group(w: &mut BitWriter) {
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(true);
    let base_uv_ac_delta_q: i32 = 0;
    let offset = (base_uv_ac_delta_q - DELTA_DCQUANT_MIN) as u32;
    w.write_literal(offset, DELTA_DCQUANT_BITS);
    w.write_bit(false);
}

fn write_filter_group(w: &mut BitWriter) {
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_literal(0, 2);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitstream::ObuWriter;
    use crate::bitstream::obu::ObuType;

    #[test]
    fn bits_needed_cases() {
        assert_eq!(bits_needed(1), 1);
        assert_eq!(bits_needed(2), 1);
        assert_eq!(bits_needed(64), 6);
        assert_eq!(bits_needed(128), 7);
        assert_eq!(bits_needed(256), 8);
        assert_eq!(bits_needed(1920), 11);
    }

    #[test]
    fn quniform_n3_v0() {
        let mut w = BitWriter::new();
        write_quniform(&mut w, 3, 0);
        assert_eq!(w.total_bits_written(), 1);
        let data = w.finalize();
        assert_eq!(data, vec![0b0000_0000]);
    }

    #[test]
    fn quniform_n3_v1() {
        let mut w = BitWriter::new();
        write_quniform(&mut w, 3, 1);
        assert_eq!(w.total_bits_written(), 2);
    }

    #[test]
    fn quniform_n3_v2() {
        let mut w = BitWriter::new();
        write_quniform(&mut w, 3, 2);
        assert_eq!(w.total_bits_written(), 2);
    }

    #[test]
    fn sequence_header_starts_with_uvlc_zero() {
        let config = SequenceHeaderConfig {
            width: 64,
            height: 64,
            lcr_id: 0,
        };
        let payload = write_sequence_header(&config);
        assert_eq!(payload[0] >> 7, 1, "First bit should be 1 (UVLC(0))");
    }

    #[test]
    fn sequence_header_obu_well_formed() {
        let config = SequenceHeaderConfig {
            width: 64,
            height: 64,
            lcr_id: 0,
        };
        let payload = write_sequence_header(&config);
        let obu = ObuWriter::pack_obu(ObuType::SequenceHeader, &payload);
        assert_eq!((obu[1] >> 2) & 0x1F, 1);
    }
}
