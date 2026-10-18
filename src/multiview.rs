use crate::bitstream::{
    AUX_LAYER, EmbeddedLayerConfig, EnhancedAtlasConfig, GlobalLcrConfig, LocalLcrConfig,
    MsdoConfig, ObuType, ObuWriter, SequenceHeaderConfig, TEXTURE_LAYER, VIEW_EXPLICIT,
    VIEW_LEFT, VIEW_RIGHT, VIEW_UNSPECIFIED, XLayerInfoConfig, write_enhanced_atlas,
    write_global_lcr, write_local_lcr, write_msdo, write_sequence_header,
};
use crate::bitstream::sequence::compute_level;
use crate::encode::{EncodeError, encode_intra_frame_payload, round_up_to_64};

pub const GLOBAL_XLAYER_ID: u8 = 31;
pub const GLOBAL_LCR_ID: u8 = 1;
pub const LOCAL_LCR_ID: u8 = 1;
pub const ATLAS_ID: u8 = 0;
pub const MAX_VIEWS: usize = 4;

pub const PURPOSE_UNSPECIFIED_MULTIVIEW: u8 = 0;
pub const PURPOSE_STEREOSCOPIC_VIEWPORTS: u8 = 1;
pub const PURPOSE_MULTIVIEW_PLAYBACK: u8 = 6;

pub type ViewFrame = (Vec<u8>, Vec<u8>, Vec<u8>);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AtlasLayout {
    Row,
    Column,
}

pub struct MultiviewView {
    pub width: u32,
    pub height: u32,
    pub view_type: u8,
    pub view_id: Option<u8>,
    pub layer_type: u8,
    pub auxiliary_type: Option<u8>,
    pub frames: Vec<ViewFrame>,
}

impl MultiviewView {
    pub fn texture(width: u32, height: u32, view_type: u8, frames: Vec<ViewFrame>) -> Self {
        Self {
            width,
            height,
            view_type,
            view_id: None,
            layer_type: TEXTURE_LAYER,
            auxiliary_type: None,
            frames,
        }
    }

    pub fn explicit_view(
        width: u32,
        height: u32,
        view_id: u8,
        frames: Vec<ViewFrame>,
    ) -> Self {
        Self {
            width,
            height,
            view_type: VIEW_EXPLICIT,
            view_id: Some(view_id),
            layer_type: TEXTURE_LAYER,
            auxiliary_type: None,
            frames,
        }
    }

    pub fn auxiliary(
        width: u32,
        height: u32,
        view_type: u8,
        auxiliary_type: u8,
        frames: Vec<ViewFrame>,
    ) -> Self {
        Self {
            width,
            height,
            view_type,
            view_id: None,
            layer_type: AUX_LAYER,
            auxiliary_type: Some(auxiliary_type),
            frames,
        }
    }
}

pub struct MultiviewConfig<'a> {
    pub base_qindex: u8,
    pub purpose_id: u8,
    pub layout: AtlasLayout,
    pub views: &'a [MultiviewView],
}

pub fn stereo_view_types() -> (u8, u8) {
    (VIEW_LEFT, VIEW_RIGHT)
}

