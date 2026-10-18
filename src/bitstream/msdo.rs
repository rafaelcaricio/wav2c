use super::writer::BitWriter;

const NUM_STREAMS_MINUS_2_BITS: u8 = 3;
const PROFILE_BITS: u8 = 5;
const LEVEL_BITS: u8 = 5;
const LARGE_PICTURE_IDC_BITS: u8 = 3;
const XLAYER_BITS: u8 = 5;

pub struct MsdoConfig<'a> {
    pub profile_idc: u8,
    pub level_idx: u8,
    pub tier: bool,
    pub even_allocation: bool,
    pub xlayer_ids: &'a [u8],
    pub doh_constraint: bool,
}

pub fn write_msdo(config: &MsdoConfig<'_>) -> Vec<u8> {
    assert!((2..=4).contains(&config.xlayer_ids.len()));

    let mut w = BitWriter::new();
    w.write_literal(config.xlayer_ids.len() as u32 - 2, NUM_STREAMS_MINUS_2_BITS);
    w.write_literal(config.profile_idc as u32, PROFILE_BITS);
    w.write_literal(config.level_idx as u32, LEVEL_BITS);
    w.write_bit(config.tier);
    w.write_bit(config.even_allocation);
    if !config.even_allocation {
        w.write_literal(0, LARGE_PICTURE_IDC_BITS);
    }
    for &xlayer_id in config.xlayer_ids {
        assert!(xlayer_id < 31);
        w.write_literal(xlayer_id as u32, XLAYER_BITS);
        w.write_literal(config.profile_idc as u32, PROFILE_BITS);
        w.write_literal(config.level_idx as u32, LEVEL_BITS);
        w.write_bit(config.tier);
    }
    w.write_bit(config.doh_constraint);
    w.write_trailing_bits();
    w.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BitReader<'a> {
        data: &'a [u8],
        offset: usize,
    }

    impl<'a> BitReader<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self { data, offset: 0 }
        }

        fn bit(&mut self) -> u32 {
            let bit = (self.data[self.offset / 8] >> (7 - self.offset % 8)) & 1;
            self.offset += 1;
            bit as u32
        }

        fn bits(&mut self, count: u8) -> u32 {
            let mut value = 0u32;
            for _ in 0..count {
                value = (value << 1) | self.bit();
            }
            value
        }

        fn trailing_bits_are_valid(&mut self) -> bool {
            if self.bit() != 1 {
                return false;
            }
            while !self.offset.is_multiple_of(8) {
                if self.bit() != 0 {
                    return false;
                }
            }
            true
        }
    }

    #[test]
    fn msdo_round_trips_two_streams() {
        let payload = write_msdo(&MsdoConfig {
            profile_idc: 0,
            level_idx: 0,
            tier: false,
            even_allocation: true,
            xlayer_ids: &[0, 1],
            doh_constraint: true,
        });
        let mut r = BitReader::new(&payload);
        assert_eq!(r.bits(3) + 2, 2);
        assert_eq!(r.bits(5), 0);
        assert_eq!(r.bits(5), 0);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.bits(5), 0);
        assert_eq!(r.bits(5), 0);
        assert_eq!(r.bits(5), 0);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.bits(5), 1);
        assert_eq!(r.bits(5), 0);
        assert_eq!(r.bits(5), 0);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.bit(), 1);
        assert!(r.trailing_bits_are_valid());
    }

    #[test]
    fn msdo_round_trips_four_streams_without_even_allocation() {
        let payload = write_msdo(&MsdoConfig {
            profile_idc: 0,
            level_idx: 4,
            tier: false,
            even_allocation: false,
            xlayer_ids: &[0, 1, 2, 3],
            doh_constraint: true,
        });
        let mut r = BitReader::new(&payload);
        assert_eq!(r.bits(3), 2);
        assert_eq!(r.bits(5), 0);
        assert_eq!(r.bits(5), 4);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.bits(3), 0);
        for xlayer_id in 0..4 {
            assert_eq!(r.bits(5), xlayer_id);
            assert_eq!(r.bits(5), 0);
            assert_eq!(r.bits(5), 4);
            assert_eq!(r.bit(), 0);
        }
        assert_eq!(r.bit(), 1);
        assert!(r.trailing_bits_are_valid());
    }
}
