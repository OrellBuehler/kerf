//! In-process FFmpeg (libav) engine via `ffmpeg-next`.
//!
//! [`probe`] reads stream metadata in-process and is used whenever the
//! `ffmpeg` feature is on. [`render`] is the **experimental** in-process export
//! pipeline, compiled only with the `libav-render` feature; the default export
//! path drives the `ffmpeg` binary (see [`super::cli::render`]).
//!
//! `render` cannot be exercised in the `--no-default-features` CI build (it
//! needs the FFmpeg development libraries), so it is written against the
//! documented `ffmpeg-next` 8.1 API and may need adjustment on a full build.

use std::path::Path;
use std::sync::Once;

use ffmpeg_next as ff;

use super::ProbeResult;
use crate::error::Result;
use crate::model::{StreamInfo, StreamKind};

static INIT: Once = Once::new();

fn ensure_init() {
    INIT.call_once(|| {
        let _ = ff::init();
    });
}

pub fn probe(path: &Path) -> Result<ProbeResult> {
    ensure_init();

    let ictx = ff::format::input(&path)?;
    let duration = (ictx.duration() as f64 / f64::from(ff::ffi::AV_TIME_BASE)).max(0.0);

    let mut streams = Vec::new();
    for stream in ictx.streams() {
        let codec = ff::codec::context::Context::from_parameters(stream.parameters())?;
        let medium = codec.medium();
        let kind = match medium {
            ff::media::Type::Video => StreamKind::Video,
            ff::media::Type::Audio => StreamKind::Audio,
            ff::media::Type::Subtitle => StreamKind::Subtitle,
            _ => StreamKind::Data,
        };

        let mut info = StreamInfo {
            index: stream.index() as u32,
            kind,
            codec: format!("{:?}", codec.id()).to_lowercase(),
            width: None,
            height: None,
            fps: None,
            sample_rate: None,
            channels: None,
        };

        match medium {
            ff::media::Type::Video => {
                if let Ok(video) = codec.decoder().video() {
                    info.width = Some(video.width());
                    info.height = Some(video.height());
                }
                let rate = stream.rate();
                if rate.denominator() != 0 {
                    info.fps = Some(rate.numerator() as f64 / rate.denominator() as f64);
                }
            }
            ff::media::Type::Audio => {
                if let Ok(audio) = codec.decoder().audio() {
                    info.sample_rate = Some(audio.rate());
                    info.channels = Some(audio.channels());
                }
            }
            _ => {}
        }

        streams.push(info);
    }

    Ok(ProbeResult { duration, streams })
}

// ---- in-process render (experimental, `libav-render` feature) --------------

#[cfg(feature = "libav-render")]
pub use render_impl::render;

#[cfg(feature = "libav-render")]
mod render_impl {
    use std::path::Path;

    use ff::format::{sample::Sample, Pixel};
    use ff::media::Type;
    use ffmpeg_next as ff;

    use super::ensure_init;
    use crate::error::{Error, Result};
    use crate::model::{Asset, StreamKind, Timeline};

    // Canonical intermediate formats negotiated through the graph so the
    // encoders always receive a known shape.
    const OUT_PIX: Pixel = Pixel::YUV420P;
    const OUT_RATE: i32 = 48_000;

