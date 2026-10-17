use std::fs;
use std::process::Command;

mod common;
use common::{avmdec_available, decode_with_avmdec};

fn psnr(original: &[u8], decoded: &[u8]) -> f64 {
    assert_eq!(original.len(), decoded.len());
    let mse: f64 = original
        .iter()
        .zip(decoded.iter())
        .map(|(&a, &b)| {
            let d = a as f64 - b as f64;
            d * d
        })
        .sum::<f64>()
        / original.len() as f64;
    if mse == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0_f64 * 255.0 / mse).log10()
}

fn encode_y4m_fixture(fixture_path: &str, out_path: &str, qindex: u8) {
    let status = Command::new("cargo")
        .args([
            "run",
            "--bin",
            "av2codec",
            "--",
            fixture_path,
            out_path,
            "--qindex",
            &qindex.to_string(),
        ])
        .status()
        .expect("failed to run av2codec CLI");
    assert!(status.success(), "av2codec CLI failed");
}

#[test]
#[ignore]
fn avmdec_decodes_single_frame_midgray() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let bs = av2codec::encode::encode_still_image(64, 64, 50);
    fs::write("/tmp/av2codec_test_single.obu", &bs).unwrap();

    let decoded = decode_with_avmdec(
        "/tmp/av2codec_test_single.obu",
        "/tmp/av2codec_test_single_out.yuv",
    );

    let frame_size = 64 * 64 + 32 * 32 * 2;
    assert_eq!(decoded.len(), frame_size, "Expected 1 decoded I420 frame");
    assert_eq!(decoded[0], 128, "Mid-gray frame should decode to Y=128");
}

#[test]
#[ignore]
fn avmdec_decodes_multi_frame_midgray() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let frames: Vec<Vec<u8>> = (0..3).map(|_| vec![128; 64 * 64]).collect();
    let bs = av2codec::encode::encode_video(64, 64, 50, &frames);
    fs::write("/tmp/av2codec_test_multi.obu", &bs).unwrap();

    let decoded = decode_with_avmdec("/tmp/av2codec_test_multi.obu", "/tmp/av2codec_test_multi_out.yuv");

    let frame_size = 64 * 64 + 32 * 32 * 2;
    assert_eq!(decoded.len(), frame_size * 3, "Expected 3 decoded frames");

    for f in 0..3 {
        let y_start = f * frame_size;
        assert_eq!(decoded[y_start], 128, "Frame {} Y should be 128", f);
    }
}

#[test]
#[ignore]
fn avmdec_decodes_solid_green_all_planes() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let y_plane = vec![149u8; 64 * 64];
    let u_plane = vec![43u8; 32 * 32];
    let v_plane = vec![21u8; 32 * 32];
    let bs = av2codec::encode::encode_video_yuv(64, 64, 50, &[(y_plane, u_plane, v_plane)]);
    fs::write("/tmp/av2codec_test_green.obu", &bs).unwrap();

    let decoded = decode_with_avmdec("/tmp/av2codec_test_green.obu", "/tmp/av2codec_test_green_out.yuv");

    let y_size = 64 * 64;
    let uv_size = 32 * 32;
    assert_eq!(decoded.len(), y_size + 2 * uv_size);

    let y_pixel = decoded[0];
    let u_pixel = decoded[y_size];
    let v_pixel = decoded[y_size + uv_size];

    eprintln!(
        "Input Y=149 U=43 V=21, Decoded Y={} U={} V={}",
        y_pixel, u_pixel, v_pixel
    );

    assert_eq!(y_pixel, 149, "Y pixel should be 149, got {}", y_pixel);
    assert_eq!(u_pixel, 43, "U pixel should be 43, got {}", u_pixel);
    assert_eq!(v_pixel, 21, "V pixel should be 21, got {}", v_pixel);
}

