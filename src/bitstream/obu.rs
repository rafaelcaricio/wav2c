use super::writer::encode_leb128;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObuType {
    SequenceHeader = 1,
    TemporalDelimiter = 2,
    MultiFrameHeader = 3,
    Clk = 4,
    Olk = 5,
    LeadingTileGroup = 6,
    RegularTileGroup = 7,
    MetadataShort = 8,
    MetadataGroup = 9,
    Switch = 10,
    LayerConfigurationRecord = 16,
    AtlasSegment = 17,
    OperatingPointSet = 18,
    MultiStreamDecoderOperation = 20,
    Padding = 25,
}

pub struct ObuWriter;

impl ObuWriter {
    pub fn pack_obu(obu_type: ObuType, payload: &[u8]) -> Vec<u8> {
        Self::pack_obu_with_layer(obu_type, 0, 0, 0, payload)
    }

    pub fn pack_obu_with_layer(
        obu_type: ObuType,
        tlayer_id: u8,
        mlayer_id: u8,
        xlayer_id: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        Self::pack_obu_raw(obu_type as u8, tlayer_id, mlayer_id, xlayer_id, payload)
    }

    pub fn pack_obu_raw(
        obu_type: u8,
        tlayer_id: u8,
        mlayer_id: u8,
        xlayer_id: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let extension_flag = mlayer_id != 0 || xlayer_id != 0;
        let header_byte = Self::encode_header_byte_raw(obu_type, extension_flag, tlayer_id);
        let header_size = if extension_flag { 2u32 } else { 1 };

        let obu_size = header_size + payload.len() as u32;
        let size_bytes = encode_leb128(obu_size);

        let mut result =
            Vec::with_capacity(size_bytes.len() + header_size as usize + payload.len());
        result.extend_from_slice(&size_bytes);
        result.push(header_byte);
        if extension_flag {
            result.push(((mlayer_id & 0x07) << 5) | (xlayer_id & 0x1F));
        }
        result.extend_from_slice(payload);
        result
    }

    #[cfg(test)]
    fn encode_header_byte(obu_type: ObuType, extension_flag: bool, tlayer_id: u8) -> u8 {
        Self::encode_header_byte_raw(obu_type as u8, extension_flag, tlayer_id)
    }

    fn encode_header_byte_raw(obu_type: u8, extension_flag: bool, tlayer_id: u8) -> u8 {
        let ext = if extension_flag { 1u8 } else { 0u8 };
        (ext << 7) | ((obu_type & 0x1F) << 2) | (tlayer_id & 0x03)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_byte_sequence_header() {
        let byte = ObuWriter::encode_header_byte(ObuType::SequenceHeader, false, 0);
        assert_eq!(byte, 0b0000_0100);
        assert_eq!(byte >> 7, 0);
        assert_eq!((byte >> 2) & 0x1F, 1);
        assert_eq!(byte & 0x03, 0);
    }

    #[test]
    fn header_byte_regular_tile_group() {
        let byte = ObuWriter::encode_header_byte(ObuType::RegularTileGroup, false, 0);
        assert_eq!((byte >> 2) & 0x1F, 7);
    }

    #[test]
    fn pack_obu_format() {
        let payload = vec![0xAA, 0xBB];
        let obu = ObuWriter::pack_obu(ObuType::SequenceHeader, &payload);

        let obu_size = 1 + 2;
        assert_eq!(obu[0], obu_size as u8);

        assert_eq!((obu[1] >> 2) & 0x1F, 1);

        assert_eq!(&obu[2..], &[0xAA, 0xBB]);
    }

    #[test]
    fn pack_obu_large_payload() {
        let payload = vec![0u8; 300];
        let obu = ObuWriter::pack_obu(ObuType::RegularTileGroup, &payload);

        let obu_size: u32 = 1 + 300;
        let expected_leb = encode_leb128(obu_size);
        assert_eq!(&obu[..expected_leb.len()], &expected_leb);

        let header_byte = obu[expected_leb.len()];
        assert_eq!((header_byte >> 2) & 0x1F, 7);

        assert_eq!(obu.len(), expected_leb.len() + 1 + 300);
    }

    #[test]
    fn pack_obu_empty_payload() {
        let obu = ObuWriter::pack_obu(ObuType::TemporalDelimiter, &[]);
        assert_eq!(obu[0], 1);
        assert_eq!((obu[1] >> 2) & 0x1F, 2);
        assert_eq!(obu.len(), 2);
    }

    #[test]
    fn pack_layer_scoped_obu_uses_extension_header() {
        let payload = [0xAA, 0xBB];
        let obu = ObuWriter::pack_obu_with_layer(
            ObuType::LayerConfigurationRecord,
            0,
            0,
            31,
            &payload,
        );

        assert_eq!(obu[0], 2 + payload.len() as u8);
        assert_eq!(obu[1], 0x80 | (16 << 2));
        assert_eq!(obu[2], 0x1F);
        assert_eq!(&obu[3..], &payload);
    }

    #[test]
    fn pack_layer_scoped_obu_carries_mlayer_and_xlayer() {
        let obu = ObuWriter::pack_obu_with_layer(ObuType::SequenceHeader, 0, 2, 5, &[0x00]);
        assert_eq!(obu[1] >> 7, 1);
        assert_eq!(obu[2], (2 << 5) | 5);
    }

    #[test]
    fn pack_zero_layer_ids_omit_extension_header() {
        let obu = ObuWriter::pack_obu_with_layer(ObuType::SequenceHeader, 0, 0, 0, &[0x00]);
        assert_eq!(obu[1] >> 7, 0);
        assert_eq!(obu.len(), 3);
    }
}