    /// Render the timeline by decoding each clip, running the trim+volume+concat
    /// graph in-process via libavfilter, and encoding to H.264 / AAC.
    pub fn render(timeline: &Timeline, assets: &[Asset], output: &Path, _format: &str) -> Result<()> {
        ensure_init();

        let track = timeline
            .tracks
            .iter()
            .find(|t| t.kind == StreamKind::Video && !t.clips.is_empty())
            .or_else(|| timeline.tracks.iter().find(|t| !t.clips.is_empty()))
            .ok_or_else(|| Error::InvalidArgument("timeline has no clips to export".into()))?;

        // Open every clip's source and build its video + audio decoders.
        let mut inputs = Vec::new();
        for clip in &track.clips {
            let path = assets
                .iter()
                .find(|a| a.id == clip.asset_id)
                .map(|a| a.path.clone())
                .ok_or(Error::AssetNotFound(clip.asset_id))?;
            inputs.push(Clip::open(&path, clip.source_in, clip.source_out, clip.volume)?);
        }

        // Build a fully-closed filter graph: inline buffer sources per clip,
        // trim/volume, concat, then format-normalized buffersinks.
        let mut graph = ff::filter::Graph::new();
        graph.parse(&build_spec(&inputs))?;
        graph.validate()?;

        // Configure encoders from the first clip's parameters.
        let first = &inputs[0];
        let mut octx = ff::format::output(&output)?;

        let vcodec = ff::encoder::find(ff::codec::Id::H264);
        let mut vstream = octx.add_stream(vcodec)?;
        let mut venc = ff::codec::context::Context::new_with_codec(vcodec.ok_or(ff::Error::EncoderNotFound)?)
            .encoder()
            .video()?;
        venc.set_width(first.width);
        venc.set_height(first.height);
        venc.set_format(OUT_PIX);
        venc.set_time_base((1, first.fps.max(1)));
        venc.set_frame_rate(Some((first.fps.max(1), 1)));
        let mut venc = venc.open_as(vcodec)?;
        vstream.set_parameters(&venc);
        let v_index = vstream.index();

        let acodec = ff::encoder::find(ff::codec::Id::AAC);
        let mut astream = octx.add_stream(acodec)?;
        let mut aenc = ff::codec::context::Context::new_with_codec(acodec.ok_or(ff::Error::EncoderNotFound)?)
            .encoder()
            .audio()?;
        aenc.set_rate(OUT_RATE);
        aenc.set_channel_layout(ff::channel_layout::ChannelLayout::STEREO);
        aenc.set_format(Sample::F32(ff::format::sample::Type::Planar));
        aenc.set_time_base((1, OUT_RATE));
        let mut aenc = aenc.open_as(acodec)?;
        astream.set_parameters(&aenc);
        let a_index = astream.index();

        octx.write_header()?;
        let v_tb = octx.stream(v_index).unwrap().time_base();
        let a_tb = octx.stream(a_index).unwrap().time_base();

        // Feed each clip's frames into its sources in order, draining the sinks
        // after each so concat emits segments sequentially.
        for (i, clip) in inputs.iter_mut().enumerate() {
            clip.decode_into(&mut graph, i)?;
            graph.get(&format!("in{i}v")).unwrap().source().flush()?;
            graph.get(&format!("in{i}a")).unwrap().source().flush()?;
            drain(&mut graph, &mut venc, &mut aenc, &mut octx, v_index, a_index, v_tb, a_tb)?;
        }
        drain(&mut graph, &mut venc, &mut aenc, &mut octx, v_index, a_index, v_tb, a_tb)?;

        // Flush encoders.
        venc.send_eof()?;
        flush_encoder(&mut venc, &mut octx, v_index, v_tb)?;
        aenc.send_eof()?;
        flush_encoder(&mut aenc, &mut octx, a_index, a_tb)?;

        octx.write_trailer()?;
        Ok(())
    }

    /// One opened source clip: decoders plus its trim/volume parameters.
    struct Clip {
        ictx: ff::format::context::Input,
        v_stream: usize,
        a_stream: usize,
        vdec: ff::decoder::Video,
        adec: ff::decoder::Audio,
        width: u32,
        height: u32,
        fps: i32,
        start: f64,
        end: f64,
        volume: f32,
    }

    impl Clip {
        fn open(path: &str, start: f64, end: f64, volume: f32) -> Result<Self> {
            let ictx = ff::format::input(&path)?;
            let vstream = ictx
                .streams()
                .best(Type::Video)
                .ok_or_else(|| Error::Engine(format!("{path}: no video stream")))?;
            let astream = ictx
                .streams()
                .best(Type::Audio)
                .ok_or_else(|| Error::Engine(format!("{path}: no audio stream")))?;
            let v_stream = vstream.index();
            let a_stream = astream.index();
            let rate = vstream.rate();
            let fps = if rate.denominator() != 0 {
                (rate.numerator() as f64 / rate.denominator() as f64).round() as i32
            } else {
                25
            };

            let vdec = ff::codec::context::Context::from_parameters(vstream.parameters())?
                .decoder()
                .video()?;
            let adec = ff::codec::context::Context::from_parameters(astream.parameters())?
                .decoder()
                .audio()?;
            let width = vdec.width();
            let height = vdec.height();
            Ok(Self {
                ictx,
                v_stream,
                a_stream,
                vdec,
                adec,
                width,
                height,
                fps,
                start,
                end,
                volume,
            })
        }