#[test]
#[ignore]
fn avmdec_decodes_solid_red_all_planes() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let y_val: u8 = 81;
    let u_val: u8 = 90;
    let v_val: u8 = 240;
    let y_plane = vec![y_val; 64 * 64];
    let u_plane = vec![u_val; 32 * 32];
    let v_plane = vec![v_val; 32 * 32];
    let bs = av2codec::encode::encode_video_yuv(64, 64, 50, &[(y_plane, u_plane, v_plane)]);
    fs::write("/tmp/av2codec_test_red.obu", &bs).unwrap();

    let decoded = decode_with_avmdec("/tmp/av2codec_test_red.obu", "/tmp/av2codec_test_red_out.yuv");

    let y_size = 64 * 64;
    let uv_size = 32 * 32;
    assert_eq!(decoded.len(), y_size + 2 * uv_size);

    let dy = decoded[0];
    let du = decoded[y_size];
    let dv = decoded[y_size + uv_size];

    eprintln!(
        "Input Y={} U={} V={}, Decoded Y={} U={} V={}",
        y_val, u_val, v_val, dy, du, dv
    );

    let y_diff = (dy as i32 - y_val as i32).abs();
    let u_diff = (du as i32 - u_val as i32).abs();
    let v_diff = (dv as i32 - v_val as i32).abs();
    assert!(
        y_diff <= 3,
        "Y pixel mismatch: expected {}, got {} (diff {})",
        y_val,
        dy,
        y_diff
    );
    assert!(
        u_diff <= 3,
        "U pixel mismatch: expected {}, got {} (diff {})",
        u_val,
        du,
        u_diff
    );
    assert!(
        v_diff <= 3,
        "V pixel mismatch: expected {}, got {} (diff {})",
        v_val,
        dv,
        v_diff
    );
}

#[test]
#[ignore]
fn avmdec_decodes_multi_frame_yuv() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let colors: [(u8, u8, u8); 3] = [(149, 43, 21), (81, 90, 240), (128, 128, 128)];
    let frames: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> = colors
        .iter()
        .map(|&(y, u, v)| (vec![y; 64 * 64], vec![u; 32 * 32], vec![v; 32 * 32]))
        .collect();

    let bs = av2codec::encode::encode_video_yuv(64, 64, 50, &frames);
    fs::write("/tmp/av2codec_test_multi_yuv.obu", &bs).unwrap();

    let decoded = decode_with_avmdec(
        "/tmp/av2codec_test_multi_yuv.obu",
        "/tmp/av2codec_test_multi_yuv_out.yuv",
    );

    let y_size = 64 * 64;
    let uv_size = 32 * 32;
    let frame_size = y_size + 2 * uv_size;
    assert_eq!(decoded.len(), frame_size * 3, "Expected 3 decoded frames");

    for (f, &(ey, eu, ev)) in colors.iter().enumerate() {
        let base = f * frame_size;
        let dy = decoded[base];
        let du = decoded[base + y_size];
        let dv = decoded[base + y_size + uv_size];

        eprintln!(
            "Frame {}: expected Y={} U={} V={}, got Y={} U={} V={}",
            f, ey, eu, ev, dy, du, dv
        );

        let y_diff = (dy as i32 - ey as i32).abs();
        let u_diff = (du as i32 - eu as i32).abs();
        let v_diff = (dv as i32 - ev as i32).abs();
        assert!(
            y_diff <= 3,
            "Frame {} Y mismatch: expected {}, got {} (diff {})",
            f,
            ey,
            dy,
            y_diff
        );
        assert!(
            u_diff <= 3,
            "Frame {} U mismatch: expected {}, got {} (diff {})",
            f,
            eu,
            du,
            u_diff
        );
        assert!(
            v_diff <= 3,
            "Frame {} V mismatch: expected {}, got {} (diff {})",
            f,
            ev,
            dv,
            v_diff
        );
    }
}

#[test]
#[ignore]
fn cli_encodes_y4m_fixture() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let status = Command::new("cargo")
        .args([
            "run",
            "--bin",
            "av2codec",
            "--",
            "tests/fixtures/solid_green_64x64.y4m",
            "/tmp/av2codec_cli_fixture.obu",
            "--qindex",
            "50",
        ])
        .status()
        .expect("failed to run av2codec CLI");
    assert!(status.success(), "av2codec CLI failed");

    let decoded = decode_with_avmdec(
        "/tmp/av2codec_cli_fixture.obu",
        "/tmp/av2codec_cli_fixture_out.yuv",
    );

    let y_size = 64 * 64;
    let uv_size = 32 * 32;
    assert_eq!(decoded.len(), y_size + 2 * uv_size);

    let y_pixel = decoded[0];
    eprintln!("CLI fixture: Decoded Y={}", y_pixel);
    let diff = (y_pixel as i32 - 149).abs();
    assert!(diff < 10, "Y pixel should be close to 149, got {}", y_pixel);
}

