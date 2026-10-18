use super::writer::{BitWriter, encode_leb128};

const LCR_ID_BITS: u8 = 3;
const XLAYER_MAP_BITS: u8 = 31;
const PURPOSE_ID_BITS: u8 = 7;
const ATLAS_ID_BITS: u8 = 3;
const MLAYER_MAP_BITS: u8 = 8;
const TLAYER_MAP_BITS: u8 = 4;
const LAYER_TYPE_BITS: u8 = 8;
const AUXILIARY_TYPE_BITS: u8 = 8;
const VIEW_TYPE_BITS: u8 = 8;
const VIEW_ID_BITS: u8 = 8;
const ATLAS_SEGMENT_ID_BITS: u8 = 8;
const PRIORITY_ORDER_BITS: u8 = 8;
const RENDERING_METHOD_BITS: u8 = 8;

pub const TEXTURE_LAYER: u8 = 0;
pub const AUX_LAYER: u8 = 1;
pub const STEREO_LAYER: u8 = 2;
pub const DEPENDENT_LAYER: u8 = 3;

pub const VIEW_UNSPECIFIED: u8 = 0;
pub const VIEW_CENTER: u8 = 1;
pub const VIEW_LEFT: u8 = 2;
pub const VIEW_RIGHT: u8 = 3;
pub const VIEW_EXPLICIT: u8 = 4;

pub struct EmbeddedLayerConfig {
    pub atlas_segment_id: u8,
    pub priority_order: u8,
    pub rendering_method: u8,
    pub layer_type: u8,
    pub auxiliary_type: Option<u8>,
    pub view_type: u8,
    pub view_id: Option<u8>,
}

pub struct XLayerInfoConfig<'a> {
    pub max_pic_width: u32,
    pub max_pic_height: u32,
    pub purpose_id: u8,
    pub layers: &'a [EmbeddedLayerConfig],
}

pub struct GlobalLcrConfig<'a> {
    pub id: u8,
    pub xlayer_ids: &'a [u8],
    pub purpose_id: u8,
    pub atlas_id: u8,
    pub doh_constraint: bool,
    pub enforce_tile_alignment: bool,
    pub xlayer_infos: &'a [XLayerInfoConfig<'a>],
}

pub struct LocalLcrConfig<'a> {
    pub global_id: u8,
    pub local_id: u8,
    pub atlas_id: u8,
    pub xlayer_info: XLayerInfoConfig<'a>,
}

pub fn write_xlayer_info(config: &XLayerInfoConfig<'_>) -> Vec<u8> {
    assert!(!config.layers.is_empty() && config.layers.len() <= MLAYER_MAP_BITS as usize);
    assert!(config.max_pic_width > 0 && config.max_pic_height > 0);

    let mut w = BitWriter::new();
    w.write_bit(true);
    w.write_bit(true);
    w.write_bit(false);
    w.write_bit(true);

    w.write_uvlc(config.max_pic_width);
    w.write_uvlc(config.max_pic_height);
    w.write_bit(false);
    w.write_bit(false);
    w.write_literal(config.purpose_id as u32, PURPOSE_ID_BITS);
    w.write_byte_alignment();

    let mlayer_map: u32 = (1u32 << config.layers.len()) - 1;
    w.write_literal(mlayer_map, MLAYER_MAP_BITS);
    for layer in config.layers {
        w.write_literal(0b0001, TLAYER_MAP_BITS);
        w.write_literal(layer.atlas_segment_id as u32, ATLAS_SEGMENT_ID_BITS);
        w.write_literal(layer.priority_order as u32, PRIORITY_ORDER_BITS);
        w.write_literal(layer.rendering_method as u32, RENDERING_METHOD_BITS);
        w.write_literal(layer.layer_type as u32, LAYER_TYPE_BITS);
        if layer.layer_type == AUX_LAYER {
            w.write_literal(
                layer.auxiliary_type.expect("auxiliary type") as u32,
                AUXILIARY_TYPE_BITS,
            );
        }
        w.write_literal(layer.view_type as u32, VIEW_TYPE_BITS);
        if layer.view_type == VIEW_EXPLICIT {
            w.write_literal(layer.view_id.expect("view id") as u32, VIEW_ID_BITS);
        }
        w.write_bit(true);
        w.write_byte_alignment();
    }
    w.write_byte_alignment();
    w.finalize()
}

pub fn write_global_lcr(config: &GlobalLcrConfig<'_>) -> Vec<u8> {
    assert!((1..8).contains(&config.id));
    assert_eq!(config.xlayer_ids.len(), config.xlayer_infos.len());

    let mut xlayer_map: u32 = 0;
    for &xlayer_id in config.xlayer_ids {
        assert!(xlayer_id < 31);
        xlayer_map |= 1 << xlayer_id;
    }
    assert!(xlayer_map >= 1);

    let mut w = BitWriter::new();
    w.write_literal(config.id as u32, LCR_ID_BITS);
    w.write_literal(xlayer_map, XLAYER_MAP_BITS);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(true);
    w.write_bit(false);
    w.write_bit(true);
    w.write_literal(config.purpose_id as u32, PURPOSE_ID_BITS);
    w.write_bit(config.doh_constraint);
    w.write_bit(config.enforce_tile_alignment);
    w.write_literal(config.atlas_id as u32, ATLAS_ID_BITS);
    w.write_literal(0, 5);

    for xlayer_info in config.xlayer_infos {
        let payload = write_xlayer_info(xlayer_info);
        for byte in encode_leb128(payload.len() as u32) {
            w.write_literal(byte as u32, 8);
        }
        for byte in payload {
            w.write_literal(byte as u32, 8);
        }
    }

    w.write_bit(false);
    w.write_trailing_bits();
    w.finalize()
}

