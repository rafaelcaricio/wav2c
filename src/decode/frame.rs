use super::DecodeError;
use super::bitreader::BitReader;
use super::sequence::SequenceParams;

fn tile_log2(blk_size: u32, target: u32) -> u32 {
    let mut k = 0;
    while (blk_size << k) < target {
        k += 1;
    }
    k
}

fn parse_tile_info(r: &mut BitReader, width: u32, height: u32, sb_size: u32) {
    let _uniform_spacing = r.read_bit();

    let mi_cols = (width + 3) / 4;
    let mi_rows = (height + 3) / 4;
    let mib_size = sb_size / 4;
    let sb_cols = (mi_cols + mib_size - 1) / mib_size;
    let sb_rows = (mi_rows + mib_size - 1) / mib_size;

    let max_tile_width_sb = 4096 / sb_size;
    let max_tile_cols = 64u32;
    let max_tile_area_sb = (4096 * 2304) / (sb_size * sb_size);

    let min_log2_cols = tile_log2(max_tile_width_sb, sb_cols);
    let max_log2_cols = tile_log2(1, sb_cols.min(max_tile_cols));
    let min_log2 = tile_log2(max_tile_area_sb, sb_cols * sb_rows).max(min_log2_cols);
    let min_log2_rows = if min_log2 > 0 { min_log2 } else { 0 };
    let max_log2_rows = tile_log2(1, sb_rows.min(64));

    let mut log2_cols = min_log2_cols;
    while log2_cols < max_log2_cols {
        if !r.read_bit() {
            break;
        }
        log2_cols += 1;
    }
    let mut log2_rows = min_log2_rows;
    while log2_rows < max_log2_rows {
        if !r.read_bit() {
            break;
        }
        log2_rows += 1;
    }
}

pub struct FrameParams<'a> {
    pub base_qindex: u8,
    pub order_hint: u32,
    pub tile_data: &'a [u8],
    pub disable_cdf_update: bool,
    pub tx_mode_select: bool,
    pub reduced_tx_set_used: u8,
    pub allow_intrabc: bool,
    pub allow_screen_content_tools: bool,
    pub cur_frame_force_integer_mv: bool,
    pub max_bvp_drl_bits: u32,
    pub cdef_frame_enable: bool,
    pub nb_cdef_strengths: u32,
    pub ccso_frame_enable: bool,
    pub ccso_enable: [bool; 3],
    pub ccso_params: CcsoParams,
    pub delta_q_present: bool,
    pub delta_q_res: u32,
    pub tcq_mode: bool,
    pub allow_parity_hiding: bool,
    pub u_ac_delta_q: i32,
    pub v_ac_delta_q: i32,
}

pub const CCSO_BAND_NUM: usize = 64;

#[derive(Clone, Debug)]
pub struct CcsoPlaneParams {
    pub enabled: bool,
    pub bo_only: bool,
    pub scale_idx: u8,
    pub quant_idx: u8,
    pub ext_filter_support: u8,
    pub edge_clf: u8,
    pub max_band_log2: u8,
    pub filter_offset: [i8; CCSO_BAND_NUM * 16],
}