        /// Demux and decode the whole clip, feeding frames into the graph's
        /// `in{i}v` / `in{i}a` buffer sources. The graph's `trim`/`atrim`
        /// filters keep only the `[start, end)` source range.
        fn decode_into(&mut self, graph: &mut ff::filter::Graph, i: usize) -> Result<()> {
            let v_name = format!("in{i}v");
            let a_name = format!("in{i}a");
            let mut vframe = ff::frame::Video::empty();
            let mut aframe = ff::frame::Audio::empty();

            let packets: Vec<(usize, ff::Packet)> = self.ictx.packets().map(|(s, p)| (s.index(), p)).collect();
            for (index, packet) in packets {
                if index == self.v_stream {
                    self.vdec.send_packet(&packet)?;
                    while self.vdec.receive_frame(&mut vframe).is_ok() {
                        graph.get(&v_name).unwrap().source().add(&vframe)?;
                    }
                } else if index == self.a_stream {
                    self.adec.send_packet(&packet)?;
                    while self.adec.receive_frame(&mut aframe).is_ok() {
                        graph.get(&a_name).unwrap().source().add(&aframe)?;
                    }
                }
            }
            // Flush decoders.
            self.vdec.send_eof()?;
            while self.vdec.receive_frame(&mut vframe).is_ok() {
                graph.get(&v_name).unwrap().source().add(&vframe)?;
            }
            self.adec.send_eof()?;
            while self.adec.receive_frame(&mut aframe).is_ok() {
                graph.get(&a_name).unwrap().source().add(&aframe)?;
            }
            Ok(())
        }
    }

    /// Build the closed libavfilter graph description for the export track.
    fn build_spec(clips: &[Clip]) -> String {
        let mut spec = String::new();
        let mut concat_in = String::new();
        for (i, c) in clips.iter().enumerate() {
            spec.push_str(&format!(
                "buffer@in{i}v=video_size={w}x{h}:pix_fmt={pf}:time_base=1/{fps}:pixel_aspect=1/1[src{i}v];",
                w = c.width,
                h = c.height,
                pf = OUT_PIX as i32,
                fps = c.fps.max(1),
            ));
            spec.push_str(&format!(
                "[src{i}v]trim=start={s}:end={e},setpts=PTS-STARTPTS,format=pix_fmts=yuv420p[v{i}];",
                s = c.start,
                e = c.end,
            ));
            spec.push_str(&format!(
                "abuffer@in{i}a=time_base=1/{r}:sample_rate={r}:sample_fmt=fltp:channel_layout=stereo[src{i}a];",
                r = OUT_RATE,
            ));
            spec.push_str(&format!(
                "[src{i}a]atrim=start={s}:end={e},asetpts=PTS-STARTPTS,volume={vol},aresample={r}[a{i}];",
                s = c.start,
                e = c.end,
                vol = c.volume,
                r = OUT_RATE,
            ));
            concat_in.push_str(&format!("[v{i}][a{i}]"));
        }
        spec.push_str(&format!("{concat_in}concat=n={n}:v=1:a=1[cv][ca];", n = clips.len()));
        spec.push_str("[cv]buffersink@outv;");
        spec.push_str(&format!(
            "[ca]aformat=sample_fmts=fltp:sample_rates={r}:channel_layouts=stereo,abuffersink@outa",
            r = OUT_RATE,
        ));
        spec
    }

    /// Pull every available frame from both sinks and encode + mux it.
    #[allow(clippy::too_many_arguments)]
    fn drain(
        graph: &mut ff::filter::Graph,
        venc: &mut ff::encoder::Video,
        aenc: &mut ff::encoder::Audio,
        octx: &mut ff::format::context::Output,
        v_index: usize,
        a_index: usize,
        v_tb: ff::Rational,
        a_tb: ff::Rational,
    ) -> Result<()> {
        let mut vframe = ff::frame::Video::empty();
        while graph.get("outv").unwrap().sink().frame(&mut vframe).is_ok() {
            venc.send_frame(&vframe)?;
            flush_encoder(venc, octx, v_index, v_tb)?;
        }
        let mut aframe = ff::frame::Audio::empty();
        while graph.get("outa").unwrap().sink().frame(&mut aframe).is_ok() {
            aenc.send_frame(&aframe)?;
            flush_encoder(aenc, octx, a_index, a_tb)?;
        }
        Ok(())
    }

    /// Drain pending packets from an encoder, rescale timestamps, and mux them.
    fn flush_encoder<E>(
        encoder: &mut E,
        octx: &mut ff::format::context::Output,
        stream_index: usize,
        stream_tb: ff::Rational,
    ) -> Result<()>
    where
        E: std::ops::DerefMut<Target = ff::codec::encoder::Encoder>,
    {
        let enc_tb = encoder.time_base();
        let mut packet = ff::Packet::empty();
        while encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(stream_index);
            packet.rescale_ts(enc_tb, stream_tb);
            packet.write_interleaved(octx)?;
        }
        Ok(())
    }
}
