pub struct BitWriter {
    buffer: Vec<u8>,
    current_byte: u8,
    bits_in_current: u8,
}

impl BitWriter {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            current_byte: 0,
            bits_in_current: 0,
        }
    }

    pub fn write_bit(&mut self, bit: bool) {
        self.current_byte = (self.current_byte << 1) | (bit as u8);
        self.bits_in_current += 1;
        if self.bits_in_current == 8 {
            self.buffer.push(self.current_byte);
            self.current_byte = 0;
            self.bits_in_current = 0;
        }
    }

    pub fn write_bits(&mut self, value: u64, num_bits: u8) {
        assert!(num_bits <= 64);
        for i in (0..num_bits).rev() {
            self.write_bit((value >> i) & 1 == 1);
        }
    }

    pub fn write_literal(&mut self, value: u32, num_bits: u8) {
        self.write_bits(value as u64, num_bits);
    }

    pub fn write_uvlc(&mut self, value: u32) {
        let value = value as u64;
        let coded = value + 1;
        let leading_zeros = 63 - coded.leading_zeros() as u8;
        for _ in 0..leading_zeros {
            self.write_bit(false);
        }
        self.write_bits(coded, leading_zeros + 1);
    }

    pub fn write_trailing_bits(&mut self) {
        self.write_bit(true);
        while self.bits_in_current != 0 {
            self.write_bit(false);
        }
    }

    pub fn write_byte_alignment(&mut self) {
        while self.bits_in_current != 0 {
            self.write_bit(false);
        }
    }

    pub fn byte_aligned(&self) -> bool {
        self.bits_in_current == 0
    }

    pub fn total_bits_written(&self) -> usize {
        self.buffer.len() * 8 + self.bits_in_current as usize
    }

    pub fn finalize(mut self) -> Vec<u8> {
        if self.bits_in_current > 0 {
            self.current_byte <<= 8 - self.bits_in_current;
            self.buffer.push(self.current_byte);
        }
        self.buffer
    }

    pub fn as_bytes(&self) -> &[u8] {
        assert!(
            self.byte_aligned(),
            "cannot get bytes from non-aligned writer"
        );
        &self.buffer
    }
}

impl Default for BitWriter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn encode_leb128(value: u32) -> Vec<u8> {
    let mut result = Vec::new();
    let mut remaining = value;
    loop {
        let mut byte = (remaining & 0x7F) as u8;
        remaining >>= 7;
        if remaining > 0 {
            byte |= 0x80;
        }
        result.push(byte);
        if remaining == 0 {
            break;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_single_bits() {
        let mut w = BitWriter::new();
        w.write_bit(true);
        w.write_bit(false);
        w.write_bit(true);
        w.write_bit(true);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(true);
        w.write_bit(false);
        assert_eq!(w.finalize(), vec![0b10110010]);
    }

    #[test]
    fn write_bits_multi_byte() {
        let mut w = BitWriter::new();
        w.write_bits(0xFF, 8);
        w.write_bits(0x00, 8);
        assert_eq!(w.finalize(), vec![0xFF, 0x00]);
    }

    #[test]
    fn write_literal_3bits() {
        let mut w = BitWriter::new();
        w.write_literal(5, 3);
        let data = w.finalize();
        assert_eq!(data, vec![0b10100000]);
    }

    #[test]
    fn write_partial_byte_finalize() {
        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        let data = w.finalize();
        assert_eq!(data, vec![0b10100000]);
    }

    #[test]
    fn uvlc_zero() {
        let mut w = BitWriter::new();
        w.write_uvlc(0);
        assert_eq!(w.total_bits_written(), 1);
        let data = w.finalize();
        assert_eq!(data, vec![0b10000000]);
    }

    #[test]
    fn uvlc_one() {
        let mut w = BitWriter::new();
        w.write_uvlc(1);
        assert_eq!(w.total_bits_written(), 3);
        let data = w.finalize();
        assert_eq!(data, vec![0b01000000]);
    }

    #[test]
    fn uvlc_two() {
        let mut w = BitWriter::new();
        w.write_uvlc(2);
        assert_eq!(w.total_bits_written(), 3);
        let data = w.finalize();
        assert_eq!(data, vec![0b01100000]);
    }

    #[test]
    fn uvlc_three() {
        let mut w = BitWriter::new();
        w.write_uvlc(3);
        assert_eq!(w.total_bits_written(), 5);
        let data = w.finalize();
        assert_eq!(data, vec![0b0010_0000]);
    }

    #[test]
    fn trailing_bits_aligned() {
        let mut w = BitWriter::new();
        w.write_bits(0xFF, 7);
        w.write_trailing_bits();
        assert!(w.byte_aligned());
        assert_eq!(w.as_bytes(), &[0xFF]);
    }

    #[test]
    fn trailing_bits_from_empty() {
        let mut w = BitWriter::new();
        w.write_trailing_bits();
        assert_eq!(w.as_bytes(), &[0x80]);
    }

    #[test]
    fn leb128_zero() {
        assert_eq!(encode_leb128(0), vec![0x00]);
    }

    #[test]
    fn leb128_small() {
        assert_eq!(encode_leb128(127), vec![0x7F]);
    }

    #[test]
    fn leb128_two_bytes() {
        assert_eq!(encode_leb128(128), vec![0x80, 0x01]);
    }

    #[test]
    fn leb128_larger() {
        assert_eq!(encode_leb128(300), vec![0xAC, 0x02]);
    }

    #[test]
    fn total_bits_written_tracking() {
        let mut w = BitWriter::new();
        assert_eq!(w.total_bits_written(), 0);
        w.write_bit(true);
        assert_eq!(w.total_bits_written(), 1);
        w.write_bits(0, 7);
        assert_eq!(w.total_bits_written(), 8);
        w.write_bits(0, 3);
        assert_eq!(w.total_bits_written(), 11);
    }
}
