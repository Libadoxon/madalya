pub mod frame;
pub mod player;
pub mod probe;
pub mod thumbnail;

use std::path::Path;

use anyhow::{Result, anyhow};
use gstreamer as gst;
use gstreamer_video as gst_video;

pub fn init() -> Result<()> {
    gst::init().map_err(|e| anyhow!("gstreamer init failed: {e}"))
}

pub fn path_to_uri(path: &Path) -> Result<String> {
    gst::glib::filename_to_uri(path, None)
        .map(|s| s.to_string())
        .map_err(|e| anyhow!("bad clip path {path:?}: {e}"))
}

/// Copy a decoded BGRA video sample into a tightly-packed buffer, dropping any
/// per-row stride padding so it maps 1:1 onto `RenderImage`.
pub fn sample_to_bgra(sample: &gst::Sample) -> Option<(u32, u32, Vec<u8>)> {
    let buffer = sample.buffer()?;
    let caps = sample.caps()?;
    let info = gst_video::VideoInfo::from_caps(caps).ok()?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info).ok()?;

    let w = info.width();
    let h = info.height();
    let stride = info.stride()[0] as usize;
    let data = frame.plane_data(0).ok()?;
    let row = w as usize * 4;

    let mut out = Vec::with_capacity(row * h as usize);
    for y in 0..h as usize {
        let start = y * stride;
        out.extend_from_slice(&data[start..start + row]);
    }
    Some((w, h, out))
}