impl Default for CcsoPlaneParams {
    fn default() -> Self {
        Self {
            enabled: false,
            bo_only: false,
            scale_idx: 0,
            quant_idx: 0,
            ext_filter_support: 0,
            edge_clf: 0,
            max_band_log2: 0,
            filter_offset: [0; CCSO_BAND_NUM * 16],
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CcsoParams {
    pub frame_enabled: bool,
    pub planes: [CcsoPlaneParams; 3],
}

pub fn parse_frame_header<'a>(
    payload: &'a [u8],
    seq: &SequenceParams,
) -> Result<FrameParams<'a>, DecodeError> {
    if seq.single_picture_header_flag {
        parse_frame_header_sph1(payload, seq)
    } else {
        parse_frame_header_sph0(payload, seq)
    }
}

fn read_delta_q(r: &mut BitReader) -> i32 {
    if r.read_bit() {
        let magnitude = r.read_literal(6);
        let sign = r.read_bit();
        if sign {
            -(magnitude as i32)
        } else {
            magnitude as i32
        }
    } else {
        0
    }
}

fn read_uniform(r: &mut BitReader, n: u32) -> u32 {
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

struct QuantParams {
    base_qindex: u8,
    u_ac_delta_q: i32,
    v_ac_delta_q: i32,
}

fn parse_quantization(r: &mut BitReader, seq: &SequenceParams) -> QuantParams {
    let base_qindex = r.read_literal(8) as u8;
    let mut u_ac_delta_q: i32 = 0;
    let mut v_ac_delta_q: i32 = 0;

    if seq.y_dc_delta_q_enabled {
        let _y_dc_delta_q = read_delta_q(r);
    }
    if seq.uv_dc_delta_q_enabled || seq.uv_ac_delta_q_enabled {
        let mut diff_uv_delta = false;
        if seq.separate_uv_delta_q {
            diff_uv_delta = r.read_bit();
        }
        if !seq.equal_ac_dc_q && seq.uv_dc_delta_q_enabled {
            let _u_dc_delta_q = read_delta_q(r);
        }
        if seq.uv_ac_delta_q_enabled {
            u_ac_delta_q = read_delta_q(r);
        }
        if diff_uv_delta {
            if !seq.equal_ac_dc_q && seq.uv_dc_delta_q_enabled {
                let _v_dc_delta_q = read_delta_q(r);
            }
            if seq.uv_ac_delta_q_enabled {
                v_ac_delta_q = read_delta_q(r);
            }
        } else {
            v_ac_delta_q = u_ac_delta_q;
        }
    }

    QuantParams {
        base_qindex,
        u_ac_delta_q,
        v_ac_delta_q,
    }
}

fn parse_deblocking(r: &mut BitReader, df_par_bits: u32) {
    let apply0 = r.read_bit();
    let apply1 = r.read_bit();

    let (apply_u, apply_v) = if apply0 || apply1 {
        (r.read_bit(), r.read_bit())
    } else {
        (false, false)
    };

    if apply0 {
        let has_delta = r.read_bit();
        if has_delta {
            let _delta = r.read_literal(df_par_bits as u8);
        }
    }
    if apply1 {
        let has_delta = r.read_bit();
        if has_delta {
            let _delta = r.read_literal(df_par_bits as u8);
        }
    }
    if apply_u {
        let has_delta = r.read_bit();
        if has_delta {
            let _delta = r.read_literal(df_par_bits as u8);
        }
    }
    if apply_v {
        let has_delta = r.read_bit();
        if has_delta {
            let _delta = r.read_literal(df_par_bits as u8);
        }
    }
}

fn parse_cdef_params(r: &mut BitReader, enable_cdef_on_skip_txfm: u32) -> u32 {
    let _damping = r.read_literal(2);
    let nb_strengths = r.read_literal(3) + 1;

    if enable_cdef_on_skip_txfm == 2 {
        let _cdef_on_skip_txfm = r.read_bit();
    }

    for _ in 0..nb_strengths {
        let less_4 = r.read_bit();
        if less_4 {
            let _y_strength = r.read_literal(2);
        } else {
            let _y_strength = r.read_literal(6);
        }

        let uv_less_4 = r.read_bit();
        if uv_less_4 {
            let _uv_strength = r.read_literal(2);
        } else {
            let _uv_strength = r.read_literal(6);
        }
    }

    nb_strengths
}

const CCSO_QUANT_SZ: [[u16; 4]; 4] = [
    [16, 8, 32, 0],
    [56, 40, 64, 128],
    [48, 24, 96, 192],
    [80, 112, 160, 256],
];

fn read_ccso_offset_idx(r: &mut BitReader) -> usize {
    let mut idx = 0;
    for _ in 0..7 {
        if !r.read_bit() {
            break;
        }
        idx += 1;
    }
    idx
}

fn parse_ccso_params(r: &mut BitReader) -> CcsoParams {
    let ccso_offset = [0i8, 1, -1, 3, -3, 7, -7, -10];
    let ccso_scale = [1i8, 2, 3, 4];
    let mut ccso = CcsoParams {
        frame_enabled: true,
        ..CcsoParams::default()
    };
    for plane in 0..3 {
        let ccso_enable = r.read_bit();
        ccso.planes[plane].enabled = ccso_enable;
        if ccso_enable {
            let ccso_bo_only = r.read_bit();
            ccso.planes[plane].bo_only = ccso_bo_only;
            let scale_idx = r.read_literal(2) as usize;
            ccso.planes[plane].scale_idx = scale_idx as u8;

            let (max_band_log2, num_edge_intervals) = if ccso_bo_only {
                let mbl = r.read_literal(3);
                ccso.planes[plane].quant_idx = 0;
                ccso.planes[plane].ext_filter_support = 0;
                ccso.planes[plane].edge_clf = 0;
                (mbl, 1usize)
            } else {
                let quant_idx = r.read_literal(2) as usize;
                ccso.planes[plane].quant_idx = quant_idx as u8;
                let ext_filter_support = r.read_literal(3) as u8;
                ccso.planes[plane].ext_filter_support = ext_filter_support;
                let edge_clf = if CCSO_QUANT_SZ[scale_idx][quant_idx] != 0 {
                    r.read_bit() as usize
                } else {
                    0
                };
                ccso.planes[plane].edge_clf = edge_clf as u8;
                let mbl = r.read_literal(2);
                let max_edge_interval = if edge_clf == 0 { 3 } else { 2 };
                (mbl, max_edge_interval)
            };

            ccso.planes[plane].max_band_log2 = max_band_log2 as u8;
            let max_band = 1u32 << max_band_log2;

            for d0 in 0..num_edge_intervals {
                for d1 in 0..num_edge_intervals {
                    for band in 0..max_band {
                        let offset_idx = read_ccso_offset_idx(r);
                        let lut_idx = ((band as usize) << 4) + (d0 << 2) + d1;
                        ccso.planes[plane].filter_offset[lut_idx] =
                            ccso_offset[offset_idx] * ccso_scale[scale_idx];
                    }
                }
            }
        }
    }
    ccso
}

fn parse_frame_header_sph0<'a>(
    payload: &'a [u8],
    seq: &SequenceParams,
) -> Result<FrameParams<'a>, DecodeError> {
    eprintln!(
        "[FH0] payload first8={:02x?}",
        &payload[..payload.len().min(8)]
    );
    let mut r = BitReader::new(payload);

