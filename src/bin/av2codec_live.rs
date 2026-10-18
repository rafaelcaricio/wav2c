use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use av2codec::multiview::{
    AtlasLayout, MultiviewConfig, MultiviewView, PURPOSE_MULTIVIEW_PLAYBACK,
    encode_multiview_with_threads,
};
use av2codec::y4m::Y4mReader;
use av2codec::{DecodedFrame, Decoder};

const VIEW_WIDTH: u32 = 640;
const VIEW_HEIGHT: u32 = 384;
const COLUMNS: u8 = 2;
const VIEW_COUNT: usize = 4;
const CANVAS_WIDTH: u32 = VIEW_WIDTH * 2;
const CANVAS_HEIGHT: u32 = VIEW_HEIGHT * 2;
const PAGE: &str = include_str!("live_page.html");
const FAVICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="6" fill="#0b0f0d"/><path d="M6 24 16 7l10 17h-5l-5-9-5 9z" fill="#c6ff3d"/></svg>"##;

type Frame = (Vec<u8>, Vec<u8>, Vec<u8>);

struct CameraFrame {
    sequence: u64,
    frame: Frame,
}

#[derive(Clone, Copy, Default)]
struct Stats {
    fps: f32,
    encode_ms: f32,
    decode_ms: f32,
    compose_ms: f32,
    tu_bytes: usize,
    camera_frames: u64,
    camera_connected: bool,
    frame_index: u64,
}

struct JpegSlot {
    version: u64,
    data: Vec<u8>,
}

struct Shared {
    camera: Mutex<Option<CameraFrame>>,
    stats: Mutex<Stats>,
    jpeg: Mutex<JpegSlot>,
    jpeg_ready: Condvar,
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn main() {
    let mut port: u16 = 8090;
    let mut fps: u32 = 15;
    let mut qindex: u8 = 150;
    let mut background_override: Option<PathBuf> = None;

    let arguments: Vec<String> = std::env::args().collect();
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--port" => {
                index += 1;
                port = arguments.get(index).and_then(|v| v.parse().ok()).unwrap_or(8090);
            }
            "--fps" => {
                index += 1;
                fps = arguments.get(index).and_then(|v| v.parse().ok()).unwrap_or(15);
            }
            "--qindex" => {
                index += 1;
                qindex = arguments.get(index).and_then(|v| v.parse().ok()).unwrap_or(150);
            }
            "--background-dir" => {
                index += 1;
                background_override = arguments.get(index).map(PathBuf::from);
            }
            "--help" | "-h" => {
                println!(
                    "Usage: av2codec_live [--port N] [--fps N] [--qindex N] [--background-dir DIR]\n\
                     \n\
                     Serves a camera multiview demo at http://localhost:<port>/.\n\
                     The camera feed becomes extended layer 3 of a 2x2 AV2 multistream,\n\
                     next to three looping background clips (sports views when available)."
                );
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(1);
            }
        }
        index += 1;
    }

    let backgrounds = load_backgrounds(background_override.as_deref());
    if backgrounds.len() < VIEW_COUNT - 1 {
        eprintln!(
            "need {} background clips, found {}",
            VIEW_COUNT - 1,
            backgrounds.len()
        );
        std::process::exit(1);
    }

    let shared = Arc::new(Shared {
        camera: Mutex::new(None),
        stats: Mutex::new(Stats::default()),
        jpeg: Mutex::new(JpegSlot {
            version: 0,
            data: Vec::new(),
        }),
        jpeg_ready: Condvar::new(),
    });

    let encoder_shared = Arc::clone(&shared);
    thread::spawn(move || encode_loop(encoder_shared, backgrounds, fps, qindex));

    let address = format!("0.0.0.0:{port}");
    let listener = TcpListener::bind(&address).unwrap_or_else(|error| {
        eprintln!("cannot bind {address}: {error}");
        std::process::exit(1);
    });
    println!("AV2 live multiview on http://localhost:{port}/  (camera: layer 3, canvas {CANVAS_WIDTH}x{CANVAS_HEIGHT})");
    for connection in listener.incoming() {
        match connection {
            Ok(stream) => {
                let connection_shared = Arc::clone(&shared);
                thread::spawn(move || handle_connection(stream, connection_shared));
            }
            Err(error) => eprintln!("connection error: {error}"),
        }
    }
}