pub fn encode_multiview(config: &MultiviewConfig<'_>) -> Result<Vec<u8>, EncodeError> {
    validate(config)?;

    let padded: Vec<(u32, u32)> = config
        .views
        .iter()
        .map(|view| (round_up_to_64(view.width), round_up_to_64(view.height)))
        .collect();
    let level = padded
        .iter()
        .map(|&(width, height)| compute_level(width, height))
        .max()
        .unwrap_or(0);

    let (column_widths, row_heights) = atlas_grid(config.layout, &padded);
    let atlas_config = EnhancedAtlasConfig {
        id: ATLAS_ID,
        column_widths: &column_widths,
        row_heights: &row_heights,
    };
    let atlas_payload = write_enhanced_atlas(&atlas_config);

    let xlayer_ids: Vec<u8> = (0..config.views.len() as u8).collect();
    let msdo_payload = write_msdo(&MsdoConfig {
        profile_idc: 0,
        level_idx: level as u8,
        tier: false,
        even_allocation: true,
        xlayer_ids: &xlayer_ids,
        doh_constraint: true,
    });

    let embedded_layers: Vec<EmbeddedLayerConfig> = config
        .views
        .iter()
        .enumerate()
        .map(|(index, view)| EmbeddedLayerConfig {
            atlas_segment_id: index as u8,
            priority_order: index as u8,
            rendering_method: 0,
            layer_type: view.layer_type,
            auxiliary_type: view.auxiliary_type,
            view_type: view.view_type,
            view_id: view.view_id,
        })
        .collect();
    let xlayer_infos: Vec<XLayerInfoConfig<'_>> = embedded_layers
        .iter()
        .enumerate()
        .map(|(index, layer)| XLayerInfoConfig {
            max_pic_width: padded[index].0,
            max_pic_height: padded[index].1,
            purpose_id: config.purpose_id,
            layers: std::slice::from_ref(layer),
        })
        .collect();

    let global_lcr_payload = write_global_lcr(&GlobalLcrConfig {
        id: GLOBAL_LCR_ID,
        xlayer_ids: &xlayer_ids,
        purpose_id: config.purpose_id,
        atlas_id: ATLAS_ID,
        doh_constraint: true,
        enforce_tile_alignment: atlas_is_uniform(&padded),
        xlayer_infos: &xlayer_infos,
    });

    let sequence_payloads: Vec<Vec<u8>> = padded
        .iter()
        .map(|&(width, height)| {
            write_sequence_header(&SequenceHeaderConfig {
                width,
                height,
                lcr_id: LOCAL_LCR_ID,
            })
        })
        .collect();

    let local_lcr_payloads: Vec<Vec<u8>> = xlayer_infos
        .iter()
        .map(|xlayer_info| {
            write_local_lcr(&LocalLcrConfig {
                global_id: GLOBAL_LCR_ID,
                local_id: LOCAL_LCR_ID,
                atlas_id: ATLAS_ID,
                xlayer_info: XLayerInfoConfig {
                    max_pic_width: xlayer_info.max_pic_width,
                    max_pic_height: xlayer_info.max_pic_height,
                    purpose_id: xlayer_info.purpose_id,
                    layers: xlayer_info.layers,
                },
            })
        })
        .collect();

    let frame_count = config.views[0].frames.len();
    let mut output = Vec::new();
    for frame_index in 0..frame_count {
        output.extend_from_slice(&ObuWriter::pack_obu(ObuType::TemporalDelimiter, &[]));
        output.extend_from_slice(&ObuWriter::pack_obu(
            ObuType::MultiStreamDecoderOperation,
            &msdo_payload,
        ));
        output.extend_from_slice(&ObuWriter::pack_obu_with_layer(
            ObuType::LayerConfigurationRecord,
            0,
            0,
            GLOBAL_XLAYER_ID,
            &global_lcr_payload,
        ));
        output.extend_from_slice(&ObuWriter::pack_obu_with_layer(
            ObuType::AtlasSegment,
            0,
            0,
            GLOBAL_XLAYER_ID,
            &atlas_payload,
        ));

        for (view_index, view) in config.views.iter().enumerate() {
            let xlayer_id = view_index as u8;
            output.extend_from_slice(&ObuWriter::pack_obu_with_layer(
                ObuType::LayerConfigurationRecord,
                0,
                0,
                xlayer_id,
                &local_lcr_payloads[view_index],
            ));
            output.extend_from_slice(&ObuWriter::pack_obu_with_layer(
                ObuType::SequenceHeader,
                0,
                0,
                xlayer_id,
                &sequence_payloads[view_index],
            ));
            let (y, u, v) = &view.frames[frame_index];
            let payload = encode_intra_frame_payload(
                view.width,
                view.height,
                config.base_qindex,
                frame_index as u32,
                y,
                u,
                v,
            );
            output.extend_from_slice(&ObuWriter::pack_obu_with_layer(
                ObuType::Clk,
                0,
                0,
                xlayer_id,
                &payload,
            ));
        }
    }

    Ok(output)
}