    let _first_tile_group = r.read_bit();
    eprintln!(
        "[FH0] after first_tile_group={}: pos={}",
        _first_tile_group,
        r.total_bits_read()
    );

    let mfh_id = r.read_uvlc();
    eprintln!("[FH0] after mfh_id={}: pos={}", mfh_id, r.total_bits_read());
    if mfh_id == 0 {
        let _seq_header_id = r.read_uvlc();
        eprintln!(
            "[FH0] after seq_header_id={}: pos={}",
            _seq_header_id,
            r.total_bits_read()
        );
    }

    let _long_term_id = r.read_literal(seq.number_of_bits_for_lt_frame_id as u8);
    eprintln!(
        "[FH0] after long_term_id={} (bits={}): pos={}",
        _long_term_id,
        seq.number_of_bits_for_lt_frame_id,
        r.total_bits_read()
    );

    let immediate_output_picture = r.read_bit();
    eprintln!(
        "[FH0] after immediate_output={}: pos={}",
        immediate_output_picture,
        r.total_bits_read()
    );
    if !immediate_output_picture {
        let _implicit_output_picture = r.read_bit();
        eprintln!(
            "[FH0] after implicit_output={}: pos={}",
            _implicit_output_picture,
            r.total_bits_read()
        );
    }

    let frame_size_override_flag = r.read_bit();
    eprintln!(
        "[FH0] after frame_size_override={}: pos={}",
        frame_size_override_flag,
        r.total_bits_read()
    );

    let order_hint = if seq.order_hint_bits > 0 {
        r.read_literal(seq.order_hint_bits as u8)
    } else {
        0
    };
    eprintln!(
        "[FH0] after order_hint={} (bits={}): pos={}",
        order_hint,
        seq.order_hint_bits,
        r.total_bits_read()
    );