#[test]
#[ignore]
fn cli_encodes_multi_frame_y4m() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let status = Command::new("cargo")
        .args([
            "run",
            "--bin",
            "av2codec",
            "--",
            "tests/fixtures/multi_gray_64x64_3f.y4m",
            "/tmp/av2codec_cli_multi_fix.obu",
            "--qindex",
            "50",
        ])
        .status()
        .expect("failed to run av2codec CLI");
    assert!(status.success(), "av2codec CLI failed");

    let decoded = decode_with_avmdec(
        "/tmp/av2codec_cli_multi_fix.obu",
        "/tmp/av2codec_cli_multi_fix_out.yuv",
    );

    let frame_size = 64 * 64 + 32 * 32 * 2;
    assert_eq!(decoded.len(), frame_size * 3, "Expected 3 decoded frames");

    for f in 0..3 {
        let y_start = f * frame_size;
        assert_eq!(
            decoded[y_start], 126,
            "Frame {} Y should be 126 (ffmpeg gray)",
            f
        );
    }
}

#[test]
#[ignore]
fn avmdec_decodes_gradient() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    encode_y4m_fixture(
        "tests/fixtures/gradient_64x64.y4m",
        "/tmp/av2codec_test_gradient.obu",
        50,
    );

    let decoded = decode_with_avmdec(
        "/tmp/av2codec_test_gradient.obu",
        "/tmp/av2codec_test_gradient_out.yuv",
    );

    let y_size = 64 * 64;
    let uv_size = 32 * 32;
    assert_eq!(decoded.len(), y_size + 2 * uv_size);

    let y_plane = &decoded[..y_size];
    let y_min = *y_plane.iter().min().unwrap();
    let y_max = *y_plane.iter().max().unwrap();
    eprintln!("Gradient decoded: Y range [{}, {}]", y_min, y_max);
    assert!(
        y_max - y_min > 20,
        "Gradient should have non-trivial Y range, got [{}, {}]",
        y_min,
        y_max
    );
}

#[test]
#[ignore]
fn avmdec_decodes_testsrc2() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    encode_y4m_fixture(
        "tests/fixtures/testsrc2_64x64.y4m",
        "/tmp/av2codec_test_testsrc2.obu",
        50,
    );

    let decoded = decode_with_avmdec(
        "/tmp/av2codec_test_testsrc2.obu",
        "/tmp/av2codec_test_testsrc2_out.yuv",
    );

    let y_size = 64 * 64;
    let uv_size = 32 * 32;
    assert_eq!(decoded.len(), y_size + 2 * uv_size);

    let y_plane = &decoded[..y_size];
    let y_min = *y_plane.iter().min().unwrap();
    let y_max = *y_plane.iter().max().unwrap();
    eprintln!("Testsrc2 decoded: Y range [{}, {}]", y_min, y_max);
    assert!(
        y_max - y_min > 30,
        "Test pattern should have significant Y range, got [{}, {}]",
        y_min,
        y_max
    );
}

#[test]
#[ignore]
fn avmdec_gradient_psnr() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let mut reader = av2codec::y4m::Y4mReader::open("tests/fixtures/gradient_64x64.y4m").unwrap();
    let frame = reader.next_frame().unwrap();
    let original_y = frame.y.clone();
    let original_u = frame.u.clone();
    let original_v = frame.v.clone();

    let bs = av2codec::encode::encode_video_yuv(64, 64, 50, &[(frame.y, frame.u, frame.v)]);
    fs::write("/tmp/av2codec_test_gradient_psnr.obu", &bs).unwrap();

    let decoded = decode_with_avmdec(
        "/tmp/av2codec_test_gradient_psnr.obu",
        "/tmp/av2codec_test_gradient_psnr_out.yuv",
    );

    let y_size = 64 * 64;
    let uv_size = 32 * 32;
    let decoded_y = &decoded[..y_size];
    let decoded_u = &decoded[y_size..y_size + uv_size];
    let decoded_v = &decoded[y_size + uv_size..];

    let y_psnr = psnr(&original_y, decoded_y);
    let u_psnr = psnr(&original_u, decoded_u);
    let v_psnr = psnr(&original_v, decoded_v);

    eprintln!(
        "Gradient PSNR: Y={:.1}dB U={:.1}dB V={:.1}dB",
        y_psnr, u_psnr, v_psnr
    );

    assert!(
        y_psnr > 20.0,
        "Y PSNR should be > 20dB for gradient at qindex=50, got {:.1}dB",
        y_psnr
    );
}

