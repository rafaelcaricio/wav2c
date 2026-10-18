use super::writer::BitWriter;

const ATLAS_SEGMENT_ID_BITS: u8 = 3;
const ATLAS_SEGMENT_MODE_BITS: u32 = 0;

pub struct EnhancedAtlasConfig<'a> {
    pub id: u8,
    pub column_widths: &'a [u32],
    pub row_heights: &'a [u32],
}

pub fn write_enhanced_atlas(config: &EnhancedAtlasConfig<'_>) -> Vec<u8> {
    assert!(config.id < 8);
    assert!(!config.column_widths.is_empty() && config.column_widths.len() <= 64);
    assert!(!config.row_heights.is_empty() && config.row_heights.len() <= 64);
    assert!(config.column_widths.iter().all(|&width| width > 0));
    assert!(config.row_heights.iter().all(|&height| height > 0));

    let mut w = BitWriter::new();
    w.write_literal(config.id as u32, ATLAS_SEGMENT_ID_BITS);
    w.write_uvlc(ATLAS_SEGMENT_MODE_BITS);

    w.write_uvlc(config.column_widths.len() as u32 - 1);
    w.write_uvlc(config.row_heights.len() as u32 - 1);
    w.write_bit(false);
    for &width in config.column_widths {
        w.write_uvlc(width - 1);
    }
    for &height in config.row_heights {
        w.write_uvlc(height - 1);
    }

    w.write_bit(true);
    w.write_bit(false);

    w.write_bit(false);
    w.write_trailing_bits();
    w.finalize()
}

pub fn atlas_size(config: &EnhancedAtlasConfig<'_>) -> (u32, u32) {
    let width = config.column_widths.iter().sum();
    let height = config.row_heights.iter().sum();
    (width, height)
}

pub fn atlas_segment_count(config: &EnhancedAtlasConfig<'_>) -> usize {
    config.column_widths.len() * config.row_heights.len()
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

        fn uvlc(&mut self) -> u32 {
            let mut leading_zeros = 0;
            while self.bit() == 0 {
                leading_zeros += 1;
            }
            let suffix = if leading_zeros == 0 {
                0
            } else {
                self.bits(leading_zeros as u8)
            };
            (1u32 << leading_zeros) - 1 + suffix
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
    fn enhanced_atlas_round_trips() {
        let payload = write_enhanced_atlas(&EnhancedAtlasConfig {
            id: 2,
            column_widths: &[64, 32, 16],
            row_heights: &[48, 96],
        });
        let mut r = BitReader::new(&payload);
        assert_eq!(r.bits(3), 2);
        assert_eq!(r.uvlc(), 0);
        assert_eq!(r.uvlc() + 1, 3);
        assert_eq!(r.uvlc() + 1, 2);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.uvlc() + 1, 64);
        assert_eq!(r.uvlc() + 1, 32);
        assert_eq!(r.uvlc() + 1, 16);
        assert_eq!(r.uvlc() + 1, 48);
        assert_eq!(r.uvlc() + 1, 96);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.bit(), 0);
        assert!(r.trailing_bits_are_valid());
    }

    #[test]
    fn atlas_size_sums_columns_and_rows() {
        let config = EnhancedAtlasConfig {
            id: 0,
            column_widths: &[64, 128],
            row_heights: &[64, 96],
        };
        assert_eq!(atlas_size(&config), (192, 160));
        assert_eq!(atlas_segment_count(&config), 4);
    }

    #[test]
    fn two_view_row_layout_is_one_region_per_segment() {
        let config = EnhancedAtlasConfig {
            id: 0,
            column_widths: &[64, 32],
            row_heights: &[64],
        };
        assert_eq!(atlas_segment_count(&config), 2);
        assert_eq!(atlas_size(&config), (96, 64));
    }
}
