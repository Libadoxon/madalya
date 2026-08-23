use anyhow::{Result, anyhow};
use gstreamer as gst;
use gstreamer_pbutils::{Discoverer, prelude::*};

use crate::library::model::{AudioTrack, ClipProbe};

pub fn probe(uri: &str) -> Result<ClipProbe> {
    let discoverer = Discoverer::new(gst::ClockTime::from_seconds(15))
        .map_err(|e| anyhow!("discoverer create failed: {e}"))?;
    let info = discoverer
        .discover_uri(uri)
        .map_err(|e| anyhow!("probe failed for {uri}: {e}"))?;

    let mut probe = ClipProbe {
        duration_ms: info.duration().map(|d| d.mseconds()).unwrap_or(0),
        ..Default::default()
    };

    if let Some(v) = info.video_streams().first() {
        probe.width = v.width();
        probe.height = v.height();
        if let Some(caps) = v.caps() {
            probe.vcodec = gstreamer_pbutils::pb_utils_get_codec_description(&caps).to_string();
        }
    }

    for (i, a) in info.audio_streams().iter().enumerate() {
        let language = a.language().map(|s| s.to_string());
        let title = a.tags().and_then(|t| tag_string(&t, "title"));
        let label = title
            .or_else(|| language.clone())
            .unwrap_or_else(|| format!("Track {}", i + 1));
        probe.tracks.push(AudioTrack {
            idx: i as u32,
            label,
            language,
        });
    }

    if let Some(tags) = info.tags() {
        for name in ["title", "artist", "datetime", "encoder", "comment"] {
            if let Some(v) = tag_string(&tags, name) {
                probe.container_tags.push((name.to_string(), v));
            }
        }
    }

    Ok(probe)
}

fn tag_string(tags: &gst::TagList, name: &str) -> Option<String> {
    tags.generic(name)
        .and_then(|v| v.get::<String>().ok())
        .filter(|s| !s.is_empty())
}
