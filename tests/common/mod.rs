#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub fn avmdec_path() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("AVMDEC") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }

    let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../avm_build/avmdec");
    if local.is_file() {
        return Some(local);
    }

    let temp_build = PathBuf::from("/tmp/avm/build/avmdec");
    if temp_build.is_file() {
        return Some(temp_build);
    }

    if Command::new("avmdec").arg("--help").output().is_ok() {
        return Some(PathBuf::from("avmdec"));
    }

    None
}

pub fn dav2d_path() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("DAV2D") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }

    let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dav2d/build/tools/dav2d");
    if local.is_file() {
        return Some(local);
    }

    let temp_build = PathBuf::from("/tmp/dav2d/build/tools/dav2d");
    if temp_build.is_file() {
        return Some(temp_build);
    }

    if Command::new("dav2d").arg("--version").output().is_ok() {
        return Some(PathBuf::from("dav2d"));
    }

    None
}

pub fn dav2d_available() -> bool {
    dav2d_path().is_some()
}

pub fn decode_with_dav2d(obu_path: &str, out_path: &str) -> Vec<u8> {
    let dav2d = dav2d_path().expect("dav2d not found");
    let output = Command::new(dav2d)
        .args(["-i", obu_path, "-o", out_path, "--muxer", "yuv"])
        .output()
        .expect("failed to run dav2d");

    assert!(
        output.status.success(),
        "dav2d failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    fs::read(out_path).unwrap()
}

pub fn avmdec_available() -> bool {
    avmdec_path().is_some()
}

pub fn decode_with_dav2d_demuxer(obu_path: &str, out_path: &str, demuxer: &str) -> Vec<u8> {
    let dav2d = dav2d_path().expect("dav2d not found");
    let output = Command::new(dav2d)
        .args([
            "--demuxer", demuxer, "-i", obu_path, "-o", out_path, "--muxer", "yuv",
        ])
        .output()
        .expect("failed to run dav2d");

    assert!(
        output.status.success(),
        "dav2d failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    fs::read(out_path).unwrap()
}

pub fn decode_with_avmdec(obu_path: &str, out_path: &str) -> Vec<u8> {
    let avmdec = avmdec_path().expect("avmdec not found");
    let output = Command::new(avmdec)
        .args(["--codec=av2", "--i420", "-o", out_path, obu_path])
        .output()
        .expect("failed to run avmdec");

    assert!(
        output.status.success(),
        "avmdec failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    fs::read(out_path).unwrap()
}