    if frame_size_override_flag {
        let _override_width = r.read_literal(16);
        let _override_height = r.read_literal(16);
        eprintln!(
            "[FH0] after frame_size override {}x{}: pos={}",
            _override_width,
            _override_height,
            r.total_bits_read()
        );
    }

    let allow_screen_content_tools = if seq.force_screen_content_tools == 2 {
        let v = r.read_bit();
        eprintln!(
            "[FH0] after allow_sct={} (force_sct=2): pos={}",
            v,
            r.total_bits_read()
        );
        v
    } else {
        eprintln!(
            "[FH0] allow_sct derived={} (force_sct={}): pos={}",
            seq.force_screen_content_tools != 0,
            seq.force_screen_content_tools,
            r.total_bits_read()
        );
        seq.force_screen_content_tools != 0
    };

    let cur_frame_force_integer_mv = if allow_screen_content_tools && seq.force_integer_mv == 2 {
        let v = r.read_bit();
        eprintln!(
            "[FH0] after force_int_mv={}: pos={}",
            v,
            r.total_bits_read()
        );
        v
    } else {
        eprintln!(
            "[FH0] force_int_mv derived={} (force_int_mv={}): pos={}",
            seq.force_integer_mv != 0,
            seq.force_integer_mv,
            r.total_bits_read()
        );
        seq.force_integer_mv != 0
    };

    let allow_intrabc = r.read_bit();
    eprintln!(
        "[FH0] after allow_intrabc={}: pos={}",
        allow_intrabc,
        r.total_bits_read()
    );
    let max_bvp_drl_bits = if allow_intrabc {
        let allow_global_intrabc = r.read_bit();
        eprintln!(
            "[FH0] after allow_global_intrabc={}: pos={}",
            allow_global_intrabc,
            r.total_bits_read()
        );
        if allow_global_intrabc {
            let _allow_local_intrabc = r.read_bit();
            eprintln!(
                "[FH0] after allow_local_intrabc={}: pos={}",
                _allow_local_intrabc,
                r.total_bits_read()
            );
        }
        if seq.allow_frame_max_bvp_drl_bits {
            let v = read_uniform(&mut r, 4);
            eprintln!("[FH0] after max_bvp_drl={}: pos={}", v, r.total_bits_read());
            v
        } else {
            3
        }
    } else {
        3
    };
    eprintln!("[FH0] max_bvp_drl_bits={}", max_bvp_drl_bits);

    let disable_cdf_update = r.read_bit();
    eprintln!(
        "[FH0] after disable_cdf_update={}: pos={}",
        disable_cdf_update,
        r.total_bits_read()
    );

    parse_tile_info(&mut r, seq.width, seq.height, seq.sb_size);

    let qparams = parse_quantization(&mut r, seq);
    let base_qindex = qparams.base_qindex;

    let _seg_enabled = r.read_bit();
    let _using_qmatrix = r.read_bit();

    let mut delta_q_present = false;
    let mut delta_q_res = 0u32;
    if base_qindex > 0 {
        delta_q_present = r.read_bit();
        if delta_q_present {
            delta_q_res = r.read_literal(2);
        }
    }

    let coded_lossless = base_qindex == 0;

    let tcq_mode = if coded_lossless {
        false
    } else if seq.enable_tcq >= 2 {
        r.read_bit()
    } else {
        seq.enable_tcq == 1
    };

    let mut allow_parity_hiding = false;
    if !coded_lossless && seq.enable_parity_hiding && !tcq_mode {
        allow_parity_hiding = r.read_bit();
    }

    if !coded_lossless {
        parse_deblocking(&mut r, seq.df_par_bits);
    }

    if !coded_lossless && seq.enable_gdf {
        let gdf_mode = r.read_bit();
        if gdf_mode {
            let gdf_block_num = {
                let gdf_block_size = seq.sb_size;
                ((seq.width + gdf_block_size - 1) / gdf_block_size)
                    * ((seq.height + gdf_block_size - 1) / gdf_block_size)
            };
            if gdf_block_num > 1 {
                let _gdf_mode_inc = r.read_bit();
            }
            let _gdf_pic_qp_idx = r.read_literal(2);
            let _gdf_pic_scale_idx = r.read_literal(2);
        }
    }

