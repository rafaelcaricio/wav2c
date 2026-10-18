use std::fs;
use std::process;
use av2codec::encode;
use av2codec::multiview::{
    AtlasLayout, MultiviewConfig, MultiviewView, PURPOSE_MULTIVIEW_PLAYBACK,
    PURPOSE_STEREOSCOPIC_VIEWPORTS, ViewFrame, encode_multiview, extract_layer,
    stereo_view_types,
};
use av2codec::y4m::Y4mReader;

fn usage() -> ! {
    eprintln!(
        "Usage: av2codec <input.y4m> [<view.y4m> ...] <output.obu> [options]\n\
         \n\
         Options:\n\
           --qindex N          quantization index (0-255, default 50)\n\
           --multiview         encode all inputs as extended layers of one multiview stream\n\
           --layout row|column atlas grid direction for multiview (default row)\n\
           --purpose stereo|multiview  global LCR purpose id (default by view count)\n\
           --extract-layer N   write only extended layer N of the encoded stream"
    );
    process::exit(1);
}

fn read_frames(path: &str) -> (u32, u32, Vec<ViewFrame>) {
    let mut reader = Y4mReader::open(path).unwrap_or_else(|e| {
        eprintln!("Error opening {}: {}", path, e);
        process::exit(1);
    });
    let width = reader.header().width;
    let height = reader.header().height;
    let mut frames = Vec::new();
    while let Some(frame) = reader.next_frame() {
        frames.push((frame.y, frame.u, frame.v));
    }
    if frames.is_empty() {
        eprintln!("Error: no frames found in {}", path);
        process::exit(1);
    }
    (width, height, frames)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut positional = Vec::new();
    let mut base_qindex: u8 = 50;
    let mut multiview = false;
    let mut layout = AtlasLayout::Row;
    let mut purpose: Option<u8> = None;
    let mut extract: Option<u8> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--qindex" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --qindex requires a value (0-255)");
                    process::exit(1);
                }
                base_qindex = args[i].parse().unwrap_or_else(|_| {
                    eprintln!("Error: invalid qindex value: {}", args[i]);
                    process::exit(1);
                });
            }
            "--multiview" => multiview = true,
            "--layout" => {
                i += 1;
                layout = match args.get(i).map(String::as_str) {
                    Some("row") => AtlasLayout::Row,
                    Some("column") => AtlasLayout::Column,
                    _ => {
                        eprintln!("Error: --layout expects row or column");
                        process::exit(1);
                    }
                };
            }
            "--purpose" => {
                i += 1;
                purpose = Some(match args.get(i).map(String::as_str) {
                    Some("stereo") => PURPOSE_STEREOSCOPIC_VIEWPORTS,
                    Some("multiview") => PURPOSE_MULTIVIEW_PLAYBACK,
                    _ => {
                        eprintln!("Error: --purpose expects stereo or multiview");
                        process::exit(1);
                    }
                });
            }
            "--extract-layer" => {
                i += 1;
                extract = Some(
                    args.get(i)
                        .and_then(|value| value.parse().ok())
                        .unwrap_or_else(|| {
                            eprintln!("Error: --extract-layer expects a layer id");
                            process::exit(1);
                        }),
                );
            }
            "-h" | "--help" => usage(),
            other if other.starts_with('-') => {
                eprintln!("Error: unknown argument: {}", other);
                usage();
            }
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }

    if positional.len() < 2 {
        usage();
    }
    let output_path = positional.pop().unwrap();
    let input_paths = positional;

    if !multiview {
        if input_paths.len() != 1 {
            eprintln!("Error: multiple inputs require --multiview");
            process::exit(1);
        }
        if extract.is_some() {
            eprintln!("Error: --extract-layer requires --multiview");
            process::exit(1);
        }
        let (width, height, frames) = read_frames(&input_paths[0]);
        eprintln!(
            "Encoding {} frame(s) at {}x{} qindex={}",
            frames.len(),
            width,
            height,
            base_qindex
        );
        let bitstream = encode::encode_video_yuv(width, height, base_qindex, &frames);
        fs::write(&output_path, &bitstream).unwrap_or_else(|e| {
            eprintln!("Error writing output: {}", e);
            process::exit(1);
        });
        eprintln!("Wrote {} bytes to {}", bitstream.len(), output_path);
        return;
    }

    if input_paths.len() < 2 || input_paths.len() > 4 {
        eprintln!("Error: multiview encoding needs two to four input files");
        process::exit(1);
    }

    let mut views = Vec::new();
    let mut geometry = Vec::new();
    for (index, path) in input_paths.iter().enumerate() {
        let (width, height, frames) = read_frames(path);
        geometry.push((width, height));
        let view = if input_paths.len() == 2 {
            let (left, right) = stereo_view_types();
            MultiviewView::texture(width, height, if index == 0 { left } else { right }, frames)
        } else {
            MultiviewView::explicit_view(width, height, index as u8, frames)
        };
        views.push(view);
    }
    let purpose_id = purpose.unwrap_or(if input_paths.len() == 2 {
        PURPOSE_STEREOSCOPIC_VIEWPORTS
    } else {
        PURPOSE_MULTIVIEW_PLAYBACK
    });
    let config = MultiviewConfig {
        base_qindex,
        purpose_id,
        layout,
        views: &views,
    };
    eprintln!(
        "Encoding {} views at {:?} qindex={} purpose={}",
        views.len(),
        geometry,
        base_qindex,
        purpose_id
    );
    let bitstream = encode_multiview(&config).unwrap_or_else(|e| {
        eprintln!("Error encoding multiview: {}", e);
        process::exit(1);
    });
    let output = match extract {
        Some(layer) => extract_layer(&bitstream, layer),
        None => bitstream,
    };
    fs::write(&output_path, &output).unwrap_or_else(|e| {
        eprintln!("Error writing output: {}", e);
        process::exit(1);
    });
    eprintln!("Wrote {} bytes to {}", output.len(), output_path);
}
