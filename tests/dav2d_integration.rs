mod common;

use common::{
    avmdec_available, dav2d_available, decode_with_avmdec, decode_with_dav2d,
    decode_with_dav2d_demuxer,
};

fn fixture_path(name: &str) -> String {
    format!("tests/fixtures/{}.obu", name)
}

#[test]
fn reference_fixtures_agree_between_decoders() {
    if !avmdec_available() || !dav2d_available() {
        eprintln!("SKIP: avmdec or dav2d not found");
        return;
    }

    for name in [
        "avmenc_minimal",
        "avmenc_128x128_q30",
        "avmenc_lossless_128x128",
        "avmenc_parity_128x128",
        "avmenc_qm_128x128",
        "avmenc_mrls_128x128",
    ] {
        let obu = fixture_path(name);
        let avm = decode_with_avmdec(&obu, &format!("/tmp/{name}_avm.yuv"));
        let dav = decode_with_dav2d(&obu, &format!("/tmp/{name}_dav.yuv"));
        assert_eq!(avm, dav, "{name} differs between avmdec and dav2d");
    }
}

#[test]
fn internal_decoder_matches_reference_decoders() {
    if !avmdec_available() || !dav2d_available() {
        eprintln!("SKIP: avmdec or dav2d not found");
        return;
    }

    for name in [
        "avmenc_minimal",
        "avmenc_128x128_q30",
        "avmenc_lossless_128x128",
        "avmenc_parity_128x128",
        "avmenc_qm_128x128",
        "avmenc_mrls_128x128",
    ] {
        let obu = fixture_path(name);
        let data = std::fs::read(&obu).unwrap();
        let frames = av2codec::Decoder::decode_all(&data).unwrap();
        let mut ours = Vec::new();
        for frame in &frames {
            ours.extend_from_slice(&frame.y);
            ours.extend_from_slice(&frame.u);
            ours.extend_from_slice(&frame.v);
        }
        let avm = decode_with_avmdec(&obu, &format!("/tmp/{name}_avm2.yuv"));
        let dav = decode_with_dav2d(&obu, &format!("/tmp/{name}_dav2.yuv"));
        assert_eq!(ours, avm, "{name} internal decoder differs from avmdec");
        assert_eq!(ours, dav, "{name} internal decoder differs from dav2d");
    }
}

#[test]
fn layer_metadata_fixture_matches_reference_decoders() {
    if !avmdec_available() || !dav2d_available() {
        eprintln!("SKIP: avmdec or dav2d not found");
        return;
    }

    let name = "avmenc_lcr_atlas_128x128";
    let obu = fixture_path(name);
    let data = std::fs::read(&obu).unwrap();
    let frames = av2codec::Decoder::decode_all(&data).unwrap();
    let mut ours = Vec::new();
    for frame in &frames {
        ours.extend_from_slice(&frame.y);
        ours.extend_from_slice(&frame.u);
        ours.extend_from_slice(&frame.v);
    }

    let avm = decode_with_avmdec(&obu, &format!("/tmp/{name}_avm.yuv"));
    let dav = decode_with_dav2d_demuxer(&obu, &format!("/tmp/{name}_dav.yuv"), "section5");
    assert_eq!(ours, avm, "{name} internal decoder differs from avmdec");
    assert_eq!(ours, dav, "{name} internal decoder differs from dav2d");
}

#[test]
fn encoded_streams_agree_between_reference_decoders() {
    if !avmdec_available() || !dav2d_available() {
        eprintln!("SKIP: avmdec or dav2d not found");
        return;
    }

    use av2codec::y4m::Y4mReader;

    for fixture in ["solid_red_64x64", "gradient_64x64", "testsrc2_64x64"] {
        let mut reader =
            Y4mReader::open(&format!("tests/fixtures/{fixture}.y4m")).expect("y4m fixture");
        let frames: Vec<_> = std::iter::from_fn(|| reader.next_frame()).collect();
        let frames: Vec<_> = frames
            .into_iter()
            .map(|frame| (frame.y, frame.u, frame.v))
            .collect();
        for qindex in [0u8, 50, 200] {
            let bitstream = av2codec::encode::encode_video_yuv(
                reader.header().width,
                reader.header().height,
                qindex,
                &frames,
            );
            let obu = format!("/tmp/dav2d_{fixture}_q{qindex}.obu");
            std::fs::write(&obu, &bitstream).unwrap();
            let avm = decode_with_avmdec(&obu, &format!("/tmp/dav2d_{fixture}_q{qindex}_avm.yuv"));
            let dav = decode_with_dav2d(&obu, &format!("/tmp/dav2d_{fixture}_q{qindex}_dav.yuv"));
            assert_eq!(
                avm, dav,
                "{fixture} q{qindex} avmdec and dav2d disagree"
            );
        }
    }
}
