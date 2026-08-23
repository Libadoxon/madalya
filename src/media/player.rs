use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{Empty, RenderImage, Task, Window, img};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::{frame, sample_to_bgra};
use crate::library::model::TrackState;

#[derive(Clone, Copy, Default)]
pub struct PlayerOptions {
    pub muted: bool,
    pub looping: bool,
    pub preview_width: Option<u32>,
}

enum PlayerMsg {
    Frame(u32, u32, Vec<u8>),
    Eos,
}

pub struct Player {
    pipeline: Option<gst::Pipeline>,
    volumes: Arc<Mutex<Vec<gst::Element>>>,
    latest_frame: Option<Arc<RenderImage>>,
    current_rendered_frame: Option<Arc<RenderImage>>,
    previous_rendered_frame: Option<Arc<RenderImage>>,
    playing: bool,
    ended: bool,
    looping: bool,
    position_ms: u64,
    duration_ms: u64,
    _tasks: Vec<Task<()>>,
}

impl Player {
    pub fn new(
        uri: &str,
        opts: PlayerOptions,
        states: Vec<TrackState>,
        cx: &mut Context<Self>,
    ) -> Self {
        let volumes = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = smol::channel::bounded::<PlayerMsg>(2);

        let pipeline = match build_pipeline(uri, opts, states, volumes.clone(), tx) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::error!("failed to build player pipeline for {uri}: {e}");
                None
            }
        };

        let has_pipeline = pipeline.is_some();
        let mut tasks = Vec::new();
        if let Some(pipeline) = &pipeline {
            let _ = pipeline.set_state(gst::State::Playing);

            tasks.push(cx.spawn(async move |this, cx| {
                while let Ok(msg) = rx.recv().await {
                    match msg {
                        PlayerMsg::Frame(w, h, data) => {
                            let Some(image) = frame::to_render_image(w, h, data) else {
                                continue;
                            };
                            if this
                                .update(cx, |p, cx| {
                                    p.latest_frame = Some(image);
                                    cx.notify();
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        PlayerMsg::Eos => {
                            if this.update(cx, |p, cx| p.on_eos(cx)).unwrap_or(false) {
                                break;
                            }
                        }
                    }
                }
            }));

            tasks.push(cx.spawn(async move |this, cx| {
                loop {
                    smol::Timer::after(Duration::from_millis(250)).await;
                    if this
                        .update(cx, |p, cx| {
                            p.drain_bus();
                            p.refresh_position();
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }));
        }

        Self {
            pipeline,
            volumes,
            latest_frame: None,
            current_rendered_frame: None,
            previous_rendered_frame: None,
            playing: has_pipeline,
            ended: false,
            looping: opts.looping,
            position_ms: 0,
            duration_ms: 0,
            _tasks: tasks,
        }
    }

    pub fn playing(&self) -> bool {
        self.playing
    }
    pub fn position_ms(&self) -> u64 {
        self.position_ms
    }
    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    pub fn set_playing(&mut self, playing: bool, cx: &mut Context<Self>) {
        let Some(p) = &self.pipeline else { return };
        let state = if playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        if p.set_state(state).is_ok() {
            self.playing = playing;
            self.ended = false;
            cx.notify();
        }
    }

    pub fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if self.ended {
            self.seek_ms(0, cx);
            self.set_playing(true, cx);
        } else {
            self.set_playing(!self.playing, cx);
        }
    }

    pub fn seek_ms(&mut self, ms: u64, cx: &mut Context<Self>) {
        let Some(p) = &self.pipeline else { return };
        let _ = p.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_mseconds(ms),
        );
        self.position_ms = ms;
        self.ended = false;
        cx.notify();
    }

    pub fn set_track(&self, idx: u32, volume: f64, muted: bool) {
        if let Some(vol) = self.volumes.lock().unwrap().get(idx as usize) {
            vol.set_property("volume", volume);
            vol.set_property("mute", muted);
        }
    }

    pub fn set_all_muted(&self, muted: bool) {
        for vol in self.volumes.lock().unwrap().iter() {
            vol.set_property("mute", muted);
        }
    }

    fn on_eos(&mut self, cx: &mut Context<Self>) -> bool {
        if self.looping {
            self.seek_ms(0, cx);
        } else {
            self.playing = false;
            self.ended = true;
            cx.notify();
        }
        false
    }

    fn drain_bus(&self) {
        let Some(bus) = self.pipeline.as_ref().and_then(|p| p.bus()) else {
            return;
        };
        while let Some(msg) = bus.pop() {
            use gst::MessageView;
            match msg.view() {
                MessageView::Error(e) => tracing::error!(
                    "pipeline error from {:?}: {} ({:?})",
                    e.src().map(|s| s.path_string()),
                    e.error(),
                    e.debug()
                ),
                MessageView::Warning(w) => {
                    tracing::warn!("pipeline warning: {} ({:?})", w.error(), w.debug())
                }
                _ => {}
            }
        }
    }

    fn refresh_position(&mut self) {
        let Some(p) = &self.pipeline else { return };
        if let Some(pos) = p.query_position::<gst::ClockTime>() {
            self.position_ms = pos.mseconds();
        }
        if self.duration_ms == 0
            && let Some(dur) = p.query_duration::<gst::ClockTime>()
        {
            self.duration_ms = dur.mseconds();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if let Some(p) = &self.pipeline {
            let _ = p.set_state(gst::State::Null);
        }
    }
}

impl Render for Player {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let Some(latest) = &self.latest_frame else {
            return Empty.into_any_element();
        };

        // Double-buffer + drop the frame we're replacing so its GPU texture is
        // freed (gpui caches by image id). Mirrors zed's Linux video path.
        if let Some(current) = self.current_rendered_frame.take() {
            if let Some(prev) = self.previous_rendered_frame.take()
                && prev.id != current.id
            {
                let _ = _window.drop_image(prev);
            }
            self.previous_rendered_frame = Some(current);
        }
        self.current_rendered_frame = Some(latest.clone());

        img(latest.clone())
            .size_full()
            .object_fit(gpui::ObjectFit::Contain)
            .into_any_element()
    }
}

fn build_pipeline(
    uri: &str,
    opts: PlayerOptions,
    states: Vec<TrackState>,
    volumes: Arc<Mutex<Vec<gst::Element>>>,
    tx: smol::channel::Sender<PlayerMsg>,
) -> anyhow::Result<gst::Pipeline> {
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("uridecodebin")
        .property("uri", uri)
        .build()?;
    let videoconvert = gst::ElementFactory::make("videoconvert").build()?;
    let videoscale = gst::ElementFactory::make("videoscale").build()?;

    let mut caps = gst::Caps::builder("video/x-raw").field("format", "BGRA");
    if let Some(w) = opts.preview_width {
        caps = caps
            .field("width", w as i32)
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1));
    }
    let appsink = gst_app::AppSink::builder()
        .caps(&caps.build())
        .max_buffers(2)
        .drop(true)
        .build();

    pipeline.add_many([&src, &videoconvert, &videoscale, appsink.upcast_ref()])?;
    gst::Element::link_many([&videoconvert, &videoscale, appsink.upcast_ref()])?;

    let audiomixer = if opts.muted {
        None
    } else {
        let mixer = gst::ElementFactory::make("audiomixer").build()?;
        let conv = gst::ElementFactory::make("audioconvert").build()?;
        let resample = gst::ElementFactory::make("audioresample").build()?;
        let sink = gst::ElementFactory::make("autoaudiosink").build()?;
        // Don't let the audio sink's preroll gate the pipeline: video must play
        // even when no audio device is available.
        sink.set_property("async-handling", true);
        pipeline.add_many([&mixer, &conv, &resample, &sink])?;
        gst::Element::link_many([&mixer, &conv, &resample, &sink])?;
        Some(mixer)
    };

    let tx_frame = tx.clone();
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                match sample_to_bgra(&sample) {
                    Some((w, h, data)) => {
                        let _ = tx_frame.try_send(PlayerMsg::Frame(w, h, data));
                    }
                    None => tracing::warn!("sample_to_bgra returned None"),
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .eos(move |_| {
                let _ = tx.try_send(PlayerMsg::Eos);
            })
            .build(),
    );

    let counter = AtomicUsize::new(0);
    let pipeline_weak = pipeline.downgrade();
    let vc_sink = videoconvert
        .static_pad("sink")
        .expect("videoconvert has sink pad");

    src.connect_pad_added(move |_src, pad| {
        let Some(pipeline) = pipeline_weak.upgrade() else {
            return;
        };
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let name = caps
            .structure(0)
            .map(|s| s.name().as_str().to_string())
            .unwrap_or_default();

        if name.starts_with("video/") {
            if let Err(e) = pad.link(&vc_sink) {
                tracing::error!("failed to link video pad: {e:?}");
            }
            return;
        }
        if !name.starts_with("audio/") {
            return;
        }

        match &audiomixer {
            None => {
                let Ok(fakesink) = gst::ElementFactory::make("fakesink")
                    .property("sync", true)
                    .build()
                else {
                    return;
                };
                if pipeline.add(&fakesink).is_ok() {
                    let _ = fakesink.sync_state_with_parent();
                    if let Some(sinkpad) = fakesink.static_pad("sink") {
                        let _ = pad.link(&sinkpad);
                    }
                }
            }
            Some(mixer) => {
                let idx = counter.fetch_add(1, Ordering::SeqCst);
                let (Ok(queue), Ok(conv), Ok(resample), Ok(vol)) = (
                    gst::ElementFactory::make("queue").build(),
                    gst::ElementFactory::make("audioconvert").build(),
                    gst::ElementFactory::make("audioresample").build(),
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

                let chain = [&queue, &conv, &resample, &vol];
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
                volumes.lock().unwrap().push(vol);
            }
        }
    });

    Ok(pipeline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_produces_frames() {
        let clip = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join("clips-test/random_capture.mp4");
        if !clip.exists() {
            eprintln!("skipping: no test clip at {clip:?}");
            return;
        }
        crate::media::init().unwrap();
        let uri = crate::media::path_to_uri(&clip).unwrap();
        let (tx, rx) = smol::channel::bounded::<PlayerMsg>(4);
        let vols = Arc::new(Mutex::new(Vec::new()));
        let pipeline = build_pipeline(
            &uri,
            PlayerOptions {
                muted: true,
                looping: false,
                preview_width: None,
            },
            Vec::new(),
            vols,
            tx,
        )
        .unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();

        let got = smol::block_on(async {
            let recv = async { rx.recv().await.ok() };
            let timeout = async {
                smol::Timer::after(Duration::from_secs(10)).await;
                None
            };
            smol::future::or(recv, timeout).await
        });
        let _ = pipeline.set_state(gst::State::Null);

        match got {
            Some(PlayerMsg::Frame(w, h, data)) => {
                assert!(w > 0 && h > 0);
                assert_eq!(data.len(), (w * h * 4) as usize);
            }
            other => panic!("expected a frame, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn pipeline_wires_all_audio_tracks() {
        let clip = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join("clips-test/steam_app_570 - teamfight.mkv");
        if !clip.exists() {
            eprintln!("skipping: no multi-track test clip at {clip:?}");
            return;
        }
        crate::media::init().unwrap();
        let uri = crate::media::path_to_uri(&clip).unwrap();
        let (tx, _rx) = smol::channel::bounded::<PlayerMsg>(4);
        let vols = Arc::new(Mutex::new(Vec::new()));
        let pipeline = build_pipeline(
            &uri,
            PlayerOptions {
                muted: false,
                looping: false,
                preview_width: None,
            },
            Vec::new(),
            vols.clone(),
            tx,
        )
        .unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        smol::block_on(smol::Timer::after(Duration::from_secs(3)));
        let n = vols.lock().unwrap().len();
        let _ = pipeline.set_state(gst::State::Null);
        assert_eq!(n, 3, "got {n} audio tracks wired");
    }
}
