//! "Fit music to length": plan an edit list of source segments that keeps a piece's
//! intro and ending and repeats or drops whole phrases in between until it lasts as
//! long as it should. Pure, so it is unit-tested; `Project::fit_music` writes the
//! plan onto the timeline as clips.
//!
//! The middle is the run of whole bars from the first downbeat to the last one. A plan
//! is a walk through those bars that may **jump** at a 4-bar boundary from bar `p` to
//! bar `q` when the phrases starting there repeat each other ([`PhraseMatch`]): what
//! plays after the jump is what would have played anyway, so the ear does not hear it.
//! A jump back repeats material, a jump forward skips it. Among the walks that reach
//! the ending with the bar count nearest the target, the one with the fewest (and
//! longest-phrase) jumps wins.

use serde::{Deserialize, Serialize};

use super::{Clip, MusicAnalysis, PhraseMatch, Transition, TransitionKind};

/// A splice is crossfaded over this window, centred on the splice point.
pub const SPLICE_CROSSFADE_S: f64 = 0.010;

/// How long the fade-out is when a fit that runs over is cut to its target.
pub const FIT_FADE_S: f64 = 2.0;

/// The longest walk the planner will search, in bars: a few hours at any tempo.
pub const MAX_FIT_BARS: usize = 4096;

/// One piece of the source, placed at `output_start` seconds into the fitted music.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MusicSegment {
    pub source_start: f64,
    pub source_end: f64,
    pub output_start: f64,
}

impl MusicSegment {
    pub fn len(&self) -> f64 {
        self.source_end - self.source_start
    }

    pub fn is_empty(&self) -> bool {
        self.len() <= 0.0
    }
}

/// A planned fit: the segments in output order, how long they last and how far that
/// is from the target (`remainder = target - duration`: positive is short of it,
/// negative runs over and wants a fade-out).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MusicFit {
    pub segments: Vec<MusicSegment>,
    pub target: f64,
    pub duration: f64,
    pub remainder: f64,
    /// Whole bars played between the intro and the ending.
    pub bars: usize,
    pub splices: usize,
}

/// What [`crate::project::Project::fit_music`] did: the clips it placed (in order), the
/// plan they came from, whether an overrun was cut and faded, and how long the music
/// now lasts on the timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MusicFitReport {
    pub clips: Vec<uuid::Uuid>,
    pub fit: MusicFit,
    pub faded: bool,
    pub duration: f64,
}

/// Round `t` to the sample grid of `rate` (unchanged for rate 0).
pub fn to_samples(t: f64, rate: u32) -> f64 {
    if rate == 0 {
        t
    } else {
        (t * rate as f64).round() / rate as f64
    }
}

/// Plan a fit of the analysed music to `target` seconds. Segment boundaries land on the
/// source's `sample_rate` grid, and each segment's output position is the exact sum of
/// the lengths before it, rounded to that grid once — so no splice drifts.
pub fn plan_music_fit(m: &MusicAnalysis, target: f64, sample_rate: u32) -> MusicFit {
    let bars = m.bar_chroma.len().min(m.grid.whole_bars(m.duration));
    let bar_s = m.grid.bar_s();
    let intro = m.grid.first_downbeat().max(0.0);
    let ending = if bars > 0 {
        (m.duration - m.grid.bar_start(bars)).max(0.0)
    } else {
        0.0
    };
    let walk = if bars == 0 || bar_s <= 0.0 {
        None
    } else {
        let wanted = (target - intro - ending) / bar_s;
        plan_walk(bars, &m.phrases, wanted)
    };
    let runs = match &walk {
        Some(played) => source_runs(m, bars, played),
        None => vec![(0.0, m.duration)],
    };
    let mut segments = Vec::with_capacity(runs.len());
    let mut at = 0.0_f64;
    for (start, end) in runs {
        let (start, end) = (to_samples(start, sample_rate), to_samples(end, sample_rate));
        if end - start <= 0.0 {
            continue;
        }
        segments.push(MusicSegment {
            source_start: start,
            source_end: end,
            output_start: to_samples(at, sample_rate),
        });
        at += end - start;
    }
    let duration = segments.last().map_or(0.0, |s| s.output_start + s.len());
    MusicFit {
        splices: segments.len().saturating_sub(1),
        bars: walk.as_ref().map_or(bars, Vec::len),
        target,
        duration,
        remainder: target - duration,
        segments,
    }
}

