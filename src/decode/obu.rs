use super::bitreader::decode_leb128;

pub struct RawObu<'a> {
    pub obu_type: u8,
    pub obu_tlayer_id: u8,
    pub obu_mlayer_id: u8,
    pub obu_xlayer_id: u8,
    pub payload: &'a [u8],
}

pub fn decode_header_byte(byte: u8) -> (u8, bool) {
    let extension_flag = (byte >> 7) & 1 == 1;
    let obu_type = (byte >> 2) & 0x1F;
    (obu_type, extension_flag)
}

pub fn parse_obus(data: &[u8]) -> Vec<RawObu<'_>> {
    let mut obus = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        let (obu_size, leb_bytes) = decode_leb128(&data[offset..]);
        offset += leb_bytes;

        let obu_size = obu_size as usize;
        if obu_size == 0 || offset + obu_size > data.len() {
            break;
        }

        let header_byte = data[offset];
        let (obu_type, extension_flag) = decode_header_byte(header_byte);
        let obu_tlayer_id = header_byte & 0x03;

        let mut payload_start = offset + 1;
        let mut obu_mlayer_id = 0;
        let mut obu_xlayer_id = if obu_type == 2 || obu_type == 20 { 31 } else { 0 };
        if extension_flag {
            if payload_start >= offset + obu_size {
                break;
            }
            let extension_byte = data[payload_start];
            obu_mlayer_id = (extension_byte >> 5) & 0x07;
            obu_xlayer_id = extension_byte & 0x1F;
            payload_start += 1;
        }

        let payload_end = offset + obu_size;
        let payload = &data[payload_start..payload_end];

        obus.push(RawObu {
            obu_type,
            obu_tlayer_id,
            obu_mlayer_id,
            obu_xlayer_id,
            payload,
        });

        offset += obu_size;
    }

    obus
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitstream::obu::{ObuType, ObuWriter};

    #[test]
    fn parse_single_obu() {
        let packed = ObuWriter::pack_obu(ObuType::SequenceHeader, &[0xAA, 0xBB]);
        let obus = parse_obus(&packed);
        assert_eq!(obus.len(), 1);
        assert_eq!(obus[0].obu_type, 1);
        assert_eq!(obus[0].payload, &[0xAA, 0xBB]);
    }

    #[test]
    fn parse_multiple_obus() {
        let mut data = ObuWriter::pack_obu(ObuType::SequenceHeader, &[0x01, 0x02]);
        data.extend_from_slice(&ObuWriter::pack_obu(ObuType::Clk, &[0x03, 0x04, 0x05]));
        let obus = parse_obus(&data);
        assert_eq!(obus.len(), 2);
        assert_eq!(obus[0].obu_type, 1);
        assert_eq!(obus[0].payload, &[0x01, 0x02]);
        assert_eq!(obus[1].obu_type, 4);
        assert_eq!(obus[1].payload, &[0x03, 0x04, 0x05]);
    }

    #[test]
    fn parse_header_byte() {
        let (obu_type, extension_flag) = decode_header_byte(0b0000_0100);
        assert_eq!(obu_type, 1);
        assert!(!extension_flag);
    }

    #[test]
    fn parse_layer_scoped_obu_header() {
        const OBU_LAYER_CONFIGURATION_RECORD: u8 = 16;
        let payload = [0x11u8, 0x22, 0x33];
        let mut data = vec![(2 + payload.len()) as u8];
        data.push(0x80 | (OBU_LAYER_CONFIGURATION_RECORD << 2) | 1);
        data.push((2 << 5) | 5);
        data.extend_from_slice(&payload);

        let obus = parse_obus(&data);
        assert_eq!(obus.len(), 1);
        assert_eq!(obus[0].obu_type, OBU_LAYER_CONFIGURATION_RECORD);
        assert_eq!(obus[0].obu_tlayer_id, 1);
        assert_eq!(obus[0].obu_mlayer_id, 2);
        assert_eq!(obus[0].obu_xlayer_id, 5);
        assert_eq!(obus[0].payload, &payload);
    }

    #[test]
    fn layer_ids_default_to_zero_without_extension_header() {
        let packed = ObuWriter::pack_obu(ObuType::SequenceHeader, &[0x01]);
        let obus = parse_obus(&packed);
        assert_eq!(obus[0].obu_mlayer_id, 0);
        assert_eq!(obus[0].obu_xlayer_id, 0);
        assert_eq!(obus[0].obu_tlayer_id, 0);
    }

    #[test]
    fn truncated_extension_header_ends_parsing() {
        let data = [0x01u8, 0x80 | (16 << 2)];
        assert!(parse_obus(&data).is_empty());
    }

    #[test]
    fn temporal_delimiter_is_scoped_to_the_global_layer() {
        let packed = ObuWriter::pack_obu(ObuType::TemporalDelimiter, &[]);
        let obus = parse_obus(&packed);
        assert_eq!(obus[0].obu_xlayer_id, 31);
    }

    #[test]
    fn parse_empty_payload() {
        let packed = ObuWriter::pack_obu(ObuType::TemporalDelimiter, &[]);
        let obus = parse_obus(&packed);
        assert_eq!(obus.len(), 1);
        assert_eq!(obus[0].obu_type, 2);
        assert!(obus[0].payload.is_empty());
    }

    #[test]
    fn parse_large_payload() {
        let payload: Vec<u8> = (0..300).map(|i| (i & 0xFF) as u8).collect();
        let packed = ObuWriter::pack_obu(ObuType::RegularTileGroup, &payload);
        let obus = parse_obus(&packed);
        assert_eq!(obus.len(), 1);
        assert_eq!(obus[0].obu_type, 7);
        assert_eq!(obus[0].payload.len(), 300);
        assert_eq!(obus[0].payload, &payload[..]);
    }
}
