use super::DecodeError;
use super::bitreader::BitReader;

const SEQ_LEVEL_TIER_THRESHOLD: u32 = 3;

pub struct SequenceParams {
    pub width: u32,
    pub height: u32,
    pub single_picture_header_flag: bool,
    pub sb_size: u32,
    pub enable_sdp: bool,
    pub enable_cfl_intra: bool,
    pub cfl_ds_filter_index: u8,
    pub enable_mhccp: bool,
    pub enable_ibp: bool,
    pub enable_mrls: bool,
    pub enable_intra_dip: bool,
    pub enable_intra_edge_filter: bool,
    pub order_hint_bits: u32,
    pub number_of_bits_for_lt_frame_id: u32,
    pub force_screen_content_tools: u32,
    pub force_integer_mv: u32,
    pub enable_refmvbank: bool,
    pub allow_frame_max_bvp_drl_bits: bool,
    pub enable_fsc: bool,
    pub enable_idtx_intra: bool,
    pub enable_ist: bool,
    pub enable_cctx: bool,
    pub reduced_tx_part_set: bool,
    pub enable_tcq: u32,
    pub enable_parity_hiding: bool,
    pub separate_uv_delta_q: bool,
    pub equal_ac_dc_q: bool,
    pub y_dc_delta_q_enabled: bool,
    pub uv_dc_delta_q_enabled: bool,
    pub uv_ac_delta_q_enabled: bool,
    pub enable_cdef: bool,
    pub enable_gdf: bool,
    pub enable_restoration: bool,
    pub enable_ccso: bool,
    pub df_par_bits: u32,
    pub film_grain_params_present: bool,
    pub enable_cdef_on_skip_txfm: u32,
    pub lr_frame_tools_count: [u32; 3],
    pub enable_ext_partitions: bool,
    pub enable_uneven_4way_partitions: bool,
    pub enable_intra_bawp: bool,
}

fn ceil_log2(n: u32) -> u32 {
    if n <= 1 {
        0
    } else {
        32 - (n - 1).leading_zeros()
    }
}

fn read_quniform(r: &mut BitReader, n: u32) -> u32 {
    if n <= 1 {
        return 0;
    }
    let l = 32 - (n - 1).leading_zeros();
    let m = (1u32 << l) - n;
    let v = r.read_literal((l - 1) as u8);
    if v < m {
        v
    } else {
        let extra = r.read_literal(1);
        (v << 1) - m + extra
    }
}

