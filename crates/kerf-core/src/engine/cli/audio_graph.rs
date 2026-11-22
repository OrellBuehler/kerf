//! The sound of a graph: the master bus, a clip's audio chain and its effects.

use super::*;

#[cfg(test)]
thread_local! {
    /// What [`alimiter_latency_available`] answers on this thread while a test pins it.
    static ALIMITER_LATENCY_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Run `f` as if this ffmpeg's `alimiter` did (`true`) or did not (`false`) have `latency`.
#[cfg(test)]
pub(super) fn with_alimiter_latency<R>(available: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ALIMITER_LATENCY_OVERRIDE.with(|c| c.set(self.0));
        }
    }
    let _reset = Reset(ALIMITER_LATENCY_OVERRIDE.with(|c| c.replace(Some(available))));
    f()
}

/// Whether `ffmpeg -h filter=alimiter` lists a `latency` option (pure, unit-tested).
pub(super) fn help_lists_latency(help: &str) -> bool {
    help.lines().any(|l| l.split_whitespace().next() == Some("latency"))
}

/// Whether this ffmpeg's `alimiter` takes `latency`, probed once per process. The
/// option arrived after FFmpeg 4.4 (Ubuntu 22.04's ffmpeg), which refuses the whole
/// graph with `Option 'latency' not found` — every limiter-on export and level
/// measurement. A binary that will not run reads as having it (the modern spelling,
/// like [`zscale_available`]: any render on it is about to fail anyway, and it keeps
/// the builders' output from depending on whether a test machine has ffmpeg).
pub(super) fn alimiter_latency_available() -> bool {
    // Tests pin the answer so an argv oracle does not depend on the ffmpeg
    // installed where it runs (see `golden`).
    #[cfg(test)]
    if let Some(forced) = ALIMITER_LATENCY_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let ok = output_bounded(
            command(&ffmpeg_bin()).args(["-hide_banner", "-loglevel", "quiet", "-h", "filter=alimiter"]),
            CAPABILITY_PROBE_TIMEOUT,
            0,
        )
        .map(|o| help_lists_latency(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(true);
        tracing::debug!(available = ok, "probed ffmpeg for alimiter latency");
        ok
    })
}

/// The filters the master bus adds after the final sum — its fader, then its
/// limiter — each led by the comma that joins it to the `amix` before it, or
/// the empty string while the master is neutral (the graph exactly as it was
/// before there was a master).
///
/// The limiter is `alimiter` with the two options that matter spelled out:
/// `level=0`, because its default **auto-levels** — scales the output back up so
/// the peak lands at full scale, which would turn a ceiling into a makeup gain —
/// and `latency=1`, because without it the lookahead delays the whole mix by the
/// attack time and drops the last few milliseconds at the end, putting the sound
/// out of step with the picture. An ffmpeg whose `alimiter` has no `latency`
/// ([`alimiter_latency_available`]) is given the limiter without it: the mix then
/// trails the picture by the 5 ms attack and loses its last 5 ms, under a frame at
/// any rate. Attack 5 ms is the filter's own lookahead; the
/// 100 ms release is slow enough not to pump on low notes. The ceiling is
/// rounded to six decimals, which is far below anything audible and keeps the
/// text from depending on the last digit of a libm `pow`.
///
/// With `true_peak` the limiter runs on the mix upsampled four times (`aresample` there and
/// back around it), so it holds the peaks between the samples too: the true peak a
/// platform measures, not only the sample peak.
pub(super) fn master_filters(master: &crate::model::MasterBus, sample_rate: u32) -> String {
    let mut out = String::new();
    let volume = master.safe_volume();
    if (volume - 1.0).abs() > f64::EPSILON {
        out += &format!(",volume={}", fnum(volume));
    }
    if master.limiter {
        let limiter = limiter_filter(master.limit_linear());
        if master.true_peak {
            out += &oversampled(&limiter, sample_rate);
        } else {
            out += &format!(",{limiter}");
        }
    }
    out
}

/// The master's `alimiter` holding `limit` (linear, rounded to six decimals), with
/// `latency=1` where this ffmpeg has it (see [`master_filters`]).
pub(super) fn limiter_filter(limit: f64) -> String {
    let limit = (limit * 1e6).round() / 1e6;
    let latency = if alimiter_latency_available() { ":latency=1" } else { "" };
    format!("alimiter=limit={}:attack=5:release=100:level=0{latency}", fnum(limit))
}

