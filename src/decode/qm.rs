use crate::block::TxSize;

const QM_TOTAL_SIZE: usize = 3600;
pub const NUM_QM_LEVELS: usize = 16;

static IWT: &[u8] = include_bytes!("qm_iwt.bin");

fn qm_offset(tx_size: TxSize) -> Option<(usize, usize)> {
    match tx_size {
        TxSize::Tx4x4 => Some((0, 16)),
        TxSize::Tx8x8 => Some((16, 64)),
        TxSize::Tx16x16 => Some((80, 256)),
        TxSize::Tx32x32 => Some((336, 1024)),
        TxSize::Tx64x64 => Some((336, 1024)),
        TxSize::Tx4x8 => Some((1360, 32)),
        TxSize::Tx8x4 => Some((1392, 32)),
        TxSize::Tx8x16 => Some((1424, 128)),
        TxSize::Tx16x8 => Some((1552, 128)),
        TxSize::Tx16x32 => Some((1680, 512)),
        TxSize::Tx32x16 => Some((2192, 512)),
        TxSize::Tx32x64 => Some((336, 1024)),
        TxSize::Tx64x32 => Some((336, 1024)),
        TxSize::Tx4x16 => Some((2704, 64)),
        TxSize::Tx16x4 => Some((2768, 64)),
        TxSize::Tx8x32 => Some((2832, 256)),
        TxSize::Tx32x8 => Some((3088, 256)),
        TxSize::Tx16x64 => Some((1680, 512)),
        TxSize::Tx64x16 => Some((2192, 512)),
        TxSize::Tx4x32 => Some((3344, 128)),
        TxSize::Tx32x4 => Some((3472, 128)),
        TxSize::Tx8x64 => Some((2832, 256)),
        TxSize::Tx64x8 => Some((2832, 256)),
    }
}

pub fn iqmatrix(level: u8, plane: usize, tx_size: TxSize) -> Option<&'static [u8]> {
    if level as usize >= NUM_QM_LEVELS - 1 {
        return None;
    }
    let (offset, len) = qm_offset(tx_size)?;
    let base = (level as usize * 2 + usize::from(plane != 0)) * QM_TOTAL_SIZE + offset;
    Some(&IWT[base..base + len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_have_expected_size() {
        assert_eq!(IWT.len(), (NUM_QM_LEVELS - 1) * 2 * QM_TOTAL_SIZE);
    }

    #[test]
    fn no_matrix_for_last_level() {
        assert!(iqmatrix(15, 0, TxSize::Tx4x4).is_none());
    }

    #[test]
    fn luma_and_chroma_use_different_tables() {
        let luma = iqmatrix(0, 0, TxSize::Tx4x4).unwrap();
        let chroma = iqmatrix(0, 1, TxSize::Tx4x4).unwrap();
        assert_eq!(luma.len(), 16);
        assert_eq!(chroma.len(), 16);
        assert_ne!(luma, chroma);
    }
}
