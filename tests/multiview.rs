mod common;

use common::avmdec_path;
use std::process::Command;

use av2codec::multiview::{
    AtlasLayout, MultiviewConfig, MultiviewView, PURPOSE_MULTIVIEW_PLAYBACK,
    PURPOSE_STEREOSCOPIC_VIEWPORTS, encode_multiview, encode_multiview_with_threads,
    extract_layer, stereo_view_types,
};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 64;

fn view_frames(seed: u8) -> Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let y: Vec<u8> = (0..(WIDTH * HEIGHT))
        .map(|index| {
            let x = index % WIDTH;
            let row = index / WIDTH;
            (x + row + seed as u32) as u8
        })
        .collect();
    let u: Vec<u8> = (0..(WIDTH / 2 * HEIGHT / 2) as usize)
        .map(|index| (index % 64 + seed as usize) as u8)
        .collect();
    let v: Vec<u8> = (0..(WIDTH / 2 * HEIGHT / 2) as usize)
        .map(|index| (192usize.wrapping_sub(index / 3).wrapping_add(seed as usize)) as u8)
        .collect();
    vec![(y, u, v)]
}

fn planes(frame: &(Vec<u8>, Vec<u8>, Vec<u8>)) -> Vec<u8> {
    let mut data = Vec::with_capacity(frame.0.len() + frame.1.len() + frame.2.len());
    data.extend_from_slice(&frame.0);
    data.extend_from_slice(&frame.1);
    data.extend_from_slice(&frame.2);
    data
}