#[test]
#[ignore]
fn avmdec_decodes_128x128_solid_green() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let y_plane = vec![149u8; 128 * 128];
    let u_plane = vec![43u8; 64 * 64];
    let v_plane = vec![21u8; 64 * 64];
    let bs = av2codec::encode::encode_video_yuv(128, 128, 50, &[(y_plane, u_plane, v_plane)]);
    fs::write("/tmp/av2codec_test_128x128_green.obu", &bs).unwrap();

    let decoded = decode_with_avmdec(
        "/tmp/av2codec_test_128x128_green.obu",
        "/tmp/av2codec_test_128x128_green_out.yuv",
    );

    let y_size = 128 * 128;
    let uv_size = 64 * 64;
    assert_eq!(decoded.len(), y_size + 2 * uv_size);

    let y_pixel = decoded[0];
    let u_pixel = decoded[y_size];
    let v_pixel = decoded[y_size + uv_size];

    eprintln!(
        "128x128 Input Y=149 U=43 V=21, Decoded Y={} U={} V={}",
        y_pixel, u_pixel, v_pixel
    );

    assert_eq!(y_pixel, 149, "Y pixel should be 149, got {}", y_pixel);
    assert_eq!(u_pixel, 43, "U pixel should be 43, got {}", u_pixel);
    assert_eq!(v_pixel, 21, "V pixel should be 21, got {}", v_pixel);
}

#[test]
#[ignore]
fn avmdec_decodes_256x128_solid_red() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let y_plane = vec![81u8; 256 * 128];
    let u_plane = vec![90u8; 128 * 64];
    let v_plane = vec![240u8; 128 * 64];
    let bs = av2codec::encode::encode_video_yuv(256, 128, 50, &[(y_plane, u_plane, v_plane)]);
    fs::write("/tmp/av2codec_test_256x128_red.obu", &bs).unwrap();

    let decoded = decode_with_avmdec(
        "/tmp/av2codec_test_256x128_red.obu",
        "/tmp/av2codec_test_256x128_red_out.yuv",
    );

    let y_size = 256 * 128;
    let uv_size = 128 * 64;
    assert_eq!(decoded.len(), y_size + 2 * uv_size);

    let dy = decoded[0];
    let du = decoded[y_size];
    let dv = decoded[y_size + uv_size];

    eprintln!(
        "256x128 Input Y=81 U=90 V=240, Decoded Y={} U={} V={}",
        dy, du, dv
    );

    let y_diff = (dy as i32 - 81).abs();
    let u_diff = (du as i32 - 90).abs();
    let v_diff = (dv as i32 - 240).abs();
    assert!(
        y_diff <= 3,
        "Y pixel mismatch: expected 81, got {} (diff {})",
        dy,
        y_diff
    );
    assert!(
        u_diff <= 3,
        "U pixel mismatch: expected 90, got {} (diff {})",
        du,
        u_diff
    );
    assert!(
        v_diff <= 3,
        "V pixel mismatch: expected 240, got {} (diff {})",
        dv,
        v_diff
    );
}