/// `filter` run at four times `sample_rate` and resampled back, led by its joining comma: a
/// limiter there sees the peaks between the output's samples (BS.1770 reads the true peak
/// off the same 4x oversampling).
fn oversampled(filter: &str, sample_rate: u32) -> String {
    format!(
        ",aresample={},{filter},aresample={sample_rate}",
        sample_rate.saturating_mul(4)
    )
}

/// The static loudness normalization: the measured gain (omitted when there is none to
/// apply) then a true-peak limiter just under the target's ceiling, led by its comma.
pub(super) fn static_loudness_filters(target: LoudnessPreset, gain_db: Option<f64>, sample_rate: u32) -> String {
    let mut out = String::new();
    if let Some(g) = gain_db.filter(|g| g.is_finite() && g.abs() >= 0.005) {
        out += &format!(",volume={:.2}dB", g.clamp(-LOUDNESS_MAX_GAIN_DB, LOUDNESS_MAX_GAIN_DB));
    }
    let ceiling = 10f64.powf((target.true_peak_db() - STATIC_TP_MARGIN_DB) / 20.0);
    out + &oversampled(&limiter_filter(ceiling), sample_rate)
}

/// The owning track's mix settings, carried alongside each of its audio clips.
/// The fader rides the *finished* clip — after its own gain and effect chain,
/// the way a channel strip does — so pulling a music bed down does not change
/// what its own compressor was reacting to.
#[derive(Clone, Copy)]
pub(super) struct TrackMix {
    pub(super) duck: bool,
    pub(super) volume: f32,
    pub(super) pan: (f64, f64),
}

/// A clip's whole audio effect chain as one comma-joined filter string, or `None`
/// when it has no effects.
///
/// Public because the GUI's preview monitor decodes clip audio through the same
/// chain the export renders — the chain is only *described* once, here, so the
/// two can't drift.
pub fn audio_effects_filter(effects: &[AudioEffect]) -> Option<String> {
    (!effects.is_empty()).then(|| {
        effects
            .iter()
            .map(|e| audio_effect_filter(e, None))
            .collect::<Vec<_>>()
            .join(",")
    })
}

/// The filter for one audio effect. dB thresholds / make-up gain are converted to
/// the linear units ffmpeg's dynamics filters expect. `len` is the clip's length where the
/// chain is the export's (a denoise on an `afftdn` that does not delay is cut to it).
pub(super) fn audio_effect_filter(e: &AudioEffect, len: Option<f64>) -> String {
    match e {
        AudioEffect::Highpass { hz } => format!("highpass=f={}", fnum(*hz)),
        AudioEffect::Lowpass { hz } => format!("lowpass=f={}", fnum(*hz)),
        AudioEffect::Equalizer { hz, width, gain_db } => {
            format!(
                "equalizer=f={}:width_type=h:width={}:g={}",
                fnum(*hz),
                fnum(*width),
                fnum(*gain_db)
            )
        }
        AudioEffect::Compressor {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            makeup_db,
        } => format!(
            "acompressor=threshold={}:ratio={}:attack={}:release={}:makeup={}",
            fnum(db_to_linear(*threshold_db)),
            fnum(*ratio),
            fnum(*attack_ms),
            fnum(*release_ms),
            fnum(db_to_linear(*makeup_db)),
        ),
        AudioEffect::Gate { threshold_db } => format!("agate=threshold={}", fnum(db_to_linear(*threshold_db))),
        AudioEffect::Denoise {
            reduction_db,
            floor_db,
            track_noise,
        } => denoise_filter(*reduction_db, *floor_db, *track_noise, len),
    }
}

/// The audio filter chain for one clip (between `[i:a]` and `[a{i}]`): trim,
/// optional reverse / tempo, gain, fades (including transition cross-fades) and
/// delay to the clip's timeline position. Defaults reduce to the original chain.
///
/// One chain, so one tempo: a clip with a speed ramp of more than one step goes through
/// [`audio_clip_chains`] instead (this plays it at its mean speed, which is no ramp).
pub(super) fn audio_clip_chain(clip: &Clip, fmt: &ExportFormat, fx: &ClipFx, layout: &str, mix: TrackMix) -> String {
    let mut p = audio_clip_head(clip, fx);
    let s = if clip.is_ramped() {
        match ramp::audio_steps(clip, ClipTiming::new(clip, fx).duration()).as_slice() {
            [only] => only.tempo(),
            _ => clip.mean_speed(),
        }
    } else {
        clip.speed_mag()
    };
    if (s - 1.0).abs() > 1e-9 {
        p.push(atempo_chain(s));
    }
    p.extend(audio_clip_tail(clip, fmt, fx, layout, mix));
    p.join(",")
}