pub fn parse_sequence_header(payload: &[u8]) -> Result<SequenceParams, DecodeError> {
    let mut r = BitReader::new(payload);

    let _seq_header_id = r.read_uvlc();
    let profile = r.read_literal(5);
    if profile != 0 {
        return Err(DecodeError::UnsupportedProfile);
    }

    let sph = r.read_bit();
    let level = r.read_literal(5);
    if level > SEQ_LEVEL_TIER_THRESHOLD && !sph {
        let _seq_tier = r.read_bit();
    }

    let chroma_format = r.read_uvlc();
    let monochrome = chroma_format == 1;
    let _bitdepth_idx = r.read_uvlc();

    if !sph {
        let _seq_lcr_id = r.read_literal(3);
        let _still_picture = r.read_bit();
        let max_tlayer = r.read_literal(2);
        let max_mlayer = r.read_literal(3);
        if max_mlayer > 0 {
            let n = ceil_log2(max_mlayer + 1);
            let _mlayer_cnt = r.read_literal(n as u8);
        }
        let _monotonic_output_order_flag = r.read_bit();
        if max_mlayer > 0 {
            let mlayer_dep = r.read_bit();
            if mlayer_dep {
                parse_mlayer_dependency(&mut r, max_mlayer);
            }
        }
        if max_tlayer > 0 {
            let tlayer_dep = r.read_bit();
            if tlayer_dep {
                if max_mlayer > 0 {
                    let _multi_map = r.read_bit();
                }
                parse_tlayer_dependency(&mut r, max_tlayer);
            }
        }
    }

    let num_bits_width = r.read_literal(4) + 1;
    let num_bits_height = r.read_literal(4) + 1;
    let width = r.read_literal(num_bits_width as u8) + 1;
    let height = r.read_literal(num_bits_height as u8) + 1;

    let conf_win = r.read_bit();
    if conf_win {
        let _left = r.read_uvlc();
        let _right = r.read_uvlc();
        let _top = r.read_uvlc();
        let _bottom = r.read_uvlc();
    }

    if !sph {
        let display_model = r.read_bit();
        if display_model {
            let _initial_display_delay = r.read_literal(4);
        }
        let decoder_model = r.read_bit();
        if decoder_model {
            let _num_units = r.read_literal(32);
            let seq_max_dec = r.read_bit();
            if seq_max_dec {
                let _buf_delay = r.read_uvlc();
                let _enc_delay = r.read_uvlc();
                let _low_delay = r.read_bit();
            }
        }
    }

    let sb_size = if r.read_bit() {
        256
    } else if r.read_bit() {
        128
    } else {
        64
    };

    let enable_sdp = if !monochrome { r.read_bit() } else { false };
    if enable_sdp && !sph {
        let _extended_sdp = r.read_bit();
    }

    let enable_ext_partitions = r.read_bit();
    let enable_uneven_4way_partitions = if enable_ext_partitions {
        r.read_bit()
    } else {
        false
    };

    let _has_custom_aspect = r.read_bit();
    if _has_custom_aspect {
        let _custom_val = r.read_bit();
    }

    let ext_seg = r.read_bit();
    let seg_info_present = r.read_bit();
    if seg_info_present {
        read_seg_syntax_info(&mut r, ext_seg);
    }

    let enable_intra_dip = r.read_bit();
    let enable_intra_edge_filter = r.read_bit();
    let enable_mrls = r.read_bit();
    let enable_cfl_intra = r.read_bit();
    let cfl_ds_filter_index = if !monochrome {
        r.read_literal(2) as u8
    } else {
        0
    };
    let enable_mhccp = r.read_bit();
    let enable_ibp = r.read_bit();

    let (
        order_hint_bits,
        number_of_bits_for_lt_frame_id,
        enable_refmvbank,
        allow_frame_max_bvp_drl_bits,
        enable_intra_bawp,
    ) = parse_inter_group(&mut r, sph);

    let (force_screen_content_tools, force_integer_mv) = parse_scc_group(&mut r, sph);

    let tq = parse_tq_group(&mut r, sph, monochrome);

    let (
        enable_cdef,
        enable_gdf,
        enable_restoration,
        enable_ccso,
        df_par_bits,
        enable_cdef_on_skip_txfm,
        lr_frame_tools_count,
    ) = parse_filter_group(&mut r, sph);

    let _seq_tile_info_present = r.read_bit();
    if _seq_tile_info_present {
        parse_sequence_tile_info(&mut r);
    }

    let film_grain_params_present = r.read_bit();
    let _seq_extension_present = r.read_bit();

    Ok(SequenceParams {
        width,
        height,
        single_picture_header_flag: sph,
        sb_size,
        enable_sdp,
        enable_cfl_intra,
        cfl_ds_filter_index,
        enable_mhccp,
        enable_ibp,
        enable_mrls,
        enable_intra_edge_filter,
        enable_intra_dip,
        order_hint_bits,
        number_of_bits_for_lt_frame_id,
        force_screen_content_tools,
        force_integer_mv,
        enable_refmvbank,
        allow_frame_max_bvp_drl_bits,
        enable_fsc: tq.enable_fsc,
        enable_idtx_intra: tq.enable_idtx_intra,
        enable_ist: tq.enable_ist,
        enable_cctx: tq.enable_cctx,
        reduced_tx_part_set: tq.reduced_tx_part_set,
        enable_tcq: tq.enable_tcq,
        enable_parity_hiding: tq.enable_parity_hiding,
        separate_uv_delta_q: tq.separate_uv_delta_q,
        equal_ac_dc_q: tq.equal_ac_dc_q,
        y_dc_delta_q_enabled: tq.y_dc_delta_q_enabled,
        uv_dc_delta_q_enabled: tq.uv_dc_delta_q_enabled,
        uv_ac_delta_q_enabled: tq.uv_ac_delta_q_enabled,
        enable_cdef,
        enable_gdf,
        enable_restoration,
        enable_ccso,
        df_par_bits,
        film_grain_params_present,
        enable_cdef_on_skip_txfm,
        lr_frame_tools_count,
        enable_ext_partitions,
        enable_uneven_4way_partitions,
        enable_intra_bawp,
    })
}