#[test]
#[ignore]
fn avmdec_decodes_256x192_smptebars() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let ffmpeg = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "smptebars=s=256x192",
            "-frames:v",
            "1",
            "-pix_fmt",
            "yuv420p",
            "/tmp/av2codec_smptebars_256x192.yuv",
        ])
        .output()
        .expect("ffmpeg not found");
    assert!(ffmpeg.status.success(), "ffmpeg failed");

    let raw = fs::read("/tmp/av2codec_smptebars_256x192.yuv").unwrap();
    let w = 256usize;
    let h = 192usize;
    let y_size = w * h;
    let uv_w = w / 2;
    let uv_h = h / 2;
    let uv_size = uv_w * uv_h;
    assert_eq!(raw.len(), y_size + 2 * uv_size);

    let y_plane = raw[..y_size].to_vec();
    let u_plane = raw[y_size..y_size + uv_size].to_vec();
    let v_plane = raw[y_size + uv_size..].to_vec();
    let bs = av2codec::encode::encode_video_yuv(
        w as u32,
        h as u32,
        50,
        &[(y_plane.clone(), u_plane.clone(), v_plane.clone())],
    );
    fs::write("/tmp/av2codec_smptebars_256x192.obu", &bs).unwrap();

    let decoded = decode_with_avmdec(
        "/tmp/av2codec_smptebars_256x192.obu",
        "/tmp/av2codec_smptebars_256x192_out.yuv",
    );
    assert_eq!(decoded.len(), y_size + 2 * uv_size);

    let y_psnr = psnr(&y_plane, &decoded[..y_size]);
    let u_psnr = psnr(&u_plane, &decoded[y_size..y_size + uv_size]);
    let v_psnr = psnr(&v_plane, &decoded[y_size + uv_size..]);

    eprintln!(
        "SMPTE bars 256x192 PSNR: Y={:.1}dB U={:.1}dB V={:.1}dB",
        y_psnr, u_psnr, v_psnr
    );
    assert!(y_psnr > 8.0, "Y PSNR too low: {:.1}dB", y_psnr);
    assert!(u_psnr > 8.0, "U PSNR too low: {:.1}dB", u_psnr);
    assert!(v_psnr > 8.0, "V PSNR too low: {:.1}dB", v_psnr);
}

#[test]
#[ignore]
fn avmdec_decodes_256x192_testsrc2() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let ffmpeg = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=256x192",
            "-frames:v",
            "1",
            "-pix_fmt",
            "yuv420p",
            "/tmp/av2codec_testsrc2_256x192.yuv",
        ])
        .output()
        .expect("ffmpeg not found");
    assert!(ffmpeg.status.success(), "ffmpeg failed");

    let raw = fs::read("/tmp/av2codec_testsrc2_256x192.yuv").unwrap();
    let w = 256usize;
    let h = 192usize;
    let y_size = w * h;
    let uv_w = w / 2;
    let uv_h = h / 2;
    let uv_size = uv_w * uv_h;
    assert_eq!(raw.len(), y_size + 2 * uv_size);

    let y_plane = raw[..y_size].to_vec();
    let u_plane = raw[y_size..y_size + uv_size].to_vec();
    let v_plane = raw[y_size + uv_size..].to_vec();
    let bs = av2codec::encode::encode_video_yuv(
        w as u32,
        h as u32,
        50,
        &[(y_plane.clone(), u_plane.clone(), v_plane.clone())],
    );
    fs::write("/tmp/av2codec_testsrc2_256x192.obu", &bs).unwrap();

    let decoded = decode_with_avmdec(
        "/tmp/av2codec_testsrc2_256x192.obu",
        "/tmp/av2codec_testsrc2_256x192_out.yuv",
    );
    assert_eq!(decoded.len(), y_size + 2 * uv_size);

    let y_psnr = psnr(&y_plane, &decoded[..y_size]);
    let u_psnr = psnr(&u_plane, &decoded[y_size..y_size + uv_size]);
    let v_psnr = psnr(&v_plane, &decoded[y_size + uv_size..]);

    eprintln!(
        "Testsrc2 256x192 PSNR: Y={:.1}dB U={:.1}dB V={:.1}dB",
        y_psnr, u_psnr, v_psnr
    );
    assert!(y_psnr > 8.0, "Y PSNR too low: {:.1}dB", y_psnr);
    assert!(u_psnr > 8.0, "U PSNR too low: {:.1}dB", u_psnr);
    assert!(v_psnr > 8.0, "V PSNR too low: {:.1}dB", v_psnr);
}