fn decode_multistream(streams: usize, obu_path: &str, out_prefix: &str) -> Vec<Vec<u8>> {
    let avmdec = avmdec_path().expect("avmdec not found");
    let output = Command::new(avmdec)
        .args([
            "--codec=av2",
            "--i420",
            &format!("--num-streams={streams}"),
            "-o",
            out_prefix,
            obu_path,
        ])
        .output()
        .expect("failed to run avmdec");
    if !output.status.success() {
        panic!(
            "avmdec failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    (0..streams)
        .map(|index| {
            let path = if let Some(dot) = out_prefix.rfind('.') {
                format!(
                    "{}_{}{}",
                    &out_prefix[..dot],
                    index,
                    &out_prefix[dot..]
                )
            } else {
                format!("{out_prefix}_{index}")
            };
            std::fs::read(path).expect("decoded stream")
        })
        .collect()
}

#[test]
fn stereoscopic_pair_decodes_per_view_with_avmdec() {
    if avmdec_path().is_none() {
        eprintln!("SKIP: avmdec not found");
        return;
    }

    let left_frames = view_frames(0);
    let right_frames = view_frames(64);
    let (left_type, right_type) = stereo_view_types();
    let views = vec![
        MultiviewView::texture(WIDTH, HEIGHT, left_type, left_frames.clone()),
        MultiviewView::texture(WIDTH, HEIGHT, right_type, right_frames.clone()),
    ];
    let config = MultiviewConfig {
        base_qindex: 0,
        purpose_id: PURPOSE_STEREOSCOPIC_VIEWPORTS,
        layout: AtlasLayout::Row,
        views: &views,
    };
    let bitstream = encode_multiview(&config).expect("encode multiview");
    std::fs::write("/tmp/multiview_stereo.obu", &bitstream).unwrap();

    let decoded = decode_multistream(2, "/tmp/multiview_stereo.obu", "/tmp/multiview_stereo.yuv");
    assert_eq!(decoded[0], planes(&left_frames[0]), "left view");
    assert_eq!(decoded[1], planes(&right_frames[0]), "right view");
}

#[test]
fn three_view_atlas_decodes_per_view_with_avmdec() {
    if avmdec_path().is_none() {
        eprintln!("SKIP: avmdec not found");
        return;
    }

    let frames: Vec<_> = (0..3).map(|seed| view_frames(seed * 32)).collect();
    let views: Vec<_> = frames
        .into_iter()
        .enumerate()
        .map(|(index, frames)| {
            MultiviewView::explicit_view(WIDTH, HEIGHT, index as u8, frames)
        })
        .collect();
    let config = MultiviewConfig {
        base_qindex: 0,
        purpose_id: PURPOSE_MULTIVIEW_PLAYBACK,
        layout: AtlasLayout::Column,
        views: &views,
    };
    let bitstream = encode_multiview(&config).expect("encode multiview");
    std::fs::write("/tmp/multiview_three.obu", &bitstream).unwrap();

    let decoded = decode_multistream(3, "/tmp/multiview_three.obu", "/tmp/multiview_three.yuv");
    for (index, view) in views.iter().enumerate() {
        assert_eq!(
            decoded[index],
            planes(&view.frames[0]),
            "view {index} differs"
        );
    }
}

fn stereo_views(frames: usize) -> (Vec<MultiviewView>, Vec<Vec<u8>>) {
    let (left_type, right_type) = stereo_view_types();
    let mut left_frames = Vec::new();
    let mut right_frames = Vec::new();
    for index in 0..frames {
        let mut left = view_frames(0)[0].clone();
        let mut right = view_frames(64)[0].clone();
        left.0[0] = index as u8;
        right.0[0] = 255 - index as u8;
        left_frames.push(left);
        right_frames.push(right);
    }
    let expected = {
        let mut all = Vec::new();
        for frame in &left_frames {
            all.push(planes(frame));
        }
        for frame in &right_frames {
            all.push(planes(frame));
        }
        all
    };
    (
        vec![
            MultiviewView::texture(WIDTH, HEIGHT, left_type, left_frames),
            MultiviewView::texture(WIDTH, HEIGHT, right_type, right_frames),
        ],
        expected,
    )
}

#[test]
fn multi_frame_multiview_decodes_per_view_with_avmdec() {
    if avmdec_path().is_none() {
        eprintln!("SKIP: avmdec not found");
        return;
    }

    let (views, expected) = stereo_views(3);
    let config = MultiviewConfig {
        base_qindex: 0,
        purpose_id: PURPOSE_STEREOSCOPIC_VIEWPORTS,
        layout: AtlasLayout::Row,
        views: &views,
    };
    let bitstream = encode_multiview(&config).expect("encode multiview");
    std::fs::write("/tmp/multiview_frames.obu", &bitstream).unwrap();

    let decoded = decode_multistream(2, "/tmp/multiview_frames.obu", "/tmp/multiview_frames.yuv");
    let frame_size = expected[0].len();
    assert_eq!(decoded[0].len(), frame_size * 3);
    for index in 0..3 {
        assert_eq!(
            &decoded[0][index * frame_size..(index + 1) * frame_size],
            expected[index].as_slice(),
            "left frame {index}"
        );
        assert_eq!(
            &decoded[1][index * frame_size..(index + 1) * frame_size],
            expected[3 + index].as_slice(),
            "right frame {index}"
        );
    }
}

#[test]
fn extracted_views_are_valid_singlestreams() {
    if avmdec_path().is_none() {
        eprintln!("SKIP: avmdec not found");
        return;
    }

    let (views, expected) = stereo_views(1);
    let config = MultiviewConfig {
        base_qindex: 0,
        purpose_id: PURPOSE_STEREOSCOPIC_VIEWPORTS,
        layout: AtlasLayout::Row,
        views: &views,
    };
    let bitstream = encode_multiview(&config).expect("encode multiview");

    for xlayer_id in 0..2u8 {
        let layer = extract_layer(&bitstream, xlayer_id);
        std::fs::write(format!("/tmp/multiview_layer_{xlayer_id}.obu"), &layer).unwrap();

        let frames = av2codec::Decoder::decode_all(&layer).expect("decode extracted layer");
        assert_eq!(frames.len(), 1);
        let mut ours = Vec::new();
        ours.extend_from_slice(&frames[0].y);
        ours.extend_from_slice(&frames[0].u);
        ours.extend_from_slice(&frames[0].v);
        assert_eq!(ours, expected[xlayer_id as usize], "layer {xlayer_id}");

        let decoded = decode_single_stream(&format!("/tmp/multiview_layer_{xlayer_id}.obu"));
        assert_eq!(decoded, expected[xlayer_id as usize], "avmdec layer {xlayer_id}");
    }
}

fn decode_single_stream(obu_path: &str) -> Vec<u8> {
    let avmdec = avmdec_path().expect("avmdec not found");
    let out = "/tmp/multiview_single.yuv";
    let output = Command::new(avmdec)
        .args(["--codec=av2", "--i420", "-o", out, obu_path])
        .output()
        .expect("failed to run avmdec");
    if !output.status.success() {
        panic!(
            "avmdec failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::read(out).expect("decoded layer")
}

#[test]
fn views_with_different_resolutions_share_one_atlas() {
    if avmdec_path().is_none() {
        eprintln!("SKIP: avmdec not found");
        return;
    }

    let mut small = view_frames(16);
    small[0].0.truncate(64 * 64);
    let big_frame = {
        let y: Vec<u8> = (0..(128 * 64)).map(|index| (index % 251) as u8).collect();
        let u: Vec<u8> = vec![96; 64 * 32];
        let v: Vec<u8> = vec![160; 64 * 32];
        (y, u, v)
    };
    let views = vec![
        MultiviewView::texture(64, 64, 2, small),
        MultiviewView::texture(128, 64, 3, vec![big_frame.clone()]),
    ];
    let config = MultiviewConfig {
        base_qindex: 0,
        purpose_id: PURPOSE_STEREOSCOPIC_VIEWPORTS,
        layout: AtlasLayout::Row,
        views: &views,
    };
    let bitstream = encode_multiview(&config).expect("encode multiview");
    std::fs::write("/tmp/multiview_sizes.obu", &bitstream).unwrap();

    let decoded = decode_multistream(2, "/tmp/multiview_sizes.obu", "/tmp/multiview_sizes.yuv");
    assert_eq!(decoded[0], planes(&views[0].frames[0]));
    assert_eq!(decoded[1], planes(&views[1].frames[0]));
}

#[test]
fn invalid_multiview_configurations_are_rejected() {
    let frames = view_frames(0);

    let single = vec![MultiviewView::texture(64, 64, 2, frames.clone())];
    assert!(encode_multiview(&MultiviewConfig {
        base_qindex: 50,
        purpose_id: PURPOSE_STEREOSCOPIC_VIEWPORTS,
        layout: AtlasLayout::Row,
        views: &single,
    })
    .is_err());

    let five: Vec<_> = (0..5)
        .map(|index| MultiviewView::explicit_view(64, 64, index, frames.clone()))
        .collect();
    assert!(encode_multiview(&MultiviewConfig {
        base_qindex: 50,
        purpose_id: PURPOSE_MULTIVIEW_PLAYBACK,
        layout: AtlasLayout::Row,
        views: &five,
    })
    .is_err());

    let mismatched = vec![
        MultiviewView::texture(64, 64, 2, frames.clone()),
        MultiviewView::texture(64, 64, 3, Vec::new()),
    ];
    assert!(encode_multiview(&MultiviewConfig {
        base_qindex: 50,
        purpose_id: PURPOSE_STEREOSCOPIC_VIEWPORTS,
        layout: AtlasLayout::Row,
        views: &mismatched,
    })
    .is_err());

    let wrong_plane_size = vec![
        MultiviewView::texture(64, 64, 2, frames.clone()),
        MultiviewView::texture(64, 64, 3, vec![(vec![0; 10], vec![0; 10], vec![0; 10])]),
    ];
    assert!(encode_multiview(&MultiviewConfig {
        base_qindex: 50,
        purpose_id: PURPOSE_STEREOSCOPIC_VIEWPORTS,
        layout: AtlasLayout::Row,
        views: &wrong_plane_size,
    })
    .is_err());
}

#[test]
fn auxiliary_layers_are_signalled_and_decodable() {
    if avmdec_path().is_none() {
        eprintln!("SKIP: avmdec not found");
        return;
    }

    let texture = view_frames(0);
    let alpha = view_frames(32);
    let views = vec![
        MultiviewView::texture(WIDTH, HEIGHT, 2, texture.clone()),
        MultiviewView::auxiliary(WIDTH, HEIGHT, 2, 0, alpha.clone()),
    ];
    let config = MultiviewConfig {
        base_qindex: 0,
        purpose_id: PURPOSE_STEREOSCOPIC_VIEWPORTS,
        layout: AtlasLayout::Column,
        views: &views,
    };
    let bitstream = encode_multiview(&config).expect("encode multiview");
    std::fs::write("/tmp/multiview_aux.obu", &bitstream).unwrap();

    let decoded = decode_multistream(2, "/tmp/multiview_aux.obu", "/tmp/multiview_aux.yuv");
    assert_eq!(decoded[0], planes(&texture[0]));
    assert_eq!(decoded[1], planes(&alpha[0]));
}

#[test]
fn cli_encodes_stereoscopic_pair() {
    if avmdec_path().is_none() {
        eprintln!("SKIP: avmdec not found");
        return;
    }

    let status = Command::new("cargo")
        .args([
            "run",
            "--bin",
            "av2codec",
            "--",
            "tests/fixtures/solid_red_64x64.y4m",
            "tests/fixtures/solid_gray_64x64.y4m",
            "/tmp/multiview_cli.obu",
            "--multiview",
            "--qindex",
            "0",
        ])
        .status()
        .expect("failed to run av2codec CLI");
    assert!(status.success(), "av2codec CLI failed");

    let decoded = decode_multistream(2, "/tmp/multiview_cli.obu", "/tmp/multiview_cli.yuv");
    assert_eq!(decoded[0].len(), 64 * 64 + 2 * 32 * 32);
    assert_eq!(decoded[1].len(), 64 * 64 + 2 * 32 * 32);
    assert_eq!(decoded[0][0], 81, "solid red luma");
    assert_eq!(decoded[1][0], 126, "solid gray luma");

    let status = Command::new("cargo")
        .args([
            "run",
            "--bin",
            "av2codec",
            "--",
            "tests/fixtures/solid_red_64x64.y4m",
            "tests/fixtures/solid_gray_64x64.y4m",
            "/tmp/multiview_cli_layer.obu",
            "--multiview",
            "--qindex",
            "0",
            "--extract-layer",
            "1",
        ])
        .status()
        .expect("failed to run av2codec CLI");
    assert!(status.success(), "av2codec CLI extraction failed");

    let layer = std::fs::read("/tmp/multiview_cli_layer.obu").unwrap();
    let frames = av2codec::Decoder::decode_all(&layer).expect("decode extracted layer");
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].y[0], 126, "extracted right view luma");
}

fn four_view_grid() -> Vec<MultiviewView> {
    (0..4)
        .map(|index| {
            MultiviewView::explicit_view(WIDTH, HEIGHT, index, view_frames(index * 48))
        })
        .collect()
}

#[test]
fn grid_layout_maps_views_to_row_major_segments() {
    if avmdec_path().is_none() {
        eprintln!("SKIP: avmdec not found");
        return;
    }

    let views = four_view_grid();
    let config = MultiviewConfig {
        base_qindex: 0,
        purpose_id: PURPOSE_MULTIVIEW_PLAYBACK,
        layout: AtlasLayout::Grid { columns: 2 },
        views: &views,
    };
    let bitstream = encode_multiview(&config).expect("encode multiview");
    std::fs::write("/tmp/multiview_grid.obu", &bitstream).unwrap();

    let decoded = decode_multistream(4, "/tmp/multiview_grid.obu", "/tmp/multiview_grid.yuv");
    for (index, view) in views.iter().enumerate() {
        assert_eq!(decoded[index], planes(&view.frames[0]), "view {index}");
    }
}

#[test]
fn grid_layout_rejects_column_count_that_does_not_divide_views() {
    let views = four_view_grid();
    let config = MultiviewConfig {
        base_qindex: 50,
        purpose_id: PURPOSE_MULTIVIEW_PLAYBACK,
        layout: AtlasLayout::Grid { columns: 3 },
        views: &views,
    };
    assert!(encode_multiview(&config).is_err());
}

#[test]
fn parallel_encoding_matches_sequential_output() {
    let views = four_view_grid();
    let config = MultiviewConfig {
        base_qindex: 50,
        purpose_id: PURPOSE_MULTIVIEW_PLAYBACK,
        layout: AtlasLayout::Grid { columns: 2 },
        views: &views,
    };
    let sequential = encode_multiview(&config).expect("encode multiview");
    for threads in [2, 4] {
        let parallel = encode_multiview_with_threads(&config, threads).expect("encode multiview");
        assert_eq!(parallel, sequential, "threads={threads}");
    }
}