fn read_seg_syntax_info(r: &mut BitReader, enable_ext_seg: bool) {
    let _allow_seg_info_change = r.read_bit();
    let max_seg_num: u32 = if enable_ext_seg { 16 } else { 8 };
    for _ in 0..max_seg_num {
        for j in 0..3u32 {
            let feature_enabled = r.read_bit();
            if feature_enabled && j == 0 {
                let _data = r.read_literal(8);
                let _sign = r.read_bit();
            }
        }
    }
}

fn parse_mlayer_dependency(r: &mut BitReader, max_mlayer_id: u32) {
    let n = ceil_log2(max_mlayer_id + 1);
    for _ in 0..max_mlayer_id {
        let dep_cnt = r.read_literal(n as u8);
        for _ in 0..dep_cnt {
            let _dep = r.read_literal(n as u8);
        }
    }
}

fn parse_tlayer_dependency(r: &mut BitReader, max_tlayer_id: u32) {
    for _ in 1..=max_tlayer_id {
        let _dep = r.read_literal(2);
    }
}

fn parse_sequence_tile_info(r: &mut BitReader) {
    let _allow_change = r.read_bit();
    let _uniform = r.read_bit();
}

fn parse_inter_group(r: &mut BitReader, sph: bool) -> (u32, u32, bool, bool, bool) {
    let mut order_hint_bits = 0u32;
    let mut number_of_bits_for_lt_frame_id = 0u32;
    let mut enable_tip = 0u32;
    let mut enable_lf_sub_pu = false;
    let mut enable_opfl_refine = 0u32;
    let mut enable_refinemv = false;

    if !sph {
        let mode_interintra = r.read_bit();
        let _mode_warp_causal = r.read_bit();
        let mode_warp_delta = r.read_bit();
        let mode_warp_extend = r.read_bit();

        let any_motion =
            mode_interintra || _mode_warp_causal || mode_warp_delta || mode_warp_extend;
        if any_motion {
            let _frame_motion_modes_present = r.read_bit();
        }
        if mode_warp_delta {
            let _six_param_warp = r.read_bit();
        }

        let _masked_compound = r.read_bit();
        let ref_frame_mvs = r.read_bit();
        if ref_frame_mvs {
            let _reduced_ref_frame_mvs = r.read_bit();
        }

        order_hint_bits = r.read_literal(4) + 1;
    }

    let enable_refmvbank = r.read_bit();

    let drl_disabled = r.read_bit();
    if !drl_disabled {
        let _drl_constraint = r.read_bit();
    }

    if !sph {
        let _explicit_ref_map = r.read_bit();

        let custom_ref = r.read_bit();
        let _ref_frames = if custom_ref { r.read_literal(4) + 1 } else { 8 };

        number_of_bits_for_lt_frame_id = r.read_literal(3);

        let _max_drl = read_quniform(r, 5) + 1;
        let _allow_max_drl = r.read_bit();
    }

    let _max_bvp_drl = read_quniform(r, 3) + 1;
    let allow_frame_max_bvp_drl_bits = r.read_bit();

    if !sph {
        let _num_same_ref_compound = r.read_literal(2);

        let tip_flag = r.read_bit();
        if tip_flag {
            enable_tip = 1 + r.read_bit() as u32;
        }
    }

    if enable_tip > 0 {
        let _tip_hole_fill = r.read_bit();
    }

    if !sph {
        let _mv_traj = r.read_bit();
    }

    let enable_intra_bawp = r.read_bit();

    if !sph {
        let _cwp = r.read_bit();
        let _imp_msk_bld = r.read_bit();
        enable_lf_sub_pu = r.read_bit();
    }

    if enable_tip == 1 && enable_lf_sub_pu {
        let _tip_explicit_qp = r.read_bit();
    }

    if !sph {
        enable_opfl_refine = r.read_literal(2);
        enable_refinemv = r.read_bit();
    }

    if enable_tip > 0 && (enable_opfl_refine > 0 || enable_refinemv) {
        let _tip_refinemv = r.read_bit();
    }

    if !sph {
        let _bru = r.read_bit();
        let _adaptive_mvd = r.read_bit();
        let _mvd_sign_derive = r.read_bit();
        let _flex_mvres = r.read_bit();
        let _global_motion = r.read_bit();
        let _short_refresh = r.read_bit();
    }

    (
        order_hint_bits,
        number_of_bits_for_lt_frame_id,
        enable_refmvbank,
        allow_frame_max_bvp_drl_bits,
        enable_intra_bawp,
    )
}