#[test]
#[ignore]
fn avmdec_testsrc2_psnr() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let mut reader = av2codec::y4m::Y4mReader::open("tests/fixtures/testsrc2_64x64.y4m").unwrap();
    let frame = reader.next_frame().unwrap();
    let original_y = frame.y.clone();
    let original_u = frame.u.clone();
    let original_v = frame.v.clone();

    let bs = av2codec::encode::encode_video_yuv(64, 64, 50, &[(frame.y, frame.u, frame.v)]);
    fs::write("/tmp/av2codec_test_testsrc2_psnr.obu", &bs).unwrap();

    let decoded = decode_with_avmdec(
        "/tmp/av2codec_test_testsrc2_psnr.obu",
        "/tmp/av2codec_test_testsrc2_psnr_out.yuv",
    );

    let y_size = 64 * 64;
    let uv_size = 32 * 32;
    let decoded_y = &decoded[..y_size];
    let decoded_u = &decoded[y_size..y_size + uv_size];
    let decoded_v = &decoded[y_size + uv_size..];

    let y_psnr = psnr(&original_y, decoded_y);
    let u_psnr = psnr(&original_u, decoded_u);
    let v_psnr = psnr(&original_v, decoded_v);

    eprintln!(
        "Testsrc2 PSNR: Y={:.1}dB U={:.1}dB V={:.1}dB",
        y_psnr, u_psnr, v_psnr
    );

    assert!(
        y_psnr > 20.0,
        "Y PSNR should be > 20dB for testsrc2 at qindex=50, got {:.1}dB",
        y_psnr
    );
}

#[test]
#[ignore]
fn avmdec_av2codec_decoder_agreement_multi_sb() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let ffmpeg = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=256x192",
            "-frames:v",
            "1",
            "-pix_fmt",
            "yuv420p",
            "/tmp/av2codec_regression_testsrc2.yuv",
        ])
        .output()
        .expect("ffmpeg not found");
    assert!(ffmpeg.status.success(), "ffmpeg failed");

    let raw = fs::read("/tmp/av2codec_regression_testsrc2.yuv").unwrap();
    let w = 256usize;
    let h = 192usize;
    let y_size = w * h;
    let uv_size = (w / 2) * (h / 2);
    let y_plane = raw[..y_size].to_vec();
    let u_plane = raw[y_size..y_size + uv_size].to_vec();
    let v_plane = raw[y_size + uv_size..].to_vec();

    let bs = av2codec::encode::encode_video_yuv(
        w as u32,
        h as u32,
        50,
        &[(y_plane.clone(), u_plane.clone(), v_plane.clone())],
    );
    fs::write("/tmp/av2codec_regression_testsrc2.obu", &bs).unwrap();

    let avmdec_raw = decode_with_avmdec(
        "/tmp/av2codec_regression_testsrc2.obu",
        "/tmp/av2codec_regression_testsrc2_avmdec.yuv",
    );

    let av2codec_frames = av2codec::Decoder::decode_all(&bs).expect("av2codec decoder failed");
    assert_eq!(av2codec_frames.len(), 1);
    let av2codec_y = &av2codec_frames[0].y;
    let av2codec_u = &av2codec_frames[0].u;
    let av2codec_v = &av2codec_frames[0].v;

    let avmdec_y = &avmdec_raw[..y_size];
    let avmdec_u = &avmdec_raw[y_size..y_size + uv_size];
    let avmdec_v = &avmdec_raw[y_size + uv_size..];

    let y_psnr_avmdec = psnr(&y_plane, avmdec_y);
    let y_psnr_av2codec = psnr(&y_plane, av2codec_y);
    let decoder_y_psnr = psnr(avmdec_y, av2codec_y);
    let decoder_u_psnr = psnr(avmdec_u, av2codec_u);
    let decoder_v_psnr = psnr(avmdec_v, av2codec_v);

    eprintln!(
        "Regression test 256x192 testsrc2:\n  avmdec vs original Y PSNR: {:.1}dB\n  av2codec  vs original Y PSNR: {:.1}dB\n  avmdec vs av2codec Y PSNR: {:.1}dB  U: {:.1}dB  V: {:.1}dB",
        y_psnr_avmdec, y_psnr_av2codec, decoder_y_psnr, decoder_u_psnr, decoder_v_psnr
    );

    assert!(
        y_psnr_avmdec > 15.0,
        "avmdec Y PSNR vs original too low: {:.1}dB (regression in encoder spec compliance)",
        y_psnr_avmdec
    );
    assert!(
        decoder_y_psnr > 20.0,
        "avmdec vs av2codec Y PSNR too low: {:.1}dB (decoders diverging, possible spec violation)",
        decoder_y_psnr
    );
    assert!(
        decoder_u_psnr > 20.0,
        "avmdec vs av2codec U PSNR too low: {:.1}dB",
        decoder_u_psnr
    );
    assert!(
        decoder_v_psnr > 20.0,
        "avmdec vs av2codec V PSNR too low: {:.1}dB",
        decoder_v_psnr
    );
}