pub fn write_local_lcr(config: &LocalLcrConfig<'_>) -> Vec<u8> {
    assert!((1..8).contains(&config.local_id));

    let mut w = BitWriter::new();
    w.write_literal(config.global_id as u32, LCR_ID_BITS);
    w.write_literal(config.local_id as u32, LCR_ID_BITS);
    w.write_bit(false);
    w.write_bit(true);
    w.write_literal(config.atlas_id as u32, ATLAS_ID_BITS);
    w.write_literal(0, 5);

    for byte in write_xlayer_info(&config.xlayer_info) {
        w.write_literal(byte as u32, 8);
    }

    w.write_bit(false);
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

        fn align(&mut self) {
            self.offset = self.offset.div_ceil(8) * 8;
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

    fn texture_layer(segment_id: u8, view_type: u8, view_id: Option<u8>) -> EmbeddedLayerConfig {
        EmbeddedLayerConfig {
            atlas_segment_id: segment_id,
            priority_order: segment_id,
            rendering_method: 0,
            layer_type: TEXTURE_LAYER,
            auxiliary_type: None,
            view_type,
            view_id,
        }
    }

    fn xlayer_info(layers: &[EmbeddedLayerConfig]) -> XLayerInfoConfig<'_> {
        XLayerInfoConfig {
            max_pic_width: 64,
            max_pic_height: 48,
            purpose_id: 1,
            layers,
        }
    }

    #[test]
    fn global_lcr_round_trips() {
        let layers = [texture_layer(0, VIEW_LEFT, None)];
        let infos = [xlayer_info(&layers)];
        let payload = write_global_lcr(&GlobalLcrConfig {
            id: 3,
            xlayer_ids: &[3],
            purpose_id: 1,
            atlas_id: 2,
            doh_constraint: true,
            enforce_tile_alignment: true,
            xlayer_infos: &infos,
        });
        let mut r = BitReader::new(&payload);
        assert_eq!(r.bits(3), 3);
        assert_eq!(r.bits(31), 1 << 3);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.bits(7), 1);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.bits(3), 2);
        assert_eq!(r.bits(5), 0);
        let size = r.bits(8);
        let info = write_xlayer_info(&infos[0]);
        assert_eq!(size as usize, info.len());
        for byte in &info {
            assert_eq!(r.bits(8), *byte as u32);
        }
        assert_eq!(r.bit(), 0);
        assert!(r.trailing_bits_are_valid());
    }

    #[test]
    fn xlayer_info_round_trips() {
        let layers = [
            texture_layer(0, VIEW_LEFT, None),
            EmbeddedLayerConfig {
                atlas_segment_id: 0,
                priority_order: 1,
                rendering_method: 0,
                layer_type: AUX_LAYER,
                auxiliary_type: Some(0),
                view_type: VIEW_LEFT,
                view_id: None,
            },
        ];
        let payload = write_xlayer_info(&xlayer_info(&layers));
        let mut r = BitReader::new(&payload);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.uvlc(), 64);
        assert_eq!(r.uvlc(), 48);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.bits(7), 1);
        r.align();
        assert_eq!(r.bits(8), 0b11);
        assert_eq!(r.bits(4), 0b0001);
        assert_eq!(r.bits(8), 0);
        assert_eq!(r.bits(8), 0);
        assert_eq!(r.bits(8), 0);
        assert_eq!(r.bits(8), TEXTURE_LAYER as u32);
        assert_eq!(r.bits(8), VIEW_LEFT as u32);
        assert_eq!(r.bit(), 1);
        r.align();
        assert_eq!(r.bits(4), 0b0001);
        assert_eq!(r.bits(8), 0);
        assert_eq!(r.bits(8), 1);
        assert_eq!(r.bits(8), 0);
        assert_eq!(r.bits(8), AUX_LAYER as u32);
        assert_eq!(r.bits(8), 0);
        assert_eq!(r.bits(8), VIEW_LEFT as u32);
        assert_eq!(r.bit(), 1);
    }

    #[test]
    fn local_lcr_prefix_and_payload_are_aligned() {
        let layers = [texture_layer(1, VIEW_RIGHT, None)];
        let payload = write_local_lcr(&LocalLcrConfig {
            global_id: 1,
            local_id: 1,
            atlas_id: 0,
            xlayer_info: xlayer_info(&layers),
        });
        let mut r = BitReader::new(&payload);
        assert_eq!(r.bits(3), 1);
        assert_eq!(r.bits(3), 1);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.bits(3), 0);
        assert_eq!(r.bits(5), 0);
        let info = write_xlayer_info(&xlayer_info(&layers));
        for byte in info {
            assert_eq!(r.bits(8), byte as u32);
        }
        assert_eq!(r.bit(), 0);
        assert!(r.trailing_bits_are_valid());
    }

    #[test]
    fn explicit_view_carries_view_id() {
        let layers = [texture_layer(2, VIEW_EXPLICIT, Some(7))];
        let payload = write_xlayer_info(&xlayer_info(&layers));
        let mut r = BitReader::new(&payload);
        assert_eq!(r.bits(4), 0b1101);
        assert_eq!(r.uvlc(), 64);
        assert_eq!(r.uvlc(), 48);
        assert_eq!(r.bits(2), 0);
        assert_eq!(r.bits(7), 1);
        r.align();
        assert_eq!(r.bits(8), 1);
        assert_eq!(r.bits(4), 1);
        assert_eq!(r.bits(8), 2);
        assert_eq!(r.bits(8), 2);
        assert_eq!(r.bits(8), 0);
        assert_eq!(r.bits(8), 0);
        assert_eq!(r.bits(8), VIEW_EXPLICIT as u32);
        assert_eq!(r.bits(8), 7);
        assert_eq!(r.bit(), 1);
    }
}