/// The sound of a clip as the chain starts it: the source window (with the crossfade tail, and
/// relative to the input-side `-ss`), restamped to zero, and played backwards when the clip is.
fn audio_clip_head(clip: &Clip, fx: &ClipFx) -> Vec<String> {
    // Mirror the video crossfade tail (extends below source_in when reversed) and
    // the same input-side `-ss` fast-seek, so the atrim is relative to the seek.
    let (trim_start, trim_end) = clip_source_window(clip, fx);
    let seek = clip_input_seek(clip, fx, trim_start);
    let mut p: Vec<String> = Vec::new();
    p.push(format!("atrim=start={}:end={}", trim_start - seek, trim_end - seek));
    p.push("asetpts=PTS-STARTPTS".to_string());
    if clip.is_reversed() {
        p.push("areverse".to_string());
    }
    p
}

/// What follows the tempo in a clip's sound: gain (keyed or not), effects, the fades, the track
/// fader, the delivery format and pan, and the delay to the clip's place on the timeline.
fn audio_clip_tail(clip: &Clip, fmt: &ExportFormat, fx: &ClipFx, layout: &str, mix: TrackMix) -> Vec<String> {
    let dur = ClipTiming::new(clip, fx).duration();
    let delay = audio_delay(clip.timeline_start, fmt.sample_rate);
    let fi = clip.fade_in + fx.black_in + fx.white_in + fx.afade_in;
    let fo = clip.fade_out + fx.black_out + fx.white_out + fx.tail;

    let mut p: Vec<String> = Vec::new();
    if clip.volume_animated() {
        // A keyed gain: `volume` evaluated per frame, after `atempo` so its `t` is the
        // clip's own playing time. It holds one gain for a whole frame, so the frames are
        // cut to a few milliseconds first (`asetnsamples`, without padding the last one) or
        // a fade would step at the decoder's frame size — 21 ms for AAC, audible as zipper noise.
        p.push(format!("asetnsamples=n={VOLUME_FRAME_SAMPLES}:p=0"));
        p.push(format!(
            "volume='{}':eval=frame",
            keyframe_expr(&clip.property_curve(Property::Volume), "t", 0.0)
        ));
    } else {
        p.push(format!("volume={}", clip.volume));
    }
    // Per-clip audio effects (EQ / compressor / gate / denoise / filters) in author order,
    // after the clip gain.
    for e in &clip.audio {
        p.push(audio_effect_filter(e, Some(dur)));
    }
    // A fade that is wholly one half of a real crossfade pair (`ClipFx::afade_in_power` /
    // `tail_power`) is constant-power (`qsin`: sin in, cos out, so the summed power stays
    // put) where linear ramps dip about 3 dB at the midpoint. A fade with a clip fade or a
    // dip in it, a fade up from black, and a pair playing the same sound on both sides keep
    // the linear ramp they always had.
    let crossfade = |fade: f64, own: f64, power: bool| {
        if fade > 0.0 && own <= 0.0 && power {
            ":curve=qsin"
        } else {
            ""
        }
    };
    if fi > 0.0 {
        let curve = crossfade(fx.afade_in, clip.fade_in + fx.black_in + fx.white_in, fx.afade_in_power);
        p.push(format!("afade=t=in:st=0:d={}{curve}", fi.clamp(0.0, dur)));
    }
    if fo > 0.0 {
        let curve = crossfade(fx.tail, clip.fade_out + fx.black_out + fx.white_out, fx.tail_power);
        p.push(format!(
            "afade=t=out:st={}:d={}{curve}",
            (dur - fo).max(0.0),
            fo.clamp(0.0, dur)
        ));
    }
    // The track fader, after the clip's own chain. Omitted at unity, so a
    // project that never touched a fader renders the graph it always did.
    if (mix.volume - 1.0).abs() > f32::EPSILON {
        p.push(format!("volume={}", mix.volume));
    }
    p.push(format!(
        "aformat=sample_rates={sr}:channel_layouts={layout}",
        sr = fmt.sample_rate
    ));
    // The track pan, after `aformat` has normalized the stream to the delivery
    // layout: `pan` indexes channels by number, and run before the upmix a mono
    // source has no c1 — the attenuated leg would be synthesized from silence,
    // so panning a mono voice right would mute it instead of leaning it.
    // Omitted at centre, so an untouched mix stays byte-identical.
    let (gl, gr) = mix.pan;
    if layout == "stereo" && ((gl - 1.0).abs() > 1e-9 || (gr - 1.0).abs() > 1e-9) {
        // A balance, not a mono re-pan: each side keeps its own channel and is
        // attenuated, so a stereo music bed leans without collapsing.
        p.push(format!("pan=stereo|c0={}*c0|c1={}*c1", fnum(gl), fnum(gr)));
    }
    p.push(format!("adelay={delay}:all=1"));
    p
}