    let mut cdef_frame_enable = false;
    let mut nb_cdef_strengths = 0u32;
    if !coded_lossless && seq.enable_cdef {
        cdef_frame_enable = r.read_bit();
        if cdef_frame_enable {
            nb_cdef_strengths = parse_cdef_params(&mut r, seq.enable_cdef_on_skip_txfm);
        }
    }

    if !coded_lossless && seq.enable_restoration {
        for plane in 0..3 {
            let _restoration_type = read_uniform(&mut r, seq.lr_frame_tools_count[plane]);
        }
    }

    let mut ccso_frame_enable = false;
    let mut ccso_enable = [false; 3];
    let mut ccso_params = CcsoParams::default();
    if !coded_lossless && seq.enable_ccso {
        ccso_frame_enable = r.read_bit();
        if ccso_frame_enable {
            ccso_params = parse_ccso_params(&mut r);
            ccso_enable = std::array::from_fn(|plane| ccso_params.planes[plane].enabled);
        }
    }

    let tx_mode_select = if !coded_lossless { r.read_bit() } else { false };

    let reduced_tx_set_used = r.read_literal(2) as u8;

    if seq.film_grain_params_present && immediate_output_picture {
        let apply_grain = r.read_bit();
        if apply_grain {
            let _fgm_id = r.read_literal(3);
            let _random_seed = r.read_literal(16);
        }
    }

    let header_bits = r.total_bits_read();
    r.skip_to_byte_boundary();

    let tile_data = if r.has_remaining() {
        r.remaining_bytes()
    } else {
        &[]
    };
    eprintln!(
        "[FH] header_bits={} tile_data_len={} first8={:02x?}",
        header_bits,
        tile_data.len(),
        &tile_data[..tile_data.len().min(8)]
    );

    Ok(FrameParams {
        base_qindex,
        order_hint,
        tile_data,
        disable_cdf_update,
        tx_mode_select,
        reduced_tx_set_used,
        allow_intrabc,
        allow_screen_content_tools,
        cur_frame_force_integer_mv,
        max_bvp_drl_bits,
        cdef_frame_enable,
        nb_cdef_strengths,
        ccso_frame_enable,
        ccso_enable,
        ccso_params,
        delta_q_present,
        delta_q_res,
        tcq_mode,
        allow_parity_hiding,
        u_ac_delta_q: qparams.u_ac_delta_q,
        v_ac_delta_q: qparams.v_ac_delta_q,
    })
}