fn handle_connection(mut stream: TcpStream, shared: Arc<Shared>) {
    let _ = stream.set_nodelay(true);
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let path = request
        .path
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    match (request.method.as_str(), path.as_str()) {
        ("GET", "/") => respond(&mut stream, "200 OK", "text/html; charset=utf-8", PAGE.as_bytes()),
        ("GET", "/favicon.svg") => {
            respond(&mut stream, "200 OK", "image/svg+xml", FAVICON.as_bytes())
        }
        ("GET", "/stats") => {
            let body = stats_json(&shared);
            respond(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                body.as_bytes(),
            );
        }
        ("POST", "/frame") => {
            if request.body.len() != (VIEW_WIDTH * VIEW_HEIGHT * 4) as usize {
                respond(&mut stream, "400 Bad Request", "text/plain", b"bad frame size");
                return;
            }
            let frame = rgba_to_yuv420(&request.body, VIEW_WIDTH as usize, VIEW_HEIGHT as usize);
            let mut camera = shared.camera.lock().unwrap();
            let sequence = camera.as_ref().map_or(1, |current| current.sequence + 1);
            *camera = Some(CameraFrame { sequence, frame });
            drop(camera);
            let mut stats = shared.stats.lock().unwrap();
            stats.camera_frames += 1;
            stats.camera_connected = true;
            drop(stats);
            respond(&mut stream, "204 No Content", "text/plain", b"");
        }
        ("GET", "/canvas.mjpg") => stream_mjpeg(stream, shared),
        _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_bytes(&buffer, b"\r\n\r\n") {
            break position;
        }
        if buffer.len() > 64 * 1024 {
            return None;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = headers.lines();
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_string();
    let path = request_line.next()?.to_string();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Some(Request { method, path, body })
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn stream_mjpeg(mut stream: TcpStream, shared: Arc<Shared>) {
    let header = "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
    if stream.write_all(header.as_bytes()).is_err() {
        return;
    }
    let mut version = 0u64;
    loop {
        let data = {
            let mut slot = shared.jpeg.lock().unwrap();
            while slot.version == version {
                slot = shared.jpeg_ready.wait(slot).unwrap();
            }
            version = slot.version;
            slot.data.clone()
        };
        if data.is_empty() {
            continue;
        }
        let part = format!(
            "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
            data.len()
        );
        if stream.write_all(part.as_bytes()).is_err() {
            return;
        }
        if stream.write_all(&data).is_err() || stream.write_all(b"\r\n").is_err() {
            return;
        }
        let _ = stream.flush();
    }
}

fn stats_json(shared: &Shared) -> String {
    let stats = shared.stats.lock().unwrap();
    let camera = shared.camera.lock().unwrap();
    let sequence = camera.as_ref().map_or(0, |frame| frame.sequence);
    let kbps = stats.tu_bytes as f32 * stats.fps * 8.0 / 1000.0;
    format!(
        "{{\"fps\":{:.1},\"encode_ms\":{:.2},\"decode_ms\":{:.2},\"compose_ms\":{:.2},\"tu_bytes\":{},\"kbps\":{:.0},\"camera_frames\":{},\"camera_sequence\":{},\"camera_connected\":{},\"frame_index\":{},\"views\":{},\"canvas\":\"{}x{}\"}}",
        stats.fps,
        stats.encode_ms,
        stats.decode_ms,
        stats.compose_ms,
        stats.tu_bytes,
        kbps,
        stats.camera_frames,
        sequence,
        stats.camera_connected,
        stats.frame_index,
        VIEW_COUNT,
        CANVAS_WIDTH,
        CANVAS_HEIGHT
    )
}

fn encode_loop(shared: Arc<Shared>, backgrounds: Vec<Vec<Frame>>, fps: u32, qindex: u8) {
    let mut views: Vec<MultiviewView> = (0..VIEW_COUNT)
        .map(|index| {
            MultiviewView::explicit_view(
                VIEW_WIDTH,
                VIEW_HEIGHT,
                index as u8,
                vec![placeholder_frame()],
            )
        })
        .collect();

    let (mut ffmpeg_stdin, ffmpeg_stdout) = spawn_mjpeg(fps);
    let jpeg_shared = Arc::clone(&shared);
    thread::spawn(move || jpeg_pump(ffmpeg_stdout, jpeg_shared));

    let budget = Duration::from_secs_f64(1.0 / fps as f64);
    let mut frame_index = 0u64;
    loop {
        let started = Instant::now();

        for (index, clip) in backgrounds.iter().enumerate().take(VIEW_COUNT - 1) {
            views[index].frames[0] = clip[(frame_index as usize) % clip.len()].clone();
        }
        {
            let camera = shared.camera.lock().unwrap();
            views[VIEW_COUNT - 1].frames[0] = camera
                .as_ref()
                .map(|current| current.frame.clone())
                .unwrap_or_else(placeholder_frame);
        }

        let config = MultiviewConfig {
            base_qindex: qindex,
            purpose_id: PURPOSE_MULTIVIEW_PLAYBACK,
            layout: AtlasLayout::Grid { columns: COLUMNS },
            views: &views,
        };

        let encode_started = Instant::now();
        let temporal_unit = match encode_multiview_with_threads(&config, VIEW_COUNT) {
            Ok(unit) => unit,
            Err(error) => {
                eprintln!("encode failed: {error}");
                thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        let encode_ms = encode_started.elapsed().as_secs_f32() * 1000.0;

        let decode_started = Instant::now();
        let frames = match Decoder::decode_all(&temporal_unit) {
            Ok(frames) if frames.len() == VIEW_COUNT => frames,
            Ok(frames) => {
                eprintln!("decoded {} frames, expected {VIEW_COUNT}", frames.len());
                thread::sleep(Duration::from_millis(200));
                continue;
            }
            Err(error) => {
                eprintln!("decode failed: {error}");
                thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        let decode_ms = decode_started.elapsed().as_secs_f32() * 1000.0;

        let compose_started = Instant::now();
        let (canvas_y, canvas_u, canvas_v) = compose_canvas(&frames);
        let compose_ms = compose_started.elapsed().as_secs_f32() * 1000.0;

        if ffmpeg_stdin
            .write_all(&canvas_y)
            .and_then(|_| ffmpeg_stdin.write_all(&canvas_u))
            .and_then(|_| ffmpeg_stdin.write_all(&canvas_v))
            .is_err()
        {
            eprintln!("mjpeg encoder pipe closed");
            return;
        }
        let _ = ffmpeg_stdin.flush();

        {
            let mut stats = shared.stats.lock().unwrap();
            stats.encode_ms = encode_ms;
            stats.decode_ms = decode_ms;
            stats.compose_ms = compose_ms;
            stats.tu_bytes = temporal_unit.len();
            stats.frame_index = frame_index;
        }
        frame_index += 1;

        let remaining = budget.saturating_sub(started.elapsed());
        if !remaining.is_zero() {
            thread::sleep(remaining);
        }

        let period = started.elapsed().as_secs_f32();
        let mut stats = shared.stats.lock().unwrap();
        let instantaneous = 1.0 / period.max(0.001);
        stats.fps = if stats.fps == 0.0 {
            instantaneous
        } else {
            stats.fps * 0.9 + instantaneous * 0.1
        };
    }
}

fn spawn_mjpeg(fps: u32) -> (ChildStdin, ChildStdout) {
    #[allow(clippy::zombie_processes)]
    let mut child = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{CANVAS_WIDTH}x{CANVAS_HEIGHT}"),
            "-r",
            &fps.to_string(),
            "-i",
            "pipe:0",
            "-c:v",
            "mjpeg",
            "-q:v",
            "5",
            "-f",
            "mjpeg",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("ffmpeg is required for the MJPEG preview stream");
    let stdin = child.stdin.take().expect("ffmpeg stdin");
    let stdout = child.stdout.take().expect("ffmpeg stdout");
    (stdin, stdout)
}

fn jpeg_pump(mut stdout: ChildStdout, shared: Arc<Shared>) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let read = match stdout.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        buffer.extend_from_slice(&chunk[..read]);
        loop {
            let Some(start) = find_bytes(&buffer, &[0xFF, 0xD8]) else {
                if buffer.len() > 32 {
                    buffer.drain(..buffer.len() - 1);
                }
                break;
            };
            if start > 0 {
                buffer.drain(..start);
            }
            let Some(end) = find_bytes(&buffer[2..], &[0xFF, 0xD9]) else {
                break;
            };
            let jpeg = buffer[..end + 4].to_vec();
            buffer.drain(..end + 4);
            if jpeg.len() < 1000 {
                continue;
            }
            let mut slot = shared.jpeg.lock().unwrap();
            slot.version += 1;
            slot.data = jpeg;
            drop(slot);
            shared.jpeg_ready.notify_all();
        }
    }
}

fn compose_canvas(frames: &[DecodedFrame]) -> Frame {
    let width = CANVAS_WIDTH as usize;
    let height = CANVAS_HEIGHT as usize;
    let mut canvas_y = vec![0u8; width * height];
    let mut canvas_u = vec![0u8; (width / 2) * (height / 2)];
    let mut canvas_v = vec![0u8; (width / 2) * (height / 2)];
    for (index, frame) in frames.iter().enumerate() {
        let column = (index % COLUMNS as usize) as u32;
        let row = (index / COLUMNS as usize) as u32;
        let left = column * VIEW_WIDTH;
        let top = row * VIEW_HEIGHT;
        let copy_width = VIEW_WIDTH.min(frame.width) as usize;
        let copy_height = VIEW_HEIGHT.min(frame.height) as usize;
        for line in 0..copy_height {
            let source = line * frame.width as usize;
            let target = (top as usize + line) * width + left as usize;
            canvas_y[target..target + copy_width]
                .copy_from_slice(&frame.y[source..source + copy_width]);
        }
        let chroma_width = copy_width / 2;
        let chroma_height = copy_height / 2;
        let frame_chroma_width = (frame.width / 2) as usize;
        for line in 0..chroma_height {
            let source = line * frame_chroma_width;
            let target = (top as usize / 2 + line) * (width / 2) + left as usize / 2;
            canvas_u[target..target + chroma_width]
                .copy_from_slice(&frame.u[source..source + chroma_width]);
            canvas_v[target..target + chroma_width]
                .copy_from_slice(&frame.v[source..source + chroma_width]);
        }
    }
    (canvas_y, canvas_u, canvas_v)
}

fn rgba_to_yuv420(rgba: &[u8], width: usize, height: usize) -> Frame {
    let mut y_plane = vec![0u8; width * height];
    let mut u_plane = vec![0u8; (width / 2) * (height / 2)];
    let mut v_plane = vec![0u8; (width / 2) * (height / 2)];
    for row in 0..height {
        for column in 0..width {
            let offset = (row * width + column) * 4;
            let red = rgba[offset] as i32;
            let green = rgba[offset + 1] as i32;
            let blue = rgba[offset + 2] as i32;
            let luma = ((66 * red + 129 * green + 25 * blue + 128) >> 8) + 16;
            y_plane[row * width + column] = luma.clamp(0, 255) as u8;
        }
    }
    for row in (0..height).step_by(2) {
        for column in (0..width).step_by(2) {
            let mut red = 0i32;
            let mut green = 0i32;
            let mut blue = 0i32;
            for offset_y in 0..2 {
                for offset_x in 0..2 {
                    let offset = ((row + offset_y) * width + column + offset_x) * 4;
                    red += rgba[offset] as i32;
                    green += rgba[offset + 1] as i32;
                    blue += rgba[offset + 2] as i32;
                }
            }
            let red = red / 4;
            let green = green / 4;
            let blue = blue / 4;
            let u = ((-38 * red - 74 * green + 112 * blue + 128) >> 8) + 128;
            let v = ((112 * red - 94 * green - 18 * blue + 128) >> 8) + 128;
            let index = (row / 2) * (width / 2) + column / 2;
            u_plane[index] = u.clamp(0, 255) as u8;
            v_plane[index] = v.clamp(0, 255) as u8;
        }
    }
    (y_plane, u_plane, v_plane)
}

fn placeholder_frame() -> Frame {
    let width = VIEW_WIDTH as usize;
    let height = VIEW_HEIGHT as usize;
    let mut y_plane = vec![0u8; width * height];
    for row in 0..height {
        for column in 0..width {
            let cell = ((column / 48) + (row / 48)) % 2;
            y_plane[row * width + column] = if cell == 0 { 24 } else { 32 };
        }
    }
    let mut u_plane = vec![116u8; (width / 2) * (height / 2)];
    let mut v_plane = vec![140u8; (width / 2) * (height / 2)];
    for (index, value) in u_plane.iter_mut().enumerate() {
        if index % 97 == 0 {
            *value = 126;
        }
    }
    for (index, value) in v_plane.iter_mut().enumerate() {
        if index % 89 == 0 {
            *value = 130;
        }
    }
    (y_plane, u_plane, v_plane)
}

fn load_backgrounds(override_dir: Option<&Path>) -> Vec<Vec<Frame>> {
    let sports_dir = PathBuf::from("target/demo/sports/clips");
    let sports_names = ["soccer", "basketball", "boxing"];
    let mut clips = Vec::new();
    for name in sports_names {
        let path = sports_dir.join(format!("view_{name}.y4m"));
        if let Some(frames) = read_clip(&path) {
            clips.push(frames);
        }
    }
    if clips.len() == VIEW_COUNT - 1 {
        return clips;
    }

    clips.clear();
    let directory = override_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("target/demo/live"));
    let _ = std::fs::create_dir_all(&directory);
    let synthetic = [
        ("testsrc", "testsrc2=size=640x384:rate=15"),
        ("mandel", "mandelbrot=size=640x384:rate=15"),
        ("bars", "smptebars=size=640x384:rate=15"),
    ];
    for (name, filter) in synthetic {
        let path = directory.join(format!("bg_{name}.y4m"));
        if !path.is_file() {
            let _ = Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    filter,
                    "-frames:v",
                    "90",
                    "-pix_fmt",
                    "yuv420p",
                ])
                .arg(&path)
                .status();
        }
        if let Some(frames) = read_clip(&path) {
            clips.push(frames);
        }
    }
    clips
}

fn read_clip(path: &Path) -> Option<Vec<Frame>> {
    let mut reader = Y4mReader::open(path.to_str()?).ok()?;
    let width = reader.header().width;
    let height = reader.header().height;
    if width != VIEW_WIDTH || height != VIEW_HEIGHT {
        eprintln!(
            "background {} is {}x{}, expected {}x{}",
            path.display(),
            width,
            height,
            VIEW_WIDTH,
            VIEW_HEIGHT
        );
        return None;
    }
    let mut frames = Vec::new();
    while let Some(frame) = reader.next_frame() {
        frames.push((frame.y, frame.u, frame.v));
        if frames.len() >= 150 {
            break;
        }
    }
    if frames.is_empty() {
        return None;
    }
    println!("background {}: {} frames", path.display(), frames.len());
    Some(frames)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_frame(value: u8) -> DecodedFrame {
        let luma = (VIEW_WIDTH * VIEW_HEIGHT) as usize;
        let chroma = (VIEW_WIDTH / 2 * VIEW_HEIGHT / 2) as usize;
        DecodedFrame {
            y: vec![value; luma],
            u: vec![value; chroma],
            v: vec![value; chroma],
            width: VIEW_WIDTH,
            height: VIEW_HEIGHT,
            frame_number: 0,
        }
    }

    #[test]
    fn rgba_to_yuv420_maps_red_to_limited_range_yuv() {
        let width = 8;
        let height = 4;
        let mut rgba = vec![0u8; width * height * 4];
        for pixel in rgba.chunks_mut(4) {
            pixel[0] = 255;
            pixel[3] = 255;
        }
        let (y_plane, u_plane, v_plane) = rgba_to_yuv420(&rgba, width, height);
        assert_eq!(y_plane.len(), width * height);
        assert_eq!(u_plane.len(), (width / 2) * (height / 2));
        assert!(y_plane.iter().all(|&value| (80..=84).contains(&value)));
        assert!(u_plane.iter().all(|&value| (88..=92).contains(&value)));
        assert!(v_plane.iter().all(|&value| value >= 238));
    }

    #[test]
    fn compose_canvas_places_views_in_row_major_order() {
        let frames = vec![
            solid_frame(10),
            solid_frame(20),
            solid_frame(30),
            solid_frame(40),
        ];
        let (y_plane, u_plane, v_plane) = compose_canvas(&frames);
        assert_eq!(y_plane.len(), (CANVAS_WIDTH * CANVAS_HEIGHT) as usize);
        assert_eq!(u_plane.len(), (CANVAS_WIDTH / 2 * CANVAS_HEIGHT / 2) as usize);
        assert_eq!(v_plane.len(), u_plane.len());
        let width = CANVAS_WIDTH as usize;
        assert_eq!(y_plane[0], 10);
        assert_eq!(y_plane[VIEW_WIDTH as usize], 20);
        assert_eq!(y_plane[width * VIEW_HEIGHT as usize], 30);
        assert_eq!(y_plane[width * VIEW_HEIGHT as usize + VIEW_WIDTH as usize], 40);
        assert_eq!(u_plane[0], 10);
        assert_eq!(u_plane[(width / 2) * (VIEW_HEIGHT / 2) as usize], 30);
    }

    #[test]
    fn find_bytes_locates_jpeg_markers() {
        assert_eq!(find_bytes(b"xx\xff\xd8yy", &[0xFF, 0xD8]), Some(2));
        assert_eq!(find_bytes(b"abc", &[0xFF, 0xD8]), None);
        assert_eq!(find_bytes(b"", &[0xFF]), None);
    }
}