fn parse_scc_group(r: &mut BitReader, sph: bool) -> (u32, u32) {
    if sph {
        return (2, 2);
    }

    let select = r.read_bit();
    let force_sct = if select { 2 } else { r.read_bit() as u32 };

    let force_imv = if force_sct > 0 {
        let imv_select = r.read_bit();
        if imv_select { 2 } else { r.read_bit() as u32 }
    } else {
        2
    };

    (force_sct, force_imv)
}

struct TqGroupParams {
    enable_fsc: bool,
    enable_idtx_intra: bool,
    enable_ist: bool,
    enable_cctx: bool,
    reduced_tx_part_set: bool,
    enable_tcq: u32,
    enable_parity_hiding: bool,
    separate_uv_delta_q: bool,
    equal_ac_dc_q: bool,
    y_dc_delta_q_enabled: bool,
    uv_dc_delta_q_enabled: bool,
    uv_ac_delta_q_enabled: bool,
}

fn parse_tq_group(r: &mut BitReader, sph: bool, monochrome: bool) -> TqGroupParams {
    let enable_fsc = r.read_bit();
    let enable_idtx_intra = if !enable_fsc { r.read_bit() } else { true };

    let enable_ist = r.read_bit();
    let _inter_ist = r.read_bit();

    let _chroma_dctonly = if !monochrome { r.read_bit() } else { false };
    let _inter_ddt = if !sph { r.read_bit() } else { false };

    let reduced_tx_part_set = r.read_bit();

    let enable_cctx = if !monochrome { r.read_bit() } else { false };


    let tcq_bit = r.read_bit();
    let mut enable_tcq = 0u32;
    if tcq_bit {
        enable_tcq = 1;
        if !sph {
            enable_tcq += r.read_bit() as u32;
        }
    }

    let enable_parity_hiding = if enable_tcq == 0 || enable_tcq >= 2 {
        r.read_bit()
    } else {
        false
    };

    if !sph {
        let avg_cdf = r.read_bit();
        if avg_cdf {
            let _avg_cdf_type = r.read_bit();
        }
    }

    let separate_uv_delta_q = if !monochrome { r.read_bit() } else { false };

    let equal_ac_dc_q = r.read_bit();

    let mut y_dc_delta_q_enabled = false;
    let mut uv_dc_delta_q_enabled = false;
    let mut uv_ac_delta_q_enabled = false;

    if !equal_ac_dc_q {
        let _base_y_dc = r.read_literal(5);
        y_dc_delta_q_enabled = r.read_bit();
    }

    if !monochrome {
        if !equal_ac_dc_q {
            let _base_uv_dc = r.read_literal(5);
            uv_dc_delta_q_enabled = r.read_bit();
        }
        let _base_uv_ac = r.read_literal(5);
        uv_ac_delta_q_enabled = r.read_bit();
    }

    TqGroupParams {
        enable_fsc,
        enable_idtx_intra,
        enable_ist,
        enable_cctx,
        reduced_tx_part_set,
        enable_tcq,
        enable_parity_hiding,
        separate_uv_delta_q,
        equal_ac_dc_q,
        y_dc_delta_q_enabled,
        uv_dc_delta_q_enabled,
        uv_ac_delta_q_enabled,
    }
}