/// The source spans a walk plays, merged wherever it runs on in the source: the intro,
/// each run of consecutive bars, and the ending after the last bar.
fn source_runs(m: &MusicAnalysis, bars: usize, played: &[usize]) -> Vec<(f64, f64)> {
    let start_of = |k: usize| m.grid.bar_start(k).max(0.0);
    let mut runs = Vec::new();
    let mut open = 0.0;
    let mut next = 0usize;
    for &b in played {
        if b != next {
            // An intro too short to crossfade out of is dropped rather than spliced.
            if start_of(next) - open >= SPLICE_CROSSFADE_S {
                runs.push((open, start_of(next)));
            }
            open = start_of(b);
        }
        next = b + 1;
    }
    debug_assert_eq!(next, bars, "a walk ends on the last bar");
    runs.push((open, m.duration));
    runs
}

/// Splice cost: an 8-bar match is the safer jump, so it is the cheaper one.
fn jump_cost(bars: usize) -> u32 {
    if bars >= 8 {
        2
    } else {
        3
    }
}

/// The bars to play, in order, from bar 0 to the ending: the walk whose length is
/// nearest `wanted` bars (a tie goes to the longer one: a fade-out can shorten it, where
/// the shorter one leaves the picture's last seconds without music), and among those
/// the cheapest in jumps. `None` when there is nothing to walk.
///
/// A dynamic programme over (bar position, bars played, just jumped): playing a bar
/// moves one bar on, a jump moves to the matching phrase without playing anything,
/// and two jumps in a row are not allowed (they would be one jump).
fn plan_walk(bars: usize, phrases: &[PhraseMatch], wanted: f64) -> Option<Vec<usize>> {
    // A jump moves at most `bars` bars, so the nearest walk above `wanted` is within
    // one more pass over the song.
    let max_count = ((wanted.max(0.0).ceil() as usize).max(bars) + bars).min(MAX_FIT_BARS.max(2 * bars));
    let mut jumps: Vec<Vec<(usize, u32)>> = vec![Vec::new(); bars + 1];
    for p in phrases {
        if p.a + p.bars > bars || p.b + p.bars > bars || p.a == p.b {
            continue;
        }
        let cost = jump_cost(p.bars);
        for (from, to) in [(p.a, p.b), (p.b, p.a)] {
            match jumps[from].iter_mut().find(|(q, _)| *q == to) {
                Some(j) => j.1 = j.1.min(cost),
                None => jumps[from].push((to, cost)),
            }
        }
    }
    let width = bars + 1;
    let idx = |count: usize, pos: usize, jumped: usize| (count * width + pos) * 2 + jumped;
    let states = (max_count + 1) * width * 2;
    let mut cost = vec![u32::MAX; states];
    let mut parent = vec![usize::MAX; states];
    cost[idx(0, 0, 0)] = 0;
    for count in 0..=max_count {
        for (pos, from_here) in jumps.iter().enumerate().take(bars) {
            let from = idx(count, pos, 0);
            if cost[from] == u32::MAX {
                continue;
            }
            for &(to, w) in from_here {
                let s = idx(count, to, 1);
                if cost[from] + w < cost[s] {
                    cost[s] = cost[from] + w;
                    parent[s] = from;
                }
            }
        }
        if count == max_count {
            break;
        }
        for pos in 0..bars {
            for jumped in 0..2 {
                let from = idx(count, pos, jumped);
                if cost[from] == u32::MAX {
                    continue;
                }
                let s = idx(count + 1, pos + 1, 0);
                if cost[from] < cost[s] {
                    cost[s] = cost[from];
                    parent[s] = from;
                }
            }
        }
    }
    let mut best: Option<(usize, u32)> = None;
    for count in 0..=max_count {
        let c = cost[idx(count, bars, 0)];
        if c == u32::MAX {
            continue;
        }
        let better = match best {
            None => true,
            Some((bc, bcost)) => {
                let (d, bd) = ((count as f64 - wanted).abs(), (bc as f64 - wanted).abs());
                d < bd - 1e-9 || ((d - bd).abs() <= 1e-9 && (count > bc || (count == bc && c < bcost)))
            }
        };
        if better {
            best = Some((count, c));
        }
    }
    let (count, _) = best?;
    let mut played = Vec::with_capacity(count);
    let mut s = idx(count, bars, 0);
    while s != idx(0, 0, 0) {
        let p = parent[s];
        let (p_count, p_pos) = (p / 2 / width, p / 2 % width);
        if p_count + 1 == s / 2 / width {
            played.push(p_pos);
        }
        s = p;
    }
    played.reverse();
    Some(played)
}

