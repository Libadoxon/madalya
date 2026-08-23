use std::path::Path;

use anyhow::{Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::sample_to_bgra;

/// Decode a single frame ~1s in and write a JPEG thumbnail scaled to `target_w`.
pub fn generate(uri: &str, dest: &Path, target_w: u32) -> Result<()> {
    let sink_bin = gst::parse::bin_from_description(
        "videoconvert ! video/x-raw,format=BGRA ! appsink name=thumbsink sync=false",
        true,
    )?;
    let appsink = sink_bin
        .by_name("thumbsink")
        .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
        .ok_or_else(|| anyhow!("thumbnail appsink missing"))?;

    let playbin = gst::ElementFactory::make("playbin")
        .property("uri", uri)
        .build()?;
    playbin.set_property("video-sink", &sink_bin);
    playbin.set_property(
        "audio-sink",
        &gst::ElementFactory::make("fakesink").build()?,
    );

    playbin.set_state(gst::State::Paused)?;
    let _ = playbin.state(gst::ClockTime::from_seconds(10));
    let _ = playbin.seek_simple(
        gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
        gst::ClockTime::from_seconds(1),
    );
    let _ = playbin.state(gst::ClockTime::from_seconds(5));

    let sample = appsink
        .pull_preroll()
        .map_err(|_| anyhow!("no preroll frame for {uri}"))?;
    let _ = playbin.set_state(gst::State::Null);

    let (w, h, bgra) = sample_to_bgra(&sample).ok_or_else(|| anyhow!("frame decode failed"))?;

    let mut rgb = image::RgbImage::new(w, h);
    for (i, px) in rgb.pixels_mut().enumerate() {
        let b = i * 4;
        *px = image::Rgb([bgra[b + 2], bgra[b + 1], bgra[b]]);
    }
    let target_h = (h as f32 * target_w as f32 / w as f32).round().max(1.0) as u32;
    let thumb = image::imageops::thumbnail(&rgb, target_w, target_h);

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    thumb.save(dest)?;
    Ok(())
}