#[test]
#[ignore]
fn avmdec_av2codec_decoder_agreement_solid_colors() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let colors: [(u8, u8, u8, &str); 3] = [
        (149, 43, 21, "green"),
        (81, 90, 240, "red"),
        (200, 100, 150, "mixed"),
    ];

    for &(y_val, u_val, v_val, name) in &colors {
        let y_plane = vec![y_val; 128 * 128];
        let u_plane = vec![u_val; 64 * 64];
        let v_plane = vec![v_val; 64 * 64];
        let bs = av2codec::encode::encode_video_yuv(128, 128, 50, &[(y_plane, u_plane, v_plane)]);

        let obu_path = format!("/tmp/av2codec_regression_{}.obu", name);
        let yuv_path = format!("/tmp/av2codec_regression_{}_avmdec.yuv", name);
        fs::write(&obu_path, &bs).unwrap();

        let avmdec_raw = decode_with_avmdec(&obu_path, &yuv_path);

        let av2codec_frames = av2codec::Decoder::decode_all(&bs).unwrap();
        let av2codec_y = &av2codec_frames[0].y;
        let av2codec_u = &av2codec_frames[0].u;
        let av2codec_v = &av2codec_frames[0].v;

        let y_size = 128 * 128;
        let uv_size = 64 * 64;
        let avmdec_y = &avmdec_raw[..y_size];
        let avmdec_u = &avmdec_raw[y_size..y_size + uv_size];
        let avmdec_v = &avmdec_raw[y_size + uv_size..];

        for (plane_name, avmdec_plane, av2codec_plane, expected, size) in [
            ("Y", avmdec_y, av2codec_y.as_slice(), y_val, y_size),
            ("U", avmdec_u, av2codec_u.as_slice(), u_val, uv_size),
            ("V", avmdec_v, av2codec_v.as_slice(), v_val, uv_size),
        ] {
            for i in 0..size {
                let avmdec_diff = (avmdec_plane[i] as i32 - expected as i32).abs();
                let av2codec_diff = (av2codec_plane[i] as i32 - expected as i32).abs();
                assert!(
                    avmdec_diff <= 3,
                    "{} {} pixel {} avmdec={} expected={} diff={}",
                    name,
                    plane_name,
                    i,
                    avmdec_plane[i],
                    expected,
                    avmdec_diff
                );
                assert!(
                    av2codec_diff <= 1,
                    "{} {} pixel {} av2codec={} expected={} diff={}",
                    name,
                    plane_name,
                    i,
                    av2codec_plane[i],
                    expected,
                    av2codec_diff
                );
            }
        }
        eprintln!(
            "  {} 128x128: avmdec and av2codec agree within tolerance",
            name
        );
    }
}

#[test]
#[ignore]
fn avmdec_decodes_100x100_solid_red() {
    if !avmdec_available() {
        eprintln!("avmdec not found, skipping");
        return;
    }

    let w = 100u32;
    let h = 100u32;
    let y_plane = vec![82u8; (w * h) as usize];
    let u_plane = vec![90u8; (w / 2 * h / 2) as usize];
    let v_plane = vec![240u8; (w / 2 * h / 2) as usize];

    let bs = av2codec::encode::encode_video_yuv(w, h, 50, &[(y_plane, u_plane, v_plane)]);
    fs::write("/tmp/av2codec_test_100x100.obu", &bs).unwrap();

    let decoded = decode_with_avmdec(
        "/tmp/av2codec_test_100x100.obu",
        "/tmp/av2codec_test_100x100_out.yuv",
    );

    let pw = 128usize;
    let ph = 128usize;
    let frame_size = pw * ph + (pw / 2) * (ph / 2) * 2;
    assert_eq!(
        decoded.len(),
        frame_size,
        "Expected padded 128x128 I420 frame"
    );

    for row in 0..100 {
        for col in 0..100 {
            let pixel = decoded[row * pw + col];
            let diff = (pixel as i32 - 82).abs();
            assert!(
                diff < 10,
                "Y pixel ({},{}) = {}, expected ~82",
                row,
                col,
                pixel
            );
        }
    }
}