/// The chains of a clip's sound, `[src]…[a{flat}]`: [`audio_clip_chain`]'s one chain, or for a speed
/// ramp of several steps the pieces of [`ramp::audio_pieces`] cut from the source, each through its own
/// `atempo` and trimmed to its share of the timeline, crossfaded end to end, and then the chain's usual
/// tail on the whole. The head is resampled to the delivery rate first, so the trims count its samples.
///
/// ```text
/// [src]atrim=…,asetpts=PTS-STARTPTS,aresample=48000,asplit=3[ar0s0][ar0s1][ar0s2]
/// [ar0s0]atrim=start=0:end=1.18,asetpts=PTS-STARTPTS,atempo=1.1,adelay=…S:all=1,apad=whole_len=…,atrim=start_sample=0:end_sample=…,asetpts=N/SR/TB[ar0p0]
/// … [ar0p0][ar0p1]acrossfade=d=0.002[ar0x1] … [ar0x2]volume=1,…,adelay=0:all=1,asetpts=N/SR/TB[a0]
/// ```
pub(super) fn audio_clip_chains(
    src: &str,
    flat: usize,
    clip: &Clip,
    fmt: &ExportFormat,
    fx: &ClipFx,
    layout: &str,
    mix: TrackMix,
) -> Vec<String> {
    let steps = if clip.is_ramped() {
        ramp::audio_steps(clip, ClipTiming::new(clip, fx).duration())
    } else {
        Vec::new()
    };
    if steps.len() < 2 || fmt.sample_rate == 0 {
        return vec![format!(
            "[{src}]{chain}[a{flat}]",
            chain = audio_clip_chain(clip, fmt, fx, layout, mix)
        )];
    }
    let (pieces, overlaps) = ramp::audio_pieces(&steps, fmt.sample_rate);
    let n = pieces.len();
    let mut chains = Vec::with_capacity(2 * n + 1);
    let mut head = audio_clip_head(clip, fx);
    head.push(format!("aresample={}", fmt.sample_rate));
    head.push(format!(
        "asplit={n}{}",
        (0..n).map(|i| format!("[ar{flat}s{i}]")).collect::<String>()
    ));
    chains.push(format!("[{src}]{}", head.join(",")));
    for (i, piece) in pieces.iter().enumerate() {
        let mut p = vec![
            format!("atrim=start={}:end={}", piece.from, piece.to),
            "asetpts=PTS-STARTPTS".to_string(),
        ];
        if ramp::tempo_applies(piece.tempo) {
            p.push(ramp::atempo_stages(piece.tempo));
        }
        if piece.lead > 0 {
            p.push(format!("adelay={}S:all=1", piece.lead));
        }
        p.push(format!("apad=whole_len={}", piece.skip + piece.keep));
        p.push(format!(
            "atrim=start_sample={}:end_sample={}",
            piece.skip,
            piece.skip + piece.keep
        ));
        // Counted, not rebased: the silence an `adelay` of `lead` puts first has no stamp to rebase.
        p.push("asetpts=N/SR/TB".to_string());
        chains.push(format!("[ar{flat}s{i}]{}[ar{flat}p{i}]", p.join(",")));
    }
    let mut joined = format!("ar{flat}p0");
    for (i, overlap) in overlaps.iter().enumerate() {
        let out = format!("ar{flat}x{}", i + 1);
        chains.push(format!(
            "[{joined}][ar{flat}p{}]acrossfade=d={}[{out}]",
            i + 1,
            *overlap as f64 / f64::from(fmt.sample_rate)
        ));
        joined = out;
    }
    // `adelay` puts out its silence before the first sample reaches it when its input is late (this
    // chain's is: nothing leaves the crossfades until the first pieces are cut), and stamps it with no
    // time at all, which the muxer cannot take. The stream is whole samples from the clip's start, so
    // they are counted again.
    chains.push(format!(
        "[{joined}]{},asetpts=N/SR/TB[a{flat}]",
        audio_clip_tail(clip, fmt, fx, layout, mix).join(",")
    ));
    chains
}

