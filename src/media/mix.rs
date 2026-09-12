use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;

use crate::library::model::TrackState;

pub fn cache_dir() -> PathBuf {
    crate::library::data_dir().join("audio-cache")
}

pub fn cache_path(path: &Path, mtime: i64, states: &[TrackState]) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    mtime.hash(&mut h);
    for st in states {
        st.idx.hash(&mut h);
        st.volume.to_bits().hash(&mut h);
        st.muted.hash(&mut h);
    }
    cache_dir().join(format!("{:016x}.flac", h.finish()))
}

pub fn render_mix(uri: &str, states: &[TrackState], dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_extension("flac.tmp");

    let mix_caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("rate", 48000)
        .field("channels", 2)
        .field("layout", "interleaved")
        .build();
    let out_caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("rate", 48000)
        .field("channels", 2)
        .field("layout", "interleaved")
        .build();

    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("uridecodebin")
        .property("uri", uri)
        .build()?;
    let mixer = gst::ElementFactory::make("audiomixer").build()?;
    let conv = gst::ElementFactory::make("audioconvert").build()?;
    let resample = gst::ElementFactory::make("audioresample").build()?;
    let outcaps = gst::ElementFactory::make("capsfilter")
        .property("caps", &out_caps)
        .build()?;
    let enc = gst::ElementFactory::make("flacenc").build()?;
    let sink = gst::ElementFactory::make("filesink")
        .property("location", tmp.to_string_lossy().as_ref())
        .build()?;

    pipeline.add_many([&src, &mixer, &conv, &resample, &outcaps, &enc, &sink])?;
    gst::Element::link_many([&mixer, &conv, &resample, &outcaps, &enc, &sink])?;

    let states = states.to_vec();
    let counter = AtomicUsize::new(0);
    let pipeline_weak = pipeline.downgrade();
    src.connect_pad_added(move |_src, pad| {
        let Some(pipeline) = pipeline_weak.upgrade() else {
            return;
        };
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let name = caps
            .structure(0)
            .map(|s| s.name().as_str().to_string())
            .unwrap_or_default();

        if !name.starts_with("audio/") {
            if let Ok(fakesink) = gst::ElementFactory::make("fakesink").build()
                && pipeline.add(&fakesink).is_ok()
            {
                let _ = fakesink.sync_state_with_parent();
                if let Some(sinkpad) = fakesink.static_pad("sink") {
                    let _ = pad.link(&sinkpad);
                }
            }
            return;
        }

        let idx = counter.fetch_add(1, Ordering::SeqCst);
        let (Ok(queue), Ok(conv), Ok(resample), Ok(rate), Ok(caps_el), Ok(vol)) = (
            gst::ElementFactory::make("queue").build(),
            gst::ElementFactory::make("audioconvert").build(),
            gst::ElementFactory::make("audioresample").build(),
            gst::ElementFactory::make("audiorate").build(),
            gst::ElementFactory::make("capsfilter")
                .property("caps", &mix_caps)
                .build(),
            gst::ElementFactory::make("volume").build(),
        ) else {
            return;
        };
        let st = states
            .iter()
            .find(|s| s.idx as usize == idx)
            .copied()
            .unwrap_or(TrackState::default_for(idx as u32));
        vol.set_property("volume", st.volume);
        vol.set_property("mute", st.muted);

        let chain = [&queue, &conv, &resample, &rate, &caps_el, &vol];
        if pipeline.add_many(chain).is_err() || gst::Element::link_many(chain).is_err() {
            return;
        }
        for el in chain {
            let _ = el.sync_state_with_parent();
        }
        let Some(queue_sink) = queue.static_pad("sink") else {
            return;
        };
        if pad.link(&queue_sink).is_err() {
            return;
        }
        if let (Some(vol_src), Some(mixer_sink)) =
            (vol.static_pad("src"), mixer.request_pad_simple("sink_%u"))
        {
            let _ = vol_src.link(&mixer_sink);
        }
    });

    pipeline.set_state(gst::State::Playing)?;
    let bus = pipeline
        .bus()
        .ok_or_else(|| anyhow!("mix pipeline has no bus"))?;
    let result = loop {
        let Some(msg) = bus.timed_pop_filtered(
            gst::ClockTime::NONE,
            &[gst::MessageType::Eos, gst::MessageType::Error],
        ) else {
            break Err(anyhow!("mix pipeline ended without EOS"));
        };
        match msg.view() {
            gst::MessageView::Eos(_) => break Ok(()),
            gst::MessageView::Error(e) => {
                break Err(anyhow!("mix error: {} ({:?})", e.error(), e.debug()));
            }
            _ => {}
        }
    };
    let _ = pipeline.set_state(gst::State::Null);
    result?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}
