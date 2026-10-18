use std::sync::Mutex;

use crate::decode::Decoder;
use crate::multiview::{
    AtlasLayout, MultiviewConfig, MultiviewView, PURPOSE_MULTIVIEW_PLAYBACK,
    encode_multiview_with_threads,
};

static RESULT: Mutex<Vec<u8>> = Mutex::new(Vec::new());

#[unsafe(no_mangle)]
pub extern "C" fn av2_alloc(size: usize) -> *mut u8 {
    let layout = std::alloc::Layout::from_size_align(size.max(1), 1).expect("layout");
    unsafe { std::alloc::alloc(layout) }
}

#[unsafe(no_mangle)]
pub extern "C" fn av2_free(pointer: *mut u8, size: usize) {
    if pointer.is_null() {
        return;
    }
    let layout = std::alloc::Layout::from_size_align(size.max(1), 1).expect("layout");
    unsafe { std::alloc::dealloc(pointer, layout) }
}

#[unsafe(no_mangle)]
pub extern "C" fn av2_result_ptr() -> *const u8 {
    RESULT.lock().expect("result").as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn av2_result_len() -> usize {
    RESULT.lock().expect("result").len()
}

#[unsafe(no_mangle)]
pub extern "C" fn av2_encode_multiview(
    view_count: u32,
    width: u32,
    height: u32,
    qindex: u32,
    columns: u32,
    frames: *const u8,
) -> usize {
    if frames.is_null() || !(2..=4).contains(&view_count) || columns == 0 {
        return 0;
    }
    let width = width as usize;
    let height = height as usize;
    if width < 4 || height < 4 || width % 4 != 0 || height % 4 != 0 {
        return 0;
    }
    let luma = width * height;
    let chroma = (width / 2) * (height / 2);
    let frame_size = luma + 2 * chroma;
    let data = unsafe { std::slice::from_raw_parts(frames, frame_size * view_count as usize) };

    let views: Vec<MultiviewView> = (0..view_count as usize)
        .map(|index| {
            let start = index * frame_size;
            let y = data[start..start + luma].to_vec();
            let u = data[start + luma..start + luma + chroma].to_vec();
            let v = data[start + luma + chroma..start + frame_size].to_vec();
            MultiviewView::explicit_view(
                width as u32,
                height as u32,
                index as u8,
                vec![(y, u, v)],
            )
        })
        .collect();

    let config = MultiviewConfig {
        base_qindex: qindex as u8,
        purpose_id: PURPOSE_MULTIVIEW_PLAYBACK,
        layout: AtlasLayout::Grid {
            columns: columns as u8,
        },
        views: &views,
    };

    match encode_multiview_with_threads(&config, 1) {
        Ok(temporal_unit) => {
            let mut result = RESULT.lock().expect("result");
            *result = temporal_unit;
            result.len()
        }
        Err(_) => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn av2_decode(bitstream: *const u8, len: usize) -> usize {
    if bitstream.is_null() || len == 0 {
        return 0;
    }
    let data = unsafe { std::slice::from_raw_parts(bitstream, len) };
    let Ok(frames) = Decoder::decode_all(data) else {
        return 0;
    };
    if frames.is_empty() {
        return 0;
    }
    let width = frames[0].width as usize;
    let height = frames[0].height as usize;
    let frame_size = width * height + 2 * (width / 2) * (height / 2);
    let mut out = Vec::with_capacity(12 + frames.len() * frame_size);
    out.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    out.extend_from_slice(&(width as u32).to_le_bytes());
    out.extend_from_slice(&(height as u32).to_le_bytes());
    for frame in frames {
        out.extend_from_slice(&frame.y);
        out.extend_from_slice(&frame.u);
        out.extend_from_slice(&frame.v);
    }
    let mut result = RESULT.lock().expect("result");
    *result = out;
    result.len()
}

#[unsafe(no_mangle)]
pub extern "C" fn av2_convert_rgba(rgba: *const u8, width: u32, height: u32, out: *mut u8) -> usize {
    if rgba.is_null() || out.is_null() || width < 4 || height < 4 {
        return 0;
    }
    let width = width as usize;
    let height = height as usize;
    if width % 2 != 0 || height % 2 != 0 {
        return 0;
    }
    let source = unsafe { std::slice::from_raw_parts(rgba, width * height * 4) };
    let luma = width * height;
    let chroma = (width / 2) * (height / 2);
    let destination = unsafe { std::slice::from_raw_parts_mut(out, luma + 2 * chroma) };
    let (y_plane, rest) = destination.split_at_mut(luma);
    let (u_plane, v_plane) = rest.split_at_mut(chroma);
    for row in 0..height {
        for column in 0..width {
            let offset = (row * width + column) * 4;
            let red = source[offset] as i32;
            let green = source[offset + 1] as i32;
            let blue = source[offset + 2] as i32;
            let value = ((66 * red + 129 * green + 25 * blue + 128) >> 8) + 16;
            y_plane[row * width + column] = value.clamp(0, 255) as u8;
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
                    red += source[offset] as i32;
                    green += source[offset + 1] as i32;
                    blue += source[offset + 2] as i32;
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
    luma + 2 * chroma
}