/// Samples in a frame the keyed volume is evaluated on (2.7 ms at 48 kHz): the gain steps by
/// at most the curve's slope times that between two frames.
const VOLUME_FRAME_SAMPLES: usize = 128;

/// `adelay`'s delay for a clip starting at `start` seconds: whole milliseconds when the
/// start is one (every graph that predates this stays byte-identical), otherwise an
/// exact sample count at `sample_rate` (`…S`) — rounding a bar-aligned splice to the
/// millisecond would put it up to half a millisecond off its neighbour. Pure.
fn audio_delay(start: f64, sample_rate: u32) -> String {
    let ms = start * 1000.0;
    if (ms - ms.round()).abs() < 1e-6 || sample_rate == 0 {
        format!("{}", ms.round().max(0.0) as i64)
    } else {
        format!("{}S", (start * sample_rate as f64).round().max(0.0) as i64)
    }
}

/// The speech-gate duck: `[aduck]` (the ducked bus) times a gain that sits at `depth`
/// dB while `[akside]` (the rest of the mix) speaks and at unity otherwise, into
/// `[aducked]`. Pure.
///
/// The gain is *made* as a signal: a constant 1.0 run through a `sidechaingate` keyed by
/// the rest is 1 while it speaks and 0 while it does not (the gate's 300 ms release is
/// the hangover between words). A gate run on the music itself switches within ~10 ms,
/// which clicks; so the 0/1 curve is smoothed twice — a fast one-pole (~50 ms rise) and a
/// slow one (~300 ms) — and the larger of the two taken, which rises with the fast one
/// and falls with the slow one. Three ffmpeg traps: a sidechain filter **ends with the
/// shorter input**, so a key that stops before the bed would cut the music off (the key
/// is padded with `apad`; the compressor path above still has this); a key of digital silence
/// holds the gate **open** (so the key gets a 1e-6 floor); and `amix` drops a negative
/// weight (so the gain is `1 - (1 - g) * curve` in one `aeval`, and the bus is
/// `amultiply`'d by it).
pub(super) fn duck_gate_chains(depth_db: f64, layout: &str) -> Vec<String> {
    let cut = 1.0 - 10f64.powf(depth_db / 20.0);
    let upmix = if layout == "mono" { "" } else { ",pan=stereo|c0=c0|c1=c0" };
    vec![
        "[akside]apad,aformat=channel_layouts=mono,aeval=val(0)+0.000001:c=same[akfloor]".to_string(),
        "[aduck]asplit=2[aduckdry][aduckdc]".to_string(),
        "[aduckdc]aeval=1:c=mono[adc]".to_string(),
        "[adc][akfloor]sidechaingate=threshold=0.03:range=0:ratio=9000:knee=1:attack=10:release=300,asplit=2[agfastin][agslowin]"
            .to_string(),
        "[agfastin]lowpass=f=7:p=1,aformat=sample_fmts=fltp:channel_layouts=mono[agfast]".to_string(),
        "[agslowin]lowpass=f=1.2:p=1,aformat=sample_fmts=fltp:channel_layouts=mono[agslow]".to_string(),
        format!("[agfast][agslow]amerge=inputs=2,aeval='1-{cut:.6}*max(val(0),val(1))':c=mono{upmix}[again]"),
        "[aduckdry][again]amultiply[aducked]".to_string(),
    ]
}

/// Decompose a tempo change into `atempo` steps each within ffmpeg's supported
/// `[0.5, 2.0]` range (e.g. 4× → `atempo=2.0,atempo=2.0`).
fn atempo_chain(speed: f64) -> String {
    let mut s = speed;
    let mut parts: Vec<String> = Vec::new();
    while s > 2.0 {
        parts.push("atempo=2.0".to_string());
        s /= 2.0;
    }
    while s < 0.5 {
        parts.push("atempo=0.5".to_string());
        s *= 2.0;
    }
    parts.push(format!("atempo={s}"));
    parts.join(",")
}