/// The clips a fit puts on the timeline in place of `template` (the music clip it
/// was planned for), in order: each segment a copy of the clip's sound (gain and audio
/// effects) on its source span, placed at `template.timeline_start` plus its output
/// position. Every splice is crossfaded over one window centred on it — the outgoing
/// clip ends half a window early and plays on under the incoming one, which starts half
/// a window early — so both fades cover the same samples and the copies never play at
/// full gain together. With `fade_out` a fit that runs over is cut at its target and
/// faded out over the last [`FIT_FADE_S`].
pub fn music_fit_clips(template: &Clip, fit: &MusicFit, sample_rate: u32, fade_out: bool) -> Vec<Clip> {
    let half = to_samples(SPLICE_CROSSFADE_S / 2.0, sample_rate);
    let segs = &fit.segments;
    let mut clips: Vec<Clip> = segs
        .iter()
        .map(|s| {
            let mut c = template.clone();
            c.id = uuid::Uuid::new_v4();
            c.source_in = s.source_start;
            c.source_out = s.source_end;
            c.timeline_start = template.timeline_start + s.output_start;
            c.fade_in = 0.0;
            c.fade_out = 0.0;
            c.transition_in = None;
            c.keyframes.clear();
            c.link_id = None;
            c
        })
        .collect();
    for i in 1..clips.len() {
        let h = half
            .min(segs[i].source_start)
            .min(segs[i].len() / 2.0)
            .min(segs[i - 1].len() / 2.0);
        let h = to_samples(h.max(0.0), sample_rate);
        if h <= 0.0 {
            continue;
        }
        clips[i - 1].source_out -= h;
        clips[i].source_in -= h;
        clips[i].timeline_start -= h;
        clips[i].transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 2.0 * h,
        });
    }
    if let Some(first) = clips.first_mut() {
        first.fade_in = template.fade_in;
    }
    let end = template.timeline_start + fit.target;
    if fade_out && fit.remainder < 0.0 {
        clips.retain(|c| c.timeline_start < end - 1e-9);
        if let Some(last) = clips.last_mut() {
            last.source_out = last.source_in + to_samples(end - last.timeline_start, sample_rate);
            last.fade_out = FIT_FADE_S.min(last.duration());
        }
    } else if let Some(last) = clips.last_mut() {
        last.fade_out = template.fade_out;
    }
    clips
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BeatGrid;

    const SR: u32 = 48_000;

    /// 16 bars of 2 s after a 1 s intro, then a 1.5 s ending: an 8-bar progression
    /// played twice, whose two halves differ (so only 8-bar repeats and the 4-bar
    /// halves of them match).
    fn song() -> MusicAnalysis {
        let chord = |pc: usize| {
            let mut c = [0.0_f32; 12];
            c[pc] = 1.0;
            c
        };
        let prog = [0, 9, 5, 7, 0, 9, 2, 4];
        let bar_chroma: Vec<[f32; 12]> = (0..16).map(|k| chord(prog[k % 8])).collect();
        let phrases = crate::engine::music::phrase_matches(&bar_chroma, crate::engine::music::SPLICE_SIMILARITY);
        MusicAnalysis {
            grid: BeatGrid {
                period_s: 0.5,
                phase_s: 0.0,
                downbeat_offset: 2,
                beats_per_bar: 4,
            },
            duration: 1.0 + 32.0 + 1.5,
            bar_chroma,
            phrases,
        }
    }

    fn assert_contiguous(fit: &MusicFit) {
        let mut at = 0.0;
        for s in &fit.segments {
            assert!((s.output_start - at).abs() < 1e-9, "{fit:?}");
            assert!(s.len() > 0.0);
            at += s.len();
        }
        assert!((fit.duration - at).abs() < 1e-9);
        assert!((fit.remainder - (fit.target - fit.duration)).abs() < 1e-9);
    }

    #[test]
    fn the_natural_length_is_the_whole_file() {
        let m = song();
        let fit = plan_music_fit(&m, m.duration, SR);
        assert_eq!(fit.segments.len(), 1);
        assert_eq!(fit.segments[0].source_start, 0.0);
        assert_eq!(fit.segments[0].source_end, m.duration);
        assert_eq!((fit.splices, fit.bars), (0, 16));
        assert_eq!(fit.remainder, 0.0);
    }

    #[test]
    fn a_longer_target_repeats_a_whole_phrase_and_keeps_intro_and_ending() {
        let m = song();
        let fit = plan_music_fit(&m, m.duration + 16.0, SR);
        assert_contiguous(&fit);
        assert_eq!(fit.bars, 24);
        assert_eq!(fit.splices, 1, "{fit:?}");
        assert!(fit.remainder.abs() < 1e-9);
        assert_eq!(fit.segments[0].source_start, 0.0, "the intro plays");
        assert_eq!(fit.segments.last().unwrap().source_end, m.duration, "the ending plays");
        let first = fit.segments[0];
        let second = fit.segments[1];
        assert_eq!(first.source_end, 17.0, "the splice is on a phrase boundary");
        assert_eq!(second.source_start, 1.0, "back to the start of the repeated phrase");
    }

    #[test]
    fn a_shorter_target_drops_a_whole_phrase() {
        let m = song();
        let fit = plan_music_fit(&m, m.duration - 16.0, SR);
        assert_contiguous(&fit);
        assert_eq!((fit.bars, fit.splices), (8, 1), "{fit:?}");
        assert!(fit.remainder.abs() < 1e-9);
        assert_eq!(fit.segments[0].source_start, 0.0);
        assert_eq!(fit.segments.last().unwrap().source_end, m.duration);
    }

    #[test]
    fn an_off_grid_target_takes_the_nearest_arrangement_and_reports_the_rest() {
        let m = song();
        // The repeats are 8 bars apart, so the reachable lengths step by 8 bars (16 s).
        let short = plan_music_fit(&m, m.duration + 6.0, SR);
        assert_contiguous(&short);
        assert_eq!(short.bars, 16, "{short:?}");
        assert!((short.remainder - 6.0).abs() < 1e-9, "{}", short.remainder);
        let over = plan_music_fit(&m, m.duration + 10.0, SR);
        assert_eq!(over.bars, 24, "{over:?}");
        assert!((over.remainder - -6.0).abs() < 1e-9, "{}", over.remainder);
        // Halfway between two arrangements: the longer one, which a fade can shorten.
        let tie = plan_music_fit(&m, m.duration + 8.0, SR);
        assert_eq!(tie.bars, 24);
    }

    #[test]
    fn a_long_target_loops_until_it_fits() {
        let m = song();
        let fit = plan_music_fit(&m, 150.0, SR);
        assert_contiguous(&fit);
        assert!(fit.remainder.abs() <= 4.0 + 1e-9, "{fit:?}");
        assert_eq!(fit.segments.last().unwrap().source_end, m.duration);
    }

    #[test]
    fn music_without_repeats_plays_through_and_reports_the_difference() {
        let mut m = song();
        m.phrases.clear();
        let fit = plan_music_fit(&m, 60.0, SR);
        assert_eq!(fit.segments.len(), 1);
        assert!((fit.remainder - (60.0 - m.duration)).abs() < 1e-9);
    }

    #[test]
    fn splices_land_on_the_sample_grid_without_drift() {
        let mut m = song();
        m.grid.period_s = 0.4987;
        m.grid.phase_s = 0.0113;
        m.duration = m.grid.bar_start(16) + 1.234_567;
        let fit = plan_music_fit(&m, 120.0, 44_100);
        assert_contiguous(&fit);
        assert!(fit.splices >= 2);
        for s in &fit.segments {
            for t in [s.source_start, s.source_end, s.output_start] {
                let n = t * 44_100.0;
                assert!((n - n.round()).abs() < 1e-6, "{t} is off the sample grid");
            }
        }
    }

    #[test]
    fn no_bars_is_the_whole_file() {
        let mut m = song();
        m.bar_chroma.clear();
        m.phrases.clear();
        m.duration = 3.0;
        let fit = plan_music_fit(&m, 10.0, SR);
        assert_eq!(fit.segments.len(), 1);
        assert_eq!(fit.duration, 3.0);
    }

    fn template() -> Clip {
        let mut c = Clip::new(uuid::Uuid::new_v4(), 0.0, 34.5, 10.0);
        c.volume = 0.5;
        c.fade_in = 0.25;
        c
    }

    #[test]
    fn fit_clips_crossfade_over_one_window_centred_on_each_splice() {
        let m = song();
        let fit = plan_music_fit(&m, m.duration + 16.0, SR);
        let clips = music_fit_clips(&template(), &fit, SR, false);
        assert_eq!(clips.len(), 2);
        let (a, b) = (&clips[0], &clips[1]);
        let splice = 10.0 + fit.segments[1].output_start;
        assert_eq!(a.timeline_end(), b.timeline_start, "adjacent, so the transition pairs them");
        assert!((b.timeline_start - (splice - 0.005)).abs() < 1e-9);
        assert!((a.source_out - (fit.segments[0].source_end - 0.005)).abs() < 1e-9);
        assert!((b.source_in - (fit.segments[1].source_start - 0.005)).abs() < 1e-9);
        let tr = b.transition_in.as_ref().expect("a crossfade");
        assert_eq!(tr.kind, TransitionKind::Crossfade);
        assert!((tr.duration - 0.010).abs() < 1e-12);
        assert_eq!((a.fade_in, b.fade_out), (0.25, 0.0));
        assert!(clips.iter().all(|c| c.volume == 0.5 && c.id != template().id));
    }

    #[test]
    fn an_overrun_is_cut_at_the_target_and_faded() {
        let m = song();
        let fit = plan_music_fit(&m, m.duration + 10.0, SR);
        assert!(fit.remainder < 0.0);
        let clips = music_fit_clips(&template(), &fit, SR, true);
        let last = clips.last().unwrap();
        assert!(
            (last.timeline_end() - (10.0 + fit.target)).abs() < 1e-6,
            "{}",
            last.timeline_end()
        );
        assert_eq!(last.fade_out, FIT_FADE_S);
        let kept = music_fit_clips(&template(), &fit, SR, false);
        assert!(kept.last().unwrap().timeline_end() > 10.0 + fit.target);
    }
}