fn parse_filter_group(
    r: &mut BitReader,
    sph: bool,
) -> (bool, bool, bool, bool, u32, u32, [u32; 3]) {
    let _loopfilters_across_tiles = r.read_bit();
    let enable_cdef = r.read_bit();
    let enable_gdf = r.read_bit();
    let enable_restoration = r.read_bit();

    let mut lr_frame_tools_count = [4u32; 3];
    if enable_restoration {
        let lr_pc_wiener_disable = r.read_bit();
        let lr_wiener_nonsep_disable = r.read_bit();
        let uv_separate = r.read_bit();
        let lr_wiener_nonsep_uv_disable = if uv_separate {
            r.read_bit()
        } else {
            lr_wiener_nonsep_disable
        };

        let enabled_luma = (!lr_pc_wiener_disable as u32) + (!lr_wiener_nonsep_disable as u32);
        let luma_count = if enabled_luma >= 2 {
            enabled_luma + 2
        } else {
            enabled_luma + 1
        };
        let enabled_uv = !lr_wiener_nonsep_uv_disable as u32;
        let chroma_count = enabled_uv + 1;
        lr_frame_tools_count = [luma_count, chroma_count, chroma_count];
    }

    let enable_ccso = r.read_bit();

    let enable_cdef_on_skip_txfm = if !sph {
        if r.read_bit() {
            1
        } else if r.read_bit() {
            0
        } else {
            2
        }
    } else {
        2
    };

    let df_par_bits = r.read_literal(2) + 2;

    (
        enable_cdef,
        enable_gdf,
        enable_restoration,
        enable_ccso,
        df_par_bits,
        enable_cdef_on_skip_txfm,
        lr_frame_tools_count,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitstream::{SequenceHeaderConfig, write_sequence_header};

    #[test]
    fn roundtrip_64x64() {
        let payload = write_sequence_header(&SequenceHeaderConfig {
            width: 64,
            height: 64,
            lcr_id: 0,
        });
        let params = parse_sequence_header(&payload).unwrap();
        assert_eq!(params.width, 64);
        assert_eq!(params.height, 64);
        assert_eq!(params.sb_size, 64);
        assert!(!params.single_picture_header_flag);
        assert_eq!(params.order_hint_bits, 6);
        assert_eq!(params.force_screen_content_tools, 0);
    }

    #[test]
    fn roundtrip_128x128() {
        let payload = write_sequence_header(&SequenceHeaderConfig {
            width: 128,
            height: 128,
            lcr_id: 0,
        });
        let params = parse_sequence_header(&payload).unwrap();
        assert_eq!(params.width, 128);
        assert_eq!(params.height, 128);
    }

    #[test]
    fn roundtrip_256x192() {
        let payload = write_sequence_header(&SequenceHeaderConfig {
            width: 256,
            height: 192,
            lcr_id: 0,
        });
        let params = parse_sequence_header(&payload).unwrap();
        assert_eq!(params.width, 256);
        assert_eq!(params.height, 192);
    }

    #[test]
    fn roundtrip_256x256() {
        let payload = write_sequence_header(&SequenceHeaderConfig {
            width: 256,
            height: 256,
            lcr_id: 0,
        });
        let params = parse_sequence_header(&payload).unwrap();
        assert_eq!(params.width, 256);
        assert_eq!(params.height, 256);
    }

    #[test]
    fn roundtrip_1920x1080() {
        let payload = write_sequence_header(&SequenceHeaderConfig {
            width: 1920,
            height: 1080,
            lcr_id: 0,
        });
        let params = parse_sequence_header(&payload).unwrap();
        assert_eq!(params.width, 1920);
        assert_eq!(params.height, 1080);
    }

    #[test]
    fn avmenc_nofilter_fixture_enables_ibp() {
        let data = std::fs::read("tests/fixtures/avmenc_128x128_q30_nofilter.obu").unwrap();
        let obus = crate::decode::obu::parse_obus(&data);
        let sequence_header = obus
            .iter()
            .find(|o| o.obu_type == 1)
            .expect("sequence header OBU");
        let params = parse_sequence_header(sequence_header.payload).unwrap();
        assert!(params.enable_ibp);
    }
}
