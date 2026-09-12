use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{Empty, RenderImage, Task, Window, img};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::{frame, sample_to_bgra};

#[derive(Clone, Default)]
pub struct PlayerOptions {
    pub muted: bool,
    pub looping: bool,
    pub preview_width: Option<u32>,
    pub start_ms: u64,
    pub stop_ms: Option<u64>,
    pub audio_path: Option<PathBuf>,
    pub resume_ms: Option<u64>,
    pub start_paused: bool,
}

enum PlayerMsg {
    Frame(u32, u32, Vec<u8>, Option<u64>),
    Eos,
}

pub struct Player {
    pipeline: Option<gst::Pipeline>,
    audio_volume: Arc<Mutex<Option<gst::Element>>>,
    latest_frame: Option<Arc<RenderImage>>,
    current_rendered_frame: Option<Arc<RenderImage>>,
    previous_rendered_frame: Option<Arc<RenderImage>>,
    playing: bool,
    ended: bool,
    looping: bool,
    position_ms: u64,
    duration_ms: u64,
    start_ms: u64,
    stop_ms: Option<u64>,
    resume_ms: Option<u64>,
    start_paused: bool,
    pending_start: bool,
    enforce_stop: bool,
    _tasks: Vec<Task<()>>,
}

impl Player {
    pub fn new(uri: &str, opts: PlayerOptions, cx: &mut Context<Self>) -> Self {
        let audio_volume = Arc::new(Mutex::new(None));
        let (tx, rx) = smol::channel::bounded::<PlayerMsg>(2);

        let pipeline = match build_pipeline(uri, &opts, audio_volume.clone(), tx) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::error!("failed to build player pipeline for {uri}: {e}");
                None
            }
        };

        let has_pipeline = pipeline.is_some();
        // Start paused so the pipeline prerolls; playback (and the seek to the
        // highlight start) is kicked off once it's ready. Seeking a not-yet-
        // prerolled pipeline is silently dropped.
        let mut tasks = Vec::new();
        if let Some(pipeline) = &pipeline {
            let _ = pipeline.set_state(gst::State::Paused);

            tasks.push(cx.spawn(async move |this, cx| {
                while let Ok(msg) = rx.recv().await {
                    match msg {
                        PlayerMsg::Frame(w, h, data, pts) => {
                            let Some(image) = frame::to_render_image(w, h, data) else {
                                continue;
                            };
                            if this.update(cx, |p, cx| p.on_frame(image, pts, cx)).is_err() {
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
                            p.refresh_position(cx);
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
            audio_volume,
            latest_frame: None,
            current_rendered_frame: None,
            previous_rendered_frame: None,
            playing: has_pipeline && !opts.start_paused,
            ended: false,
            looping: opts.looping,
            position_ms: opts.resume_ms.unwrap_or(opts.start_ms),
            duration_ms: 0,
            start_ms: opts.start_ms,
            stop_ms: opts.stop_ms,
            resume_ms: opts.resume_ms,
            start_paused: opts.start_paused,
            pending_start: has_pipeline,
            enforce_stop: opts.stop_ms.is_some(),
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
        // ACCURATE (not KEY_UNIT): clips with a large GOP may have keyframes only
        // every few seconds, and KEY_UNIT would snap the seek back to the nearest
        // one — often frame 0 — so playback always restarted from the beginning.
        let _ = p.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::ClockTime::from_mseconds(ms),
        );
        self.position_ms = ms;
        self.ended = false;
        cx.notify();
    }

    /// A seek initiated by the user, which releases the highlight auto-stop so
    /// they can watch past the marked end.
    pub fn user_seek(&mut self, ms: u64, cx: &mut Context<Self>) {
        self.enforce_stop = false;
        self.seek_ms(ms, cx);
    }

    pub fn replay(&mut self, cx: &mut Context<Self>) {
        self.enforce_stop = self.stop_ms.is_some();
        self.seek_ms(self.start_ms, cx);
        self.set_playing(true, cx);
    }

    pub fn set_segment(&mut self, start_ms: u64, stop_ms: Option<u64>) {
        self.start_ms = start_ms;
        self.stop_ms = stop_ms;
    }

    pub fn set_all_muted(&self, muted: bool) {
        if let Some(vol) = self.audio_volume.lock().unwrap().as_ref() {
            vol.set_property("mute", muted);
        }
    }

    pub fn set_master_volume(&self, volume: f64) {
        if let Some(vol) = self.audio_volume.lock().unwrap().as_ref() {
            vol.set_property("volume", volume);
        }
    }

    fn on_eos(&mut self, cx: &mut Context<Self>) -> bool {
        if self.looping {
            self.seek_ms(self.start_ms, cx);
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

    fn on_frame(&mut self, image: Arc<RenderImage>, pts: Option<u64>, cx: &mut Context<Self>) {
        self.latest_frame = Some(image);
        if let Some(ms) = pts {
            self.position_ms = ms;
            if self.enforce_stop
                && self.playing
                && let Some(stop) = self.stop_ms
                && ms >= stop
            {
                if self.looping {
                    self.seek_ms(self.start_ms, cx);
                } else {
                    self.enforce_stop = false;
                    self.position_ms = stop;
                    self.set_playing(false, cx);
                }
            }
        }
        cx.notify();
    }

    fn refresh_position(&mut self, cx: &mut Context<Self>) {
        let (dur, prerolled) = {
            let Some(p) = &self.pipeline else { return };
            (
                p.query_duration::<gst::ClockTime>().map(|t| t.mseconds()),
                matches!(p.current_state(), gst::State::Paused | gst::State::Playing),
            )
        };
        if self.duration_ms == 0
            && let Some(dur) = dur
        {
            self.duration_ms = dur;
        }
        if self.pending_start && prerolled {
            self.pending_start = false;
            let target = self.resume_ms.unwrap_or(self.start_ms);
            if target > 0 {
                self.seek_ms(target, cx);
            }
            self.set_playing(!self.start_paused, cx);
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

fn send_frame(tx: &smol::channel::Sender<PlayerMsg>, sample: &gst::Sample) {
    let Some((w, h, data)) = sample_to_bgra(sample) else {
        tracing::warn!("sample_to_bgra returned None");
        return;
    };
    let pts = sample.buffer().and_then(|b| b.pts()).map(|t| t.mseconds());
    let _ = tx.try_send(PlayerMsg::Frame(w, h, data, pts));
}

fn build_pipeline(
    uri: &str,
    opts: &PlayerOptions,
    audio_volume: Arc<Mutex<Option<gst::Element>>>,
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
        .sync(true)
        .build();

    pipeline.add_many([&src, &videoconvert, &videoscale, appsink.upcast_ref()])?;
    gst::Element::link_many([&videoconvert, &videoscale, appsink.upcast_ref()])?;

    if let Some(path) = opts.audio_path.as_ref().filter(|_| !opts.muted) {
        let filesrc = gst::ElementFactory::make("filesrc")
            .property("location", path.to_string_lossy().as_ref())
            .build()?;
        let decode = gst::ElementFactory::make("decodebin").build()?;
        let queue = gst::ElementFactory::make("queue").build()?;
        let conv = gst::ElementFactory::make("audioconvert").build()?;
        let resample = gst::ElementFactory::make("audioresample").build()?;
        let vol = gst::ElementFactory::make("volume").build()?;
        let sink = gst::ElementFactory::make("autoaudiosink").build()?;
        pipeline.add_many([&filesrc, &decode, &queue, &conv, &resample, &vol, &sink])?;
        gst::Element::link(&filesrc, &decode)?;
        gst::Element::link_many([&queue, &conv, &resample, &vol, &sink])?;

        let queue_sink = queue.static_pad("sink").expect("queue has sink pad");
        decode.connect_pad_added(move |_, pad| {
            let _ = pad.link(&queue_sink);
        });
        *audio_volume.lock().unwrap() = Some(vol);
    }

    let tx_frame = tx.clone();
    let tx_eos = tx.clone();
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                send_frame(&tx_frame, &sample);
                Ok(gst::FlowSuccess::Ok)
            })
            .new_preroll(move |sink| {
                let sample = sink.pull_preroll().map_err(|_| gst::FlowError::Eos)?;
                send_frame(&tx, &sample);
                Ok(gst::FlowSuccess::Ok)
            })
            .eos(move |_| {
                let _ = tx_eos.try_send(PlayerMsg::Eos);
            })
            .build(),
    );

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
        if let Ok(fakesink) = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .build()
            && pipeline.add(&fakesink).is_ok()
        {
            let _ = fakesink.sync_state_with_parent();
            if let Some(sinkpad) = fakesink.static_pad("sink") {
                let _ = pad.link(&sinkpad);
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
        let audio_volume = Arc::new(Mutex::new(None));
        let pipeline = build_pipeline(
            &uri,
            &PlayerOptions {
                muted: true,
                looping: false,
                preview_width: None,
                ..Default::default()
            },
            audio_volume,
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
            Some(PlayerMsg::Frame(w, h, data, _)) => {
                assert!(w > 0 && h > 0);
                assert_eq!(data.len(), (w * h * 4) as usize);
            }
            other => panic!("expected a frame, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn render_mix_produces_audio_file() {
        use crate::library::model::TrackState;
        let clip = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join("clips-test/steam_app_570 - teamfight.mkv");
        if !clip.exists() {
            eprintln!("skipping: no multi-track test clip at {clip:?}");
            return;
        }
        crate::media::init().unwrap();
        let uri = crate::media::path_to_uri(&clip).unwrap();
        let states = vec![
            TrackState {
                idx: 0,
                volume: 1.0,
                muted: false,
            },
            TrackState {
                idx: 1,
                volume: 0.5,
                muted: false,
            },
        ];
        let dest = std::env::temp_dir().join(format!(
            "madalya-mix-{}.flac",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        crate::media::mix::render_mix(&uri, &states, &dest).unwrap();
        let len = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        let _ = std::fs::remove_file(&dest);
        assert!(len > 0, "mixed audio file is empty");
    }
}