fn parse_frame_header_sph1<'a>(
    payload: &'a [u8],
    seq: &SequenceParams,
) -> Result<FrameParams<'a>, DecodeError> {
    let mut r = BitReader::new(payload);

    let _is_first_tile_group = r.read_bit();
    eprintln!(
        "[FH_BITS] after is_first_tile_group={}: pos={}",
        _is_first_tile_group,
        r.total_bits_read()
    );

    let mfh_id = r.read_uvlc();
    eprintln!(
        "[FH_BITS] after mfh_id={}: pos={}",
        mfh_id,
        r.total_bits_read()
    );
    if mfh_id == 0 {
        let _seq_header_id = r.read_uvlc();
        eprintln!(
            "[FH_BITS] after seq_header_id={}: pos={}",
            _seq_header_id,
            r.total_bits_read()
        );
    }

    let allow_screen_content_tools = r.read_bit();
    eprintln!(
        "[FH_BITS] after allow_sct={}: pos={}",
        allow_screen_content_tools,
        r.total_bits_read()
    );
    let cur_frame_force_integer_mv = if allow_screen_content_tools {
        let v = r.read_bit();
        eprintln!(
            "[FH_BITS] after force_int_mv={}: pos={}",
            v,
            r.total_bits_read()
        );
        v
    } else {
        false
    };

    let allow_intrabc = r.read_bit();
    eprintln!(
        "[FH_BITS] after allow_intrabc={}: pos={}",
        allow_intrabc,
        r.total_bits_read()
    );
    let max_bvp_drl_bits = if allow_intrabc {
        let allow_global_intrabc = r.read_bit();
        eprintln!(
            "[FH_BITS] after allow_global_intrabc={}: pos={}",
            allow_global_intrabc,
            r.total_bits_read()
        );
        if allow_global_intrabc {
            let _allow_local_intrabc = r.read_bit();
            eprintln!(
                "[FH_BITS] after allow_local_intrabc={}: pos={}",
                _allow_local_intrabc,
                r.total_bits_read()
            );
        }
        if seq.allow_frame_max_bvp_drl_bits {
            let v = read_uniform(&mut r, 4);
            eprintln!(
                "[FH_BITS] after max_bvp_drl_bits={}: pos={}",
                v,
                r.total_bits_read()
            );
            v
        } else {
            3
        }
    } else {
        3
    };

    let disable_cdf_update = r.read_bit();
    eprintln!(
        "[FH_BITS] after disable_cdf_update={}: pos={}",
        disable_cdf_update,
        r.total_bits_read()
    );

    let pos_before_tile = r.total_bits_read();
    parse_tile_info(&mut r, seq.width, seq.height, seq.sb_size);
    eprintln!(
        "[FH_BITS] after tile_info: pos={} (consumed {})",
        r.total_bits_read(),
        r.total_bits_read() - pos_before_tile
    );

    let pos_before_quant = r.total_bits_read();
    let qparams = parse_quantization(&mut r, seq);
    let base_qindex = qparams.base_qindex;
    eprintln!(
        "[FH_BITS] after quantization qindex={} u_ac_dq={} v_ac_dq={}: pos={} (consumed {})",
        base_qindex,
        qparams.u_ac_delta_q,
        qparams.v_ac_delta_q,
        r.total_bits_read(),
        r.total_bits_read() - pos_before_quant
    );

    let _seg_enabled = r.read_bit();
    eprintln!(
        "[FH_BITS] after seg_enabled={}: pos={}",
        _seg_enabled,
        r.total_bits_read()
    );
    let _using_qmatrix = r.read_bit();
    eprintln!(
        "[FH_BITS] after using_qmatrix={}: pos={}",
        _using_qmatrix,
        r.total_bits_read()
    );

    let mut delta_q_present = false;
    let mut delta_q_res = 0u32;
    if base_qindex > 0 {
        delta_q_present = r.read_bit();
        eprintln!(
            "[FH_BITS] after delta_q_present={}: pos={}",
            delta_q_present,
            r.total_bits_read()
        );
        if delta_q_present {
            delta_q_res = r.read_literal(2);
            eprintln!(
                "[FH_BITS] after delta_q_res={}: pos={}",
                delta_q_res,
                r.total_bits_read()
            );
        }
    }

    let coded_lossless = base_qindex == 0;

    let tcq_mode = if coded_lossless {
        false
    } else if seq.enable_tcq >= 2 {
        r.read_bit()
    } else {
        seq.enable_tcq == 1
    };
    eprintln!(
        "[FH_BITS] after tcq_mode={} (enable_tcq={}): pos={}",
        tcq_mode,
        seq.enable_tcq,
        r.total_bits_read()
    );

    let mut allow_parity_hiding = false;
    if !coded_lossless && seq.enable_parity_hiding && !tcq_mode {
        allow_parity_hiding = r.read_bit();
        eprintln!(
            "[FH_BITS] after parity_hiding={} (enable_ph={}): pos={}",
            allow_parity_hiding,
            seq.enable_parity_hiding,
            r.total_bits_read()
        );
    }

    let pos_before_deblock = r.total_bits_read();
    if !coded_lossless {
        parse_deblocking(&mut r, seq.df_par_bits);
    }
    eprintln!(
        "[FH_BITS] after deblocking: pos={} (consumed {})",
        r.total_bits_read(),
        r.total_bits_read() - pos_before_deblock
    );

    let mut gdf_mode = 0u32;
    if !coded_lossless && seq.enable_gdf {
        gdf_mode = 1;
        let gdf_block_num = {
            let gdf_block_size = seq.sb_size;
            ((seq.width + gdf_block_size - 1) / gdf_block_size)
                * ((seq.height + gdf_block_size - 1) / gdf_block_size)
        };
        if gdf_block_num > 1 {
            gdf_mode += r.read_bit() as u32;
        }
        let _gdf_pic_qp_idx = r.read_literal(2);
        let _gdf_pic_scale_idx = r.read_literal(2);
    }
    eprintln!(
        "[FH_BITS] after gdf (enable={}): pos={}",
        seq.enable_gdf,
        r.total_bits_read()
    );

    let mut cdef_frame_enable = false;
    let mut nb_cdef_strengths = 0u32;
    if !coded_lossless && seq.enable_cdef {
        cdef_frame_enable = true;
        nb_cdef_strengths = parse_cdef_params(&mut r, seq.enable_cdef_on_skip_txfm);
    }
    eprintln!(
        "[FH_BITS] after cdef (enable={}): pos={}",
        seq.enable_cdef,
        r.total_bits_read()
    );

    if !coded_lossless && seq.enable_restoration {
        for plane in 0..3 {
            let _restoration_type = read_uniform(&mut r, seq.lr_frame_tools_count[plane]);
        }
    }
    eprintln!(
        "[FH_BITS] after restoration (enable={}): pos={}",
        seq.enable_restoration,
        r.total_bits_read()
    );

    let mut ccso_frame_enable = false;
    let mut ccso_enable = [false; 3];
    let mut ccso_params = CcsoParams::default();
    if !coded_lossless && seq.enable_ccso {
        ccso_frame_enable = true;
        ccso_params = parse_ccso_params(&mut r);
        ccso_enable = std::array::from_fn(|plane| ccso_params.planes[plane].enabled);
    }
    eprintln!(
        "[FH_BITS] after ccso (enable={}): pos={}",
        seq.enable_ccso,
        r.total_bits_read()
    );

    eprintln!(
        "[FH_BITS] before tx_mode_select: pos={} byte_pos={} bit_pos={} raw_byte=0x{:02x} payload_first4={:02x?}",
        r.total_bits_read(),
        r.byte_position(),
        r.bit_position(),
        r.peek_current_byte(),
        &payload[..payload.len().min(4)]
    );
    let tx_mode_select = if !coded_lossless { r.read_bit() } else { false };
    eprintln!(
        "[FH_BITS] after tx_mode_select={}: pos={}",
        tx_mode_select,
        r.total_bits_read()
    );

    let reduced_tx_set_used = r.read_literal(2) as u8;
    eprintln!(
        "[FH_BITS] after reduced_tx_set_used={}: pos={}",
        reduced_tx_set_used,
        r.total_bits_read()
    );

    eprintln!(
        "[FH_BITS] film_grain_params_present={}",
        seq.film_grain_params_present
    );
    if seq.film_grain_params_present {
        let fgm_id = r.read_literal(3);
        let random_seed = r.read_literal(16);
        eprintln!(
            "[FH_BITS] film_grain: fgm_id={} random_seed={} pos={}",
            fgm_id,
            random_seed,
            r.total_bits_read()
        );
    }

    let header_bits = r.total_bits_read();
    r.skip_to_byte_boundary();

    let tile_data = if r.has_remaining() {
        r.remaining_bytes()
    } else {
        &[]
    };
    eprintln!(
        "[FH_SPH1] header_bits={} tile_data_len={} first8={:02x?} delta_q_present={} ccso_enable={:?} nb_cdef={} gdf_mode={} reduced_tx_set={}",
        header_bits,
        tile_data.len(),
        &tile_data[..tile_data.len().min(8)],
        delta_q_present,
        ccso_enable,
        nb_cdef_strengths,
        gdf_mode,
        reduced_tx_set_used
    );

    Ok(FrameParams {
        base_qindex,
        order_hint: 0,
        tile_data,
        disable_cdf_update,
        tx_mode_select,
        reduced_tx_set_used,
        allow_intrabc,
        allow_screen_content_tools,
        cur_frame_force_integer_mv,
        max_bvp_drl_bits,
        cdef_frame_enable,
        nb_cdef_strengths,
        ccso_frame_enable,
        ccso_enable,
        ccso_params,
        delta_q_present,
        delta_q_res,
        tcq_mode,
        allow_parity_hiding,
        u_ac_delta_q: qparams.u_ac_delta_q,
        v_ac_delta_q: qparams.v_ac_delta_q,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitstream::{FrameHeaderConfig, write_clk_obu_payload};

    fn sph0_seq(width: u32, height: u32) -> SequenceParams {
        SequenceParams {
            width,
            height,
            single_picture_header_flag: false,
            sb_size: 64,
            enable_sdp: false,
            enable_cfl_intra: false,
            cfl_ds_filter_index: 0,
            enable_ibp: false,
            enable_mrls: false,
            enable_intra_dip: false,
            enable_intra_edge_filter: false,
            order_hint_bits: 6,
            number_of_bits_for_lt_frame_id: 3,
            force_screen_content_tools: 0,
            force_integer_mv: 2,
            enable_refmvbank: false,
            allow_frame_max_bvp_drl_bits: false,
            enable_fsc: false,
            enable_idtx_intra: false,
            enable_ist: false,
            enable_cctx: false,
            reduced_tx_part_set: false,
            enable_tcq: 0,
            enable_parity_hiding: false,
            separate_uv_delta_q: false,
            equal_ac_dc_q: true,
            y_dc_delta_q_enabled: false,
            uv_dc_delta_q_enabled: false,
            uv_ac_delta_q_enabled: false,
            enable_cdef: false,
            enable_gdf: false,
            enable_restoration: false,
            enable_ccso: false,
            enable_cdef_on_skip_txfm: 0,
            lr_frame_tools_count: [0, 0, 0],
            df_par_bits: 2,
            film_grain_params_present: false,
            enable_ext_partitions: false,
            enable_uneven_4way_partitions: false,
            enable_mhccp: false,
            enable_intra_bawp: false,
        }
    }

    #[test]
    fn roundtrip_64x64_qindex50() {
        let config = FrameHeaderConfig {
            width: 64,
            height: 64,
            base_qindex: 50,
            order_hint: 0,
            disable_cdf_update: true,
        };
        let tile_data = vec![0xAA, 0xBB];
        let payload = write_clk_obu_payload(&config, &tile_data);
        let seq = sph0_seq(64, 64);
        let params = parse_frame_header(&payload, &seq).unwrap();
        assert_eq!(params.base_qindex, 50);
        assert_eq!(params.order_hint, 0);
        assert_eq!(params.tile_data, &[0xAA, 0xBB]);
    }

    #[test]
    fn roundtrip_128x128_order_hint_5() {
        let config = FrameHeaderConfig {
            width: 128,
            height: 128,
            base_qindex: 80,
            order_hint: 5,
            disable_cdf_update: true,
        };
        let tile_data = vec![0x01, 0x02, 0x03, 0x04];
        let payload = write_clk_obu_payload(&config, &tile_data);
        let seq = sph0_seq(128, 128);
        let params = parse_frame_header(&payload, &seq).unwrap();
        assert_eq!(params.base_qindex, 80);
        assert_eq!(params.order_hint, 5);
        assert_eq!(params.tile_data, &[0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn roundtrip_256x192() {
        let config = FrameHeaderConfig {
            width: 256,
            height: 192,
            base_qindex: 100,
            order_hint: 3,
            disable_cdf_update: true,
        };
        let tile_data = vec![0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE];
        let payload = write_clk_obu_payload(&config, &tile_data);
        let seq = sph0_seq(256, 192);
        let params = parse_frame_header(&payload, &seq).unwrap();
        assert_eq!(params.base_qindex, 100);
        assert_eq!(params.order_hint, 3);
        assert_eq!(params.tile_data, &[0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE]);
    }

    #[test]
    fn roundtrip_qindex_0() {
        let config = FrameHeaderConfig {
            width: 64,
            height: 64,
            base_qindex: 0,
            order_hint: 7,
            disable_cdf_update: true,
        };
        let tile_data = vec![0xFF];
        let payload = write_clk_obu_payload(&config, &tile_data);
        let seq = sph0_seq(64, 64);
        let params = parse_frame_header(&payload, &seq).unwrap();
        assert_eq!(params.base_qindex, 0);
        assert_eq!(params.order_hint, 7);
        assert_eq!(params.tile_data, &[0xFF]);
    }
}
