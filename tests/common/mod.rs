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

pub fn avmdec_available() -> bool {
    avmdec_path().is_some()
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