fn validate(config: &MultiviewConfig<'_>) -> Result<(), EncodeError> {
    let count = config.views.len();
    if !(2..=MAX_VIEWS).contains(&count) {
        return Err(EncodeError::InvalidMultiview(
            "two to four views are required",
        ));
    }
    for view in config.views {
        if view.width < 4 || view.height < 4 || !view.width.is_multiple_of(4) || !view.height.is_multiple_of(4)
        {
            return Err(EncodeError::InvalidDimensions {
                width: view.width,
                height: view.height,
            });
        }
        if view.frames.is_empty() {
            return Err(EncodeError::InvalidMultiview("every view needs frames"));
        }
        if view.view_type == VIEW_UNSPECIFIED {
            return Err(EncodeError::InvalidMultiview(
                "every view needs a view type",
            ));
        }
        if view.view_type == VIEW_EXPLICIT && view.view_id.is_none() {
            return Err(EncodeError::InvalidMultiview(
                "explicit view types need a view id",
            ));
        }
        if view.layer_type == AUX_LAYER && view.auxiliary_type.is_none() {
            return Err(EncodeError::InvalidMultiview(
                "auxiliary layers need an auxiliary type",
            ));
        }
        let expected_y = (view.width * view.height) as usize;
        let expected_uv = (view.width / 2 * view.height / 2) as usize;
        for (y, u, v) in &view.frames {
            if y.len() != expected_y {
                return Err(EncodeError::InvalidPlaneSize {
                    plane: "Y",
                    expected: expected_y,
                    got: y.len(),
                });
            }
            if u.len() != expected_uv {
                return Err(EncodeError::InvalidPlaneSize {
                    plane: "U",
                    expected: expected_uv,
                    got: u.len(),
                });
            }
            if v.len() != expected_uv {
                return Err(EncodeError::InvalidPlaneSize {
                    plane: "V",
                    expected: expected_uv,
                    got: v.len(),
                });
            }
        }
    }
    let frame_count = config.views[0].frames.len();
    if config.views.iter().any(|view| view.frames.len() != frame_count) {
        return Err(EncodeError::InvalidMultiview(
            "all views must have the same frame count",
        ));
    }
    Ok(())
}

fn atlas_grid(layout: AtlasLayout, padded: &[(u32, u32)]) -> (Vec<u32>, Vec<u32>) {
    match layout {
        AtlasLayout::Row => (
            padded.iter().map(|&(width, _)| width).collect(),
            vec![padded.iter().map(|&(_, height)| height).max().unwrap()],
        ),
        AtlasLayout::Column => (
            vec![padded.iter().map(|&(width, _)| width).max().unwrap()],
            padded.iter().map(|&(_, height)| height).collect(),
        ),
    }
}

fn atlas_is_uniform(padded: &[(u32, u32)]) -> bool {
    padded.iter().all(|&size| size == padded[0])
}

pub fn extract_layer(bitstream: &[u8], xlayer_id: u8) -> Vec<u8> {
    let mut output = Vec::new();
    for obu in crate::decode::obu::parse_obus(bitstream) {
        if obu.obu_type == ObuType::MultiStreamDecoderOperation as u8 {
            continue;
        }
        let belongs_to_layer =
            obu.obu_xlayer_id == GLOBAL_XLAYER_ID || obu.obu_xlayer_id == xlayer_id;
        if !belongs_to_layer {
            continue;
        }
        let global_inferred = obu.obu_type == ObuType::TemporalDelimiter as u8;
        let (mlayer_id, xlayer) = if global_inferred {
            (0, 0)
        } else {
            (obu.obu_mlayer_id, obu.obu_xlayer_id)
        };
        output.extend_from_slice(&ObuWriter::pack_obu_raw(
            obu.obu_type,
            obu.obu_tlayer_id,
            mlayer_id,
            xlayer,
            obu.payload,
        ));
    }
    output
}
