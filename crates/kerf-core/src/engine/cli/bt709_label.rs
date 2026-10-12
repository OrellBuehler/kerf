//! The BT.709 label (`setparams=colorspace=bt709`) on the four graph builders, and the plan that
//! mirrors it.
//!
//! An untagged HD YCbCr clip is labelled BT.709 when another input of **its graph** names a matrix
//! (`model::bt709_labels`): the export, the scrub window and the playback stream decide over the
//! clips of the cut they are built from, a still over the layers on screen. Everything else is built
//! as it always was. These tests hold the argv of each builder to that rule — the positive stacks and
//! each negative (a single clip, untagged footage only, SD, MJPEG, RGB, alpha, a still image) — and
//! the plan (`PlanStream::label_bt709`, `composite_matrix`) to the builders; the `#[ignore]`d ones
//! render the graphs with the real `ffmpeg` (`cargo test -p kerf-core --no-default-features --
//! --ignored bt709_label`, `KERF_FFMPEG` / `KERF_FFPROBE` pick the build) and hold the label to a
//! file tagged BT.709, the preview's assets to the originals, and the GPU plan's verdict to FFmpeg.

#![allow(clippy::print_stderr)] // what each case measured, for whoever runs the ignored ones

use super::*;
use crate::engine::test_support::{make_clip, test_asset, timeline_of, video_stream, video_track};
use crate::model::{Asset, Clip, Delivery, StreamInfo};
use crate::planner::{PlanRequest, Planner};
use crate::render_plan::{composite_matrix, RenderPlan, YuvMatrix};

const LABEL: &str = "setparams=colorspace=bt709";

/// A picture of a kind the rule tells apart.
fn picture(name: &str, w: u32, h: u32, pix_fmt: &str, tag: Option<&str>) -> Asset {
    let mut stream = video_stream(w, h, 30.0);
    stream.pix_fmt = Some(pix_fmt.into());
    stream.color_space = tag.map(Into::into);
    let mut asset = test_asset(vec![stream]);
    asset.path = format!("/m/{name}.mp4");
    asset.name = name.into();
    asset
}

fn hdr(mut asset: Asset, transfer: &str) -> Asset {
    let s = &mut asset.streams[0];
    (s.color_transfer, s.color_primaries) = (Some(transfer.into()), Some("bt2020".into()));
    (s.pix_fmt, s.color_space) = (Some("yuv420p10le".into()), Some("bt2020nc".into()));
    asset
}

/// The footage the rule is about.
struct Kit {
    /// Untagged HD (the subject), and a second one.
    hd: Asset,
    hd2: Asset,
    /// A phone's clip: BT.709, tagged.
    phone: Asset,
    /// A full-range BT.709 H.264 clip (OBS, a drone): probed `h264 yuvj420p pc bt709`, a stated matrix.
    obs: Asset,
    hlg: Asset,
    /// Untagged SD, SD tagged `smpte170m`, HD tagged `smpte170m`.
    sd: Asset,
    sd601: Asset,
    hd601: Asset,
    /// HD with alpha (FFV1 `yuva420p`, untagged).
    alpha: Asset,
    /// The pictures whose tag is the decoder's default, in the shapes the probe really gives
    /// (`test_support::decoder_default_pictures`): PNGs (`rgb24`, `rgba`, `gray`, `gbr`), a JPEG and a
    /// WebP (`bt470bg`), MJPEG video, RGB video. They name no matrix and are never labelled.
    defaults: Vec<Asset>,
}

impl Kit {
    /// The kinds that are not untagged HD YCbCr, whatever their tag says.
    fn others(&self) -> Vec<&Asset> {
        let mut others = vec![&self.sd, &self.alpha];
        others.extend(&self.defaults);
        others
    }
}

fn kit() -> Kit {
    let defaults = crate::engine::test_support::decoder_default_pictures(1920, 1080)
        .into_iter()
        .map(|(what, stream)| {
            let mut asset = test_asset(vec![stream]);
            asset.name = what.into();
            asset.path = format!("/m/{}.bin", what.replace(' ', "-"));
            asset
        })
        .collect();
    Kit {
        hd: picture("hd", 1920, 1080, "yuv420p", None),
        hd2: picture("hd2", 1280, 720, "yuv420p", Some("unknown")),
        phone: picture("phone", 1920, 1080, "yuv420p", Some("bt709")),
        obs: picture("obs", 1920, 1080, "yuvj420p", Some("bt709")),
        hlg: hdr(picture("hlg", 3840, 2160, "yuv420p10le", None), "arib-std-b67"),
        sd: picture("sd", 720, 576, "yuv420p", None),
        sd601: picture("sd601", 720, 576, "yuv420p", Some("smpte170m")),
        hd601: picture("hd601", 1920, 1080, "yuv420p", Some("smpte170m")),
        alpha: picture("alpha", 1920, 1080, "yuva420p", None),
        defaults,
    }
}

fn pip(a: &Asset, start: f64) -> Clip {
    let mut c = make_clip(a.id, 0.0, 2.0, start);
    c.transform.scale = 0.5;
    c.transform.pos_x = 0.2;
    c
}

/// `base` full-frame on V1 with `top` as a picture-in-picture over it on V2.
fn stack(base: &Asset, top: &Asset) -> Timeline {
    let mut tl = timeline_of(vec![
        video_track(vec![make_clip(base.id, 0.0, 2.0, 0.0)]),
        video_track(vec![pip(top, 0.0)]),
    ]);
    tl.tracks[1].name = "V2".into();
    tl.format = Some(Delivery::new(640, 360, Fit::Contain));
    tl
}

/// `first` then `second` on one track, one second each.
fn sequence(first: &Asset, second: &Asset) -> Timeline {
    let mut tl = timeline_of(vec![video_track(vec![
        make_clip(first.id, 0.0, 1.0, 0.0),
        make_clip(second.id, 0.0, 1.0, 1.0),
    ])]);
    tl.format = Some(Delivery::new(640, 360, Fit::Contain));
    tl
}

fn filter_of(args: &[String]) -> String {
    let at = args.iter().position(|a| a == "-filter_complex").expect("a filtergraph");
    args[at + 1].clone()
}

/// The chains of a graph that end on a `[vN]` pad, by pad: `(N, chain)`.
fn chains(graph: &str) -> Vec<(String, String)> {
    graph
        .split(';')
        .filter_map(|segment| {
            let (head, pad) = segment.trim_end().rsplit_once('[')?;
            let pad = pad.strip_suffix(']')?;
            let n = pad
                .strip_prefix('v')
                .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))?;
            let chain = head.split_once(']').map_or(head, |(_, c)| c);
            Some((n.to_string(), chain.to_string()))
        })
        .collect()
}

fn export_graph(tl: &Timeline, assets: &[Asset]) -> String {
    filter_of(&build_export_args(tl, assets, "/o.mp4", &ExportOptions::default()).expect("export"))
}

fn export_graph_range(tl: &Timeline, assets: &[Asset], from: f64, to: f64) -> String {
    let opts = ExportOptions {
        range: Some(crate::model::TimeRange { start: from, end: to }),
        ..ExportOptions::default()
    };
    filter_of(&build_export_args(tl, assets, "/o.mp4", &opts).expect("export"))
}

fn still_graph(tl: &Timeline, assets: &[Asset], t: f64) -> String {
    filter_of(&build_still_args(tl, assets, &ExportOptions::default(), t, 320, None, &StillOutput::RgbPipe).expect("still"))
}

fn preview_graph(tl: &Timeline, assets: &[Asset], start: f64) -> String {
    filter_of(&build_preview_args_with(tl, assets, start, 10.0, 320, 4, None).expect("preview"))
}

/// The scrub window's graph for the frames `[a, b)` of the cut: the export's own over a short slice
/// of it (`Snapshot::window_args`, the builder the scrub cache's window runs).
fn window_graph(tl: &Timeline, assets: &[Asset], a: u64, b: u64) -> String {
    let snap = super::scrub::Snapshot::new(tl, assets, 320, 4);
    filter_of(&snap.window_args(a, b, None).expect("a window"))
}

/// The four graphs the label can be in, for `tl` as a whole: `(name, graph)`.
fn graphs(tl: &Timeline, assets: &[Asset]) -> Vec<(&'static str, String)> {
    vec![
        ("export", export_graph(tl, assets)),
        ("still", still_graph(tl, assets, 0.5)),
        ("preview", preview_graph(tl, assets, 0.0)),
        ("window", window_graph(tl, assets, 0, 20)),
    ]
}

/// How many chains of `graph` are labelled.
fn labelled(graph: &str) -> usize {
    chains(graph).iter().filter(|(_, chain)| chain.contains(LABEL)).count()
}

#[test]
fn an_untagged_hd_clip_is_labelled_beside_a_tagged_one_in_all_four_builders() {
    let k = kit();
    for (top, base, what) in [
        (&k.hd, &k.phone, "untagged HD over a phone's BT.709 clip"),
        (&k.phone, &k.hd, "a BT.709 clip over untagged HD"),
        (&k.hd, &k.hlg, "untagged HD over HLG"),
        (
            &k.hd,
            &k.obs,
            "untagged HD over a full-range BT.709 H.264 clip (yuvj420p, stated bt709)",
        ),
        (&k.obs, &k.hd, "a full-range BT.709 H.264 clip over untagged HD"),
        (&k.hd, &k.hd601, "untagged HD over HD tagged smpte170m"),
        (&k.hd, &k.sd601, "untagged HD over SD tagged smpte170m"),
    ] {
        let assets = [top.clone(), base.clone()];
        let tl = stack(base, top);
        for (builder, graph) in graphs(&tl, &assets) {
            let found = chains(&graph);
            assert_eq!(found.len(), 2, "{what}, {builder}: {graph}");
            assert_eq!(
                labelled(&graph),
                1,
                "{what}, {builder}: exactly the untagged HD clip: {graph}"
            );
            // The untagged HD clip is the labelled one (never its neighbour), whichever way round.
            let hd_pad = if top.name == "hd" { "1" } else { "0" };
            let chain = &found.iter().find(|(n, _)| n == hd_pad).expect("the HD clip's chain").1;
            assert!(chain.contains(LABEL), "{what}, {builder}: {chain}");
        }
    }
}

#[test]
fn the_label_is_the_first_filter_of_the_chain_so_nothing_converts_the_picture_ahead_of_it() {
    let k = kit();
    let mut over = pip(&k.hd, 0.0);
    over.transform.opacity = 0.6;
    over.color.saturation = 1.3;
    let mut tl = stack(&k.phone, &k.hd);
    tl.tracks[1].clips = vec![over];
    let assets = [k.hd.clone(), k.phone];
    let at = |chain: &str, f: &str| chain.find(f).unwrap_or_else(|| panic!("`{f}` in {chain}"));
    for (builder, graph) in graphs(&tl, &assets) {
        let chain = chains(&graph)
            .into_iter()
            .find(|(_, c)| c.contains(LABEL))
            .expect("labelled")
            .1;
        // Ahead of every scaler: the fit `scale` is the filter FFmpeg asks for the RGB a dip to
        // white's `fade` needs, so a label behind it states the matrix of a picture already converted.
        assert!(chain.starts_with(&format!("{LABEL},trim=")), "{builder}: {chain}");
        assert!(at(&chain, LABEL) < at(&chain, "scale="), "{builder}: {chain}");
        assert!(at(&chain, LABEL) < at(&chain, "eq="), "{builder}: {chain}");
        assert!(at(&chain, LABEL) < at(&chain, "colorchannelmixer="), "{builder}: {chain}");
        assert!(at(&chain, LABEL) < at(&chain, "format="), "{builder}: {chain}");
        // Once, and as `setparams` states it: never the output option that converts on FFmpeg 9.
        assert_eq!(chain.matches("setparams").count(), 1, "{builder}: {chain}");
        assert!(!graph.contains("-colorspace"), "{builder}");
    }
}

#[test]
fn nothing_is_labelled_without_a_matrix_named_in_the_graph() {
    let k = kit();
    // A single clip of every kind, in every builder.
    let mut every = vec![&k.hd, &k.hd2, &k.phone, &k.obs, &k.hlg, &k.sd, &k.sd601];
    every.extend(k.others());
    for a in every {
        let tl = timeline_of(vec![video_track(vec![make_clip(a.id, 0.0, 1.0, 0.0)])]);
        for (builder, graph) in graphs(&tl, std::slice::from_ref(a)) {
            assert_eq!(labelled(&graph), 0, "{} alone, {builder}: {graph}", a.name);
        }
    }
    // Untagged footage only, and footage whose only tag is its decoder's default (a PNG logo's `gbr`, a
    // JPEG's or WebP's or an MJPEG webcam's `bt470bg`): HD with HD, with SD, with alpha, with every one
    // of the shapes the probe gives, each way round. Nothing here names a matrix: the graph is the one
    // it always was.
    let mut beside = vec![&k.hd2];
    beside.extend(k.others());
    for other in &beside {
        for (top, base) in [(&k.hd, *other), (*other, &k.hd)] {
            let tl = stack(base, top);
            for (builder, graph) in graphs(&tl, &[top.clone(), base.clone()]) {
                assert_eq!(labelled(&graph), 0, "{} with {}, {builder}: {graph}", top.name, base.name);
            }
        }
    }
    // The same with two HD clips and a shape between them, and a translucent shape on top: the decoder's
    // tag of a logo or a photo moves no HD graph (the export's VUI included: nothing is tagged).
    for other in k.defaults.iter().chain([&k.alpha]) {
        let mut tl = stack(&k.hd, &k.hd2);
        tl.tracks[1].clips[0].transform.opacity = 0.5;
        tl.tracks.push(crate::model::Track {
            clips: vec![pip(other, 0.0)],
            ..crate::model::Track::new(StreamKind::Video, "V3")
        });
        for (builder, graph) in graphs(&tl, &[k.hd.clone(), k.hd2.clone(), other.clone()]) {
            assert_eq!(labelled(&graph), 0, "{} on two HD clips, {builder}: {graph}", other.name);
        }
    }
    // Beside a named matrix, the kinds that are not untagged HD YCbCr are left as they are.
    for other in k.others() {
        for (top, base) in [(&k.phone, other), (other, &k.phone)] {
            let tl = stack(base, top);
            for (builder, graph) in graphs(&tl, &[top.clone(), base.clone()]) {
                assert_eq!(labelled(&graph), 0, "{} beside a BT.709 clip, {builder}: {graph}", other.name);
            }
        }
    }
    // A clip of a probe that never recorded the pixel format is not known to be YCbCr.
    let mut unrecorded = k.hd.clone();
    unrecorded.streams[0].pix_fmt = None;
    let tl = stack(&k.phone, &unrecorded);
    for (builder, graph) in graphs(&tl, &[unrecorded, k.phone]) {
        assert_eq!(labelled(&graph), 0, "{builder}: {graph}");
    }
}

#[test]
fn a_graph_without_a_labelled_clip_is_the_graph_it_always_was() {
    // No `setparams` anywhere, and none of the label's text: a project without a mixed stack is
    // byte-identical (the golden oracle holds this for the whole corpus).
    let k = kit();
    let assets = [k.hd.clone(), k.sd.clone()];
    let tl = stack(&k.sd, &k.hd);
    for (builder, graph) in graphs(&tl, &assets) {
        assert!(!graph.contains("setparams"), "{builder}: {graph}");
    }
}

#[test]
fn the_label_is_the_graphs_not_the_frames() {
    let k = kit();
    let assets = [k.phone.clone(), k.hd.clone()];
    // A BT.709 clip, then an untagged HD one: never on screen together.
    let tl = sequence(&k.phone, &k.hd);
    // The export holds both, so it labels the HD clip even for the frames the other is not on.
    assert_eq!(labelled(&export_graph(&tl, &assets)), 1);
    assert_eq!(labelled(&preview_graph(&tl, &assets, 0.0)), 1);
    assert_eq!(labelled(&window_graph(&tl, &assets, 20, 30)), 1);
    // A still holds the layers on screen: the HD clip alone, at 1.5 s.
    assert_eq!(labelled(&still_graph(&tl, &assets, 1.5)), 0);
    // A stream (or a window, or a range export) that starts past the tagged clip does not hold it.
    assert_eq!(labelled(&preview_graph(&tl, &assets, 1.2)), 0);
    assert_eq!(labelled(&window_graph(&tl, &assets, 40, 50)), 0);
    // A window is a graph of its own: one that spans the cut holds both, one inside either does not.
    assert_eq!(labelled(&window_graph(&tl, &assets, 24, 36)), 1);
    assert_eq!(labelled(&window_graph(&tl, &assets, 0, 20)), 0);
    assert_eq!(labelled(&export_graph_range(&tl, &assets, 1.1, 2.0)), 0);
    assert_eq!(labelled(&export_graph_range(&tl, &assets, 0.0, 2.0)), 1);
    // …and a tagged clip a cut away does not make a still of the other one labelled.
    let stacked = stack(&k.phone, &k.hd);
    assert_eq!(labelled(&still_graph(&stacked, &assets, 0.5)), 1);
    assert_eq!(
        labelled(&still_graph(&stacked, &assets, 2.5)),
        0,
        "past both clips there is no layer"
    );
}

#[test]
fn a_muted_or_disabled_tagged_clip_is_not_in_the_graph() {
    let k = kit();
    let assets = [k.phone.clone(), k.hd.clone()];
    let mut tl = stack(&k.phone, &k.hd);
    assert_eq!(labelled(&export_graph(&tl, &assets)), 1);
    tl.tracks[0].muted = true;
    assert_eq!(labelled(&export_graph(&tl, &assets)), 0);
    tl.tracks[0].muted = false;
    tl.tracks[0].clips[0].enabled = false;
    for (builder, graph) in graphs(&tl, &assets) {
        assert_eq!(labelled(&graph), 0, "{builder}: {graph}");
    }
}

// ---- the plan mirrors the builders --------------------------------------------------------------

const POLICIES: [CompositeColorPolicy; 3] = [
    CompositeColorPolicy::FixedBt601,
    CompositeColorPolicy::BottomLayerTag,
    CompositeColorPolicy::Unknown,
];

/// Which layers of a plan are labelled, by clip.
fn plan_labels(plan: &RenderPlan) -> Vec<(uuid::Uuid, bool)> {
    plan.layers.iter().map(|l| (l.clip_id, l.stream.label_bt709)).collect()
}

/// Which clips of `tl` the graph labels (by pad `v<flat>`), as clip ids.
fn graph_labels(tl: &Timeline, graph: &str) -> Vec<(uuid::Uuid, bool)> {
    let flat: Vec<&Clip> = tl.tracks.iter().flat_map(|t| t.clips.iter()).collect();
    chains(graph)
        .into_iter()
        .map(|(n, chain)| (flat[n.parse::<usize>().unwrap()].id, chain.contains(LABEL)))
        .collect()
}

/// Every pairing of two kinds of footage, each way round, with and without a third that names nothing.
fn pairings(k: &Kit) -> Vec<(String, Timeline, Vec<Asset>)> {
    let mut all = vec![&k.hd, &k.hd2, &k.phone, &k.obs, &k.hlg, &k.sd, &k.sd601, &k.hd601];
    all.extend(k.others());
    let mut out = Vec::new();
    for base in all.iter().copied() {
        for top in all.iter().copied() {
            if base.id != top.id {
                out.push((
                    format!("{} over {}", top.name, base.name),
                    stack(base, top),
                    vec![base.clone(), top.clone()],
                ));
            }
        }
    }
    out
}

#[test]
fn a_still_plan_labels_the_layers_the_still_graph_labels() {
    let k = kit();
    for (what, tl, assets) in pairings(&k) {
        let graph = still_graph(&tl, &assets, 0.5);
        for policy in POLICIES {
            let plan = RenderPlan::at(&tl, &assets, &ExportOptions::default(), 0.5, policy).unwrap();
            assert_eq!(
                plan_labels(&plan),
                graph_labels(&tl.for_render(), &graph_in_layer_order(&tl, &graph)),
                "{what}"
            );
        }
    }
}

/// A still's pads are numbered by layer, not by storage: renumber them as the timeline's clips so
/// [`graph_labels`] can read either graph.
fn graph_in_layer_order(tl: &Timeline, graph: &str) -> String {
    let order: Vec<usize> = {
        let flat: Vec<&Clip> = tl.tracks.iter().flat_map(|t| t.clips.iter()).collect();
        // Composite order: tracks in list order, clips within a track in timeline order.
        let mut ids = Vec::new();
        let mut base = 0;
        for track in &tl.tracks {
            let mut order: Vec<usize> = (0..track.clips.len()).collect();
            order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
            ids.extend(order.into_iter().map(|j| base + j));
            base += track.clips.len();
        }
        assert_eq!(ids.len(), flat.len());
        ids
    };
    // The still numbers its layers 0.. in composite order, which holds only the clips on screen at
    // t = 0.5: every clip of these timelines starts at 0, so all of them.
    chains(graph)
        .into_iter()
        .map(|(n, chain)| format!("[x]{chain}[v{}]", order[n.parse::<usize>().unwrap()]))
        .collect::<Vec<_>>()
        .join(";")
}

#[test]
fn an_export_plan_labels_the_clips_the_export_graph_labels() {
    let k = kit();
    for (what, tl, assets) in pairings(&k) {
        let graph = export_graph(&tl, &assets);
        for policy in POLICIES {
            let planner = Planner::new(&tl, &assets, &ExportOptions::default(), PlanRequest::motion(policy)).unwrap();
            let plan = planner.at_frame(3).unwrap();
            let mut want = graph_labels(&tl.for_render(), &graph);
            let mut got = plan_labels(&plan);
            want.sort();
            got.sort();
            assert_eq!(got, want, "{what}");
        }
    }
}

#[test]
fn the_plan_decides_a_still_over_the_layers_on_screen_and_an_export_over_the_cut() {
    let k = kit();
    let assets = [k.phone.clone(), k.hd.clone()];
    let tl = sequence(&k.phone, &k.hd);
    let policy = CompositeColorPolicy::BottomLayerTag;
    // Frame 45 of 30 fps (1.5 s): the HD clip alone. The export graph holds the phone's clip too and
    // labels it; the still holds only what is on screen and does not.
    let still = RenderPlan::at(&tl, &assets, &ExportOptions::default(), 1.5, policy).unwrap();
    let motion = Planner::new(&tl, &assets, &ExportOptions::default(), PlanRequest::motion(policy))
        .unwrap()
        .at_frame(45)
        .unwrap();
    assert_eq!(plan_labels(&still).iter().map(|l| l.1).collect::<Vec<_>>(), [false]);
    assert_eq!(plan_labels(&motion).iter().map(|l| l.1).collect::<Vec<_>>(), [true]);
    assert_eq!(still.layers[0].stream.matrix(), Some(YuvMatrix::Bt601));
    assert_eq!(motion.layers[0].stream.matrix(), Some(YuvMatrix::Bt709));
    assert_eq!(
        (still.canvas.matrix, motion.canvas.matrix),
        (YuvMatrix::Bt601, YuvMatrix::Bt709)
    );
    // …which is what the builders say for the same frames.
    assert_eq!(labelled(&still_graph(&tl, &assets, 1.5)), 0);
    assert_eq!(labelled(&export_graph(&tl, &assets)), 1);
}

#[test]
fn a_labelled_layer_is_bt709_to_the_plan_and_so_stacks_with_a_bt709_one() {
    let k = kit();
    let assets = [k.phone.clone(), k.hd.clone()];
    let tl = stack(&k.phone, &k.hd);
    let at = |policy| RenderPlan::at(&tl, &assets, &ExportOptions::default(), 0.5, policy).unwrap();
    // FFmpeg 9: both layers are BT.709, one matrix, no conversion left to reproduce.
    let p = at(CompositeColorPolicy::BottomLayerTag);
    assert_eq!(
        p.layers.iter().map(|l| l.stream.matrix()).collect::<Vec<_>>(),
        [Some(YuvMatrix::Bt709); 2]
    );
    let (matrix, why) = composite_matrix(&p.layers, p.canvas.policy);
    assert_eq!((matrix, why.is_empty()), (YuvMatrix::Bt709, true), "{why:?}");
    assert_eq!(p.canvas.matrix, YuvMatrix::Bt709);
    // FFmpeg 6: the composite is BT.601 whatever the layers say, and the labelled layer's own
    // matrix (a translucent one's way out of YUV) is BT.709, as the tag would make it.
    let p = at(CompositeColorPolicy::FixedBt601);
    assert_eq!(p.canvas.matrix, YuvMatrix::Bt601);
    assert_eq!(p.layers[1].stream.matrix(), Some(YuvMatrix::Bt709));
    // An unmeasured policy draws BT.601 stacks only: a BT.709 one is still refused.
    let (_, why) = composite_matrix(&at(CompositeColorPolicy::Unknown).layers, CompositeColorPolicy::Unknown);
    assert!(
        why.iter()
            .any(|w| matches!(w, crate::plan_caps::Unsupported::UnmeasuredPolicy(YuvMatrix::Bt709))),
        "{why:?}"
    );
    // The same stack without the label is the mixed one it was: untagged HD reads as BT.601.
    let unlabelled = stack(&k.sd, &k.phone);
    let assets = [k.sd.clone(), k.phone];
    let p = RenderPlan::at(
        &unlabelled,
        &assets,
        &ExportOptions::default(),
        0.5,
        CompositeColorPolicy::BottomLayerTag,
    )
    .unwrap();
    let (_, why) = composite_matrix(&p.layers, p.canvas.policy);
    assert!(
        why.iter().any(|w| matches!(w, crate::plan_caps::Unsupported::MixedMatrices)),
        "{why:?}"
    );
}

#[test]
fn untagged_hd_beside_a_601_tag_is_labelled_and_so_a_mixed_stack_the_plan_refuses() {
    // The label states what a player shows. Beside `smpte170m` it makes the stack genuinely mixed
    // (BT.709 against BT.601), which FFmpeg 9 converts and the compositor does not reproduce; it was
    // drawn before as two layers FFmpeg read alike. It is still drawn where the composite is fixed.
    let k = kit();
    for base in [&k.sd601, &k.hd601] {
        let assets = [base.clone(), k.hd.clone()];
        let tl = stack(base, &k.hd);
        let p = RenderPlan::at(
            &tl,
            &assets,
            &ExportOptions::default(),
            0.5,
            CompositeColorPolicy::BottomLayerTag,
        )
        .unwrap();
        let (_, why) = composite_matrix(&p.layers, p.canvas.policy);
        assert!(
            why.iter().any(|w| matches!(w, crate::plan_caps::Unsupported::MixedMatrices)),
            "{}: {why:?}",
            base.name
        );
        let p = RenderPlan::at(&tl, &assets, &ExportOptions::default(), 0.5, CompositeColorPolicy::FixedBt601).unwrap();
        assert!(composite_matrix(&p.layers, p.canvas.policy).1.is_empty());
    }
}

/// An asset decodes from a proxy of the size and format `video` says (its tags are the original's).
#[derive(Debug)]
struct Proxy(StreamInfo);

impl crate::media::MediaResolver for Proxy {
    fn resolve(&self, asset: &Asset) -> crate::media::SourceMedia {
        crate::media::SourceMedia {
            path: format!("/home/u/.cache/kerf/proxies/{:016x}.mp4", asset.id.as_u128() as u64),
            proxy: true,
            video: Some(self.0.clone()),
        }
    }
}

#[test]
fn the_label_is_decided_from_the_original_not_from_a_proxy_that_may_be_smaller_than_hd() {
    // A small proxy size (Settings) makes the proxy of a 1080p clip 640x360: SD by the rule, while its
    // original is HD and the graphs the preview and the export build are decided from the original.
    let k = kit();
    let assets = [k.phone.clone(), k.hd.clone()];
    let tl = stack(&k.phone, &k.hd);
    let mut small = k.hd.streams[0].clone();
    (small.width, small.height) = (Some(640), Some(360));
    let proxy = Proxy(small);
    let motion = Planner::new(
        &tl,
        &assets,
        &ExportOptions::default(),
        PlanRequest::motion(CompositeColorPolicy::BottomLayerTag).with_media(&proxy),
    )
    .unwrap()
    .at_frame(3)
    .unwrap();
    let still = Planner::new(
        &tl,
        &assets,
        &ExportOptions::default(),
        PlanRequest::still(CompositeColorPolicy::BottomLayerTag).with_media(&proxy),
    )
    .unwrap()
    .at(0.5)
    .unwrap();
    for (mode, plan) in [("export", motion), ("still", still)] {
        let hd = plan.layers.iter().find(|l| l.asset_id == k.hd.id).unwrap();
        assert!(
            hd.source.proxy && hd.stream.width == 640,
            "{mode}: the layer describes the proxy"
        );
        assert!(hd.stream.label_bt709, "{mode}: and is labelled as its original is");
        assert_eq!(hd.stream.matrix(), Some(YuvMatrix::Bt709), "{mode}");
        // The tagged clip's proxy is no more labelled than its original.
        let phone = plan.layers.iter().find(|l| l.asset_id == k.phone.id).unwrap();
        assert!(!phone.stream.label_bt709, "{mode}");
    }
}

#[test]
fn a_reserved_matrix_is_no_tag_to_the_graph_and_bt601_to_the_plan() {
    assert_eq!(known_color(Some("reserved")), None);
    assert_eq!(known_color(Some("unspecified")), None);
    assert_eq!(known_color(Some("bt709")).as_deref(), Some("bt709"));
    // An asset saved before the probe dropped it still says `reserved`: it names nothing, so an HD
    // clip of it is labelled beside a tagged one, and a stack of it is BT.601 to the plan.
    let k = kit();
    let mut reserved = k.hd.clone();
    reserved.streams[0].color_space = Some("reserved".into());
    let assets = [k.phone.clone(), reserved.clone()];
    assert_eq!(labelled(&export_graph(&stack(&k.phone, &reserved), &assets)), 1);
    let alone = timeline_of(vec![video_track(vec![make_clip(reserved.id, 0.0, 1.0, 0.0)])]);
    let p = RenderPlan::at(
        &alone,
        std::slice::from_ref(&reserved),
        &ExportOptions::default(),
        0.5,
        CompositeColorPolicy::BottomLayerTag,
    )
    .unwrap();
    assert_eq!(p.layers[0].stream.matrix(), Some(YuvMatrix::Bt601));
    assert_eq!(p.canvas.matrix, YuvMatrix::Bt601);
}

// ---- the real binary (`#[ignore]`d) ---------------------------------------------------------------

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::engine::test_support::{ProxyGuard, StatusBounded};
use crate::project::Project;

const W: u32 = 640;
const H: u32 = 360;
const FPS: f64 = 10.0;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-bt709-label-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A one second lossless clip of `source` (a lavfi graph at `size`), encoded with the values of
/// `matrix` and labelled `label`: the footage as a camera labels it.
fn footage(dir: &Path, name: &str, source: &str, matrix: &str, label: &str) -> Asset {
    let path = dir.join(format!("{name}.mp4"));
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!(
            "{source},format=gbrp,scale=out_color_matrix={matrix}:out_range=tv,format=yuv420p,{label}"
        ))
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-qp",
            "0",
            "-g",
            "5",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&path)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success(), "ffmpeg could not make {name}");
    Project::probe_asset(&path).expect("probe")
}

const UNTAGGED: &str = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
const BT709: &str = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709";
const GRADIENT: &str = "gradients=size=1280x720:rate=10:duration=1:c0=0xd03020:c1=0x2040e0:c2=0x30c060:nb_colors=3:seed=11:x0=0:y0=0:x1=1280:y1=720:speed=0.00001";
const DETAIL: &str = "testsrc2=size=1280x720:rate=10:duration=1";

/// The export of `tl` as raw `yuv420p` frames at the suite's size and rate.
fn export_yuv(tl: &Timeline, assets: &[Asset], dir: &Path, tag: &str) -> Vec<u8> {
    let opts = ExportOptions {
        resolution: Some((W, H)),
        fps: Some(FPS),
        ..ExportOptions::default()
    };
    let out = dir.join(format!("{tag}.yuv"));
    let mut args = build_export_args(tl, assets, "unused.mkv", &opts).unwrap();
    let graph = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
    args.truncate(graph + 1);
    args.extend(
        [
            "-map",
            "[outv]",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-y",
        ]
        .map(String::from),
    );
    args.push(out.to_string_lossy().into_owned());
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    let raw = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    raw
}

/// The still of `tl` at `t` as a PNG's RGB, the composite as the preview shows it.
fn still_rgb(tl: &Timeline, assets: &[Asset], t: f64, dir: &Path, tag: &str) -> Vec<u8> {
    let png = dir.join(format!("{tag}.png"));
    export_still(tl, assets, &ExportOptions::default(), t, &png, ImageFormat::Png, 0).expect("still");
    let run = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&png)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(run.status.success());
    let _ = std::fs::remove_file(&png);
    run.stdout
}

/// The first frame of the playback stream from `start`, as RGB.
fn stream_rgb(tl: &Timeline, assets: &[Asset], dir: &Path, tag: &str) -> Vec<u8> {
    let mut first = None;
    stream_preview(tl, assets, 0.0, FPS, &mut |f| {
        first = Some(f.jpeg);
        false
    })
    .expect("stream");
    let jpg = dir.join(format!("{tag}.jpg"));
    std::fs::write(&jpg, first.expect("a frame")).unwrap();
    let run = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&jpg)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(run.status.success());
    let _ = std::fs::remove_file(&jpg);
    run.stdout
}

/// The first frame of the scrub window `[0, 5)` as RGB.
fn window_rgb(tl: &Timeline, assets: &[Asset], dir: &Path, tag: &str) -> Vec<u8> {
    let snap = super::scrub::Snapshot::new(tl, assets, W, 4);
    let args = snap.window_args(0, 5, None).expect("a window");
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    let (s, e) = next_jpeg(&run.stdout).expect("a frame");
    let jpg = dir.join(format!("{tag}.jpg"));
    std::fs::write(&jpg, &run.stdout[s..e]).unwrap();
    let rgb = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&jpg)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(rgb.status.success());
    let _ = std::fs::remove_file(&jpg);
    rgb.stdout
}

/// PSNR in dB (99 for identical) and the worst error of two same-sized buffers.
fn psnr(a: &[u8], b: &[u8]) -> (f64, u8) {
    assert_eq!(a.len(), b.len(), "buffers of another size");
    let worst = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    let se: f64 = a.iter().zip(b).map(|(x, y)| f64::from(x.abs_diff(*y)).powi(2)).sum();
    if se == 0.0 {
        return (99.0, 0);
    }
    (10.0 * (255.0 * 255.0 / (se / a.len() as f64)).log10(), worst)
}

fn plain(_: &mut Clip) {}

fn translucent(c: &mut Clip) {
    c.transform.opacity = 0.5;
}

fn stack_of(base: &Asset, top: &Asset, dress: fn(&mut Clip)) -> (Timeline, Vec<Asset>) {
    let mut tl = stack(base, top);
    tl.tracks[0].clips[0].source_out = 1.0;
    tl.tracks[1].clips[0].source_out = 1.0;
    dress(&mut tl.tracks[1].clips[0]);
    (tl, vec![base.clone(), top.clone()])
}

/// `cargo test -p kerf-core --no-default-features -- --ignored a_labelled_input`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_labelled_input_renders_as_a_file_tagged_bt709_is_in_all_four_builders() {
    let dir = scratch("tagged");
    let untagged = footage(&dir, "untagged", GRADIENT, "bt709", UNTAGGED);
    let tagged = footage(&dir, "tagged", GRADIENT, "bt709", BT709);
    let phone = footage(&dir, "phone", DETAIL, "bt709", BT709);
    eprintln!("composite colour policy: {:?}", composite_color_policy());
    // What the chain does after the label, one at a time: each is a filter that may ask a scaler for
    // another pixel format, which is why the label comes first.
    let graded: fn(&mut Clip) = |c| {
        c.color.saturation = 1.4;
        c.color.brightness = 0.05;
    };
    let rotated: fn(&mut Clip) = |c| c.transform.rotation = 12.0;
    let vignetted: fn(&mut Clip) = |c| c.effects.push(crate::model::VideoEffect::Vignette);
    let faded: fn(&mut Clip) = |c| c.fade_in = 0.8;
    for (what, dress, top_first) in [
        ("untagged HD over a BT.709 clip", plain as fn(&mut Clip), true),
        ("a translucent untagged HD clip over a BT.709 clip", translucent, true),
        ("a graded untagged HD clip over a BT.709 clip", graded, true),
        ("a rotated untagged HD clip over a BT.709 clip", rotated, true),
        ("a vignetted untagged HD clip over a BT.709 clip", vignetted, true),
        ("a fading untagged HD clip over a BT.709 clip", faded, true),
        ("a BT.709 clip over untagged HD", plain, false),
        ("a translucent BT.709 clip over untagged HD", translucent, false),
    ] {
        // The labelled stack, and the same stack with the footage tagged in the file.
        let build = |hd: &Asset| {
            if top_first {
                stack_of(&phone, hd, dress)
            } else {
                stack_of(hd, &phone, dress)
            }
        };
        let ((a_tl, a_assets), (b_tl, b_assets)) = (build(&untagged), build(&tagged));
        assert_eq!(labelled(&export_graph(&a_tl, &a_assets)), 1, "{what}: the engine labels it");
        assert_eq!(
            labelled(&export_graph(&b_tl, &b_assets)),
            0,
            "{what}: the tagged file needs no label"
        );
        let tag = what.replace(' ', "-");
        let figures = [
            (
                "export",
                psnr(
                    &export_yuv(&a_tl, &a_assets, &dir, &format!("{tag}-a")),
                    &export_yuv(&b_tl, &b_assets, &dir, &format!("{tag}-b")),
                ),
            ),
            (
                "still",
                psnr(
                    &still_rgb(&a_tl, &a_assets, 0.5, &dir, &format!("{tag}-a")),
                    &still_rgb(&b_tl, &b_assets, 0.5, &dir, &format!("{tag}-b")),
                ),
            ),
            (
                "stream",
                psnr(
                    &stream_rgb(&a_tl, &a_assets, &dir, &format!("{tag}-a")),
                    &stream_rgb(&b_tl, &b_assets, &dir, &format!("{tag}-b")),
                ),
            ),
            (
                "scrub window",
                psnr(
                    &window_rgb(&a_tl, &a_assets, &dir, &format!("{tag}-a")),
                    &window_rgb(&b_tl, &b_assets, &dir, &format!("{tag}-b")),
                ),
            ),
        ];
        eprintln!("{what}: {figures:?}");
        // Every picture the label changes is the one the tagged file gives, to the bit.
        for (builder, (db, worst)) in figures {
            assert_eq!((db, worst), (99.0, 0), "{what}: {builder} differs from the tagged twin");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `cargo test -p kerf-core --no-default-features -- --ignored a_labelled_clip_dips`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_labelled_clip_dips_through_white_as_a_file_tagged_bt709_does() {
    // A dip to white is a `fade` that only takes RGB, so FFmpeg has the nearest scaler of the chain
    // convert the picture there: a label behind the fit `scale` left that conversion BT.601 and the
    // one back BT.709 (8 to 15 levels off the tagged file, every frame of the clip).
    use crate::model::{Transition, TransitionKind};
    let dir = scratch("dip");
    let untagged = footage(&dir, "untagged", GRADIENT, "bt709", UNTAGGED);
    let tagged = footage(&dir, "tagged", GRADIENT, "bt709", BT709);
    let phone = footage(&dir, "phone", DETAIL, "bt709", BT709);
    for (what, hd_first) in [("a dip into untagged HD", false), ("a dip out of untagged HD", true)] {
        let build = |hd: &Asset| {
            let mut tl = if hd_first {
                sequence(hd, &phone)
            } else {
                sequence(&phone, hd)
            };
            tl.tracks[0].clips[1].transition_in = Some(Transition {
                kind: TransitionKind::DipToWhite,
                duration: 0.6,
            });
            (tl, vec![phone.clone(), hd.clone()])
        };
        let ((a_tl, a_assets), (b_tl, b_assets)) = (build(&untagged), build(&tagged));
        assert_eq!(labelled(&export_graph(&a_tl, &a_assets)), 1, "{what}: the engine labels it");
        assert_eq!(
            labelled(&export_graph(&b_tl, &b_assets)),
            0,
            "{what}: the tagged file needs no label"
        );
        let tag = what.replace(' ', "-");
        let (db, worst) = psnr(
            &export_yuv(&a_tl, &a_assets, &dir, &format!("{tag}-a")),
            &export_yuv(&b_tl, &b_assets, &dir, &format!("{tag}-b")),
        );
        eprintln!("{what}: export {db:.1} dB, worst {worst}");
        assert_eq!((db, worst), (99.0, 0), "{what}: the export differs from the tagged twin");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `cargo test -p kerf-core --no-default-features -- --ignored a_mixed_stack_of_proxies`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_mixed_stack_of_proxies_is_the_stack_of_the_originals() {
    let dir = scratch("proxies");
    let untagged = footage(&dir, "untagged", GRADIENT, "bt709", UNTAGGED);
    let phone = footage(&dir, "phone", DETAIL, "bt709", BT709);
    let sd = footage(&dir, "sd", "testsrc2=size=720x576:rate=10:duration=1", "bt601", UNTAGGED);
    let width = crate::engine::proxy_width(None);
    let proxies: Vec<_> = [&untagged, &phone, &sd]
        .iter()
        .map(|a| ProxyGuard(generate_proxy(Path::new(&a.path), width).expect("proxy")))
        .collect();
    assert!(proxies.iter().all(|p| p.0.is_file()));
    let preview = |assets: &[Asset]| Project::preview_assets_of(assets.to_vec());
    for (what, dress, hd_on_top, other) in [
        ("untagged HD over a BT.709 clip", plain as fn(&mut Clip), true, &phone),
        ("a translucent untagged HD clip over a BT.709 clip", translucent, true, &phone),
        ("a BT.709 clip over untagged HD", plain, false, &phone),
        ("untagged HD over untagged SD", plain, true, &sd),
    ] {
        let (tl, assets) = if hd_on_top {
            stack_of(other, &untagged, dress)
        } else {
            stack_of(&untagged, other, dress)
        };
        let seen = preview(&assets);
        assert!(
            seen.iter().zip(&assets).all(|(p, a)| p.path != a.path),
            "{what}: the preview decodes proxies"
        );
        // The preview's assets carry the original's tags, so the graph labels exactly as the export's.
        assert_eq!(
            labelled(&export_graph(&tl, &seen)),
            labelled(&export_graph(&tl, &assets)),
            "{what}"
        );
        let tag = what.replace(' ', "-");
        let export = psnr(
            &export_yuv(&tl, &assets, &dir, &format!("{tag}-o")),
            &export_yuv(&tl, &seen, &dir, &format!("{tag}-p")),
        );
        let still = psnr(
            &still_rgb(&tl, &assets, 0.5, &dir, &format!("{tag}-o")),
            &still_rgb(&tl, &seen, 0.5, &dir, &format!("{tag}-p")),
        );
        let stream = psnr(
            &stream_rgb(&tl, &assets, &dir, &format!("{tag}-o")),
            &stream_rgb(&tl, &seen, &dir, &format!("{tag}-p")),
        );
        eprintln!("{what}: export {export:?}, still {still:?}, stream {stream:?}");
        // What is left is the proxy's own encode (a lossy x264 at CRF, 52.9 to 64.3 dB measured on the
        // export); a stack read in two matrices was 30 to 36 dB apart.
        assert!(export.0 > 48.0, "{what}: export {export:?}");
        assert!(still.0 > 44.0, "{what}: still {still:?}");
        assert!(stream.0 > 42.0, "{what}: stream {stream:?}");
    }
    drop(proxies);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Real files, probed (on 6.1.1 and 9.0.2 alike), whose only tag is their decoder's default: a JPEG and
/// a WebP say `bt470bg`, a PNG of any layout and RGB video say `gbr`, and the `mjpeg`, `speedhq` and
/// `amv` decoders say `bt470bg` of everything (`jpegls` does of `rgb24`). Beside untagged HD they name
/// no matrix: no builder labels anything, and the argv is the one the same file without the tag gives.
/// A full-range H.264 clip with a stated BT.709 (`h264 yuvj420p pc bt709`, what OBS and drones write) is
/// the file's word and does label the HD beside it: `yuvj` is not the decoder.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored real_files_whose_tag`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn real_files_whose_tag_is_the_decoders_default_name_no_matrix_beside_untagged_hd() {
    let dir = scratch("defaults");
    let hd = footage(&dir, "hd", GRADIENT, "bt709", UNTAGGED);
    let h264 = ["-c:v", "libx264", "-qp", "18"];
    let full_709 = [
        "-color_range",
        "pc",
        "-colorspace",
        "bt709",
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
    ];
    // (file, the pixel format it is made in, the encoder's arguments, whether its tag is the file's word)
    let files: [(&str, &str, Vec<&str>, bool); 13] = [
        ("photo.jpg", "yuvj420p", vec!["-frames:v", "1"], false),
        ("photo.webp", "yuv420p", vec!["-frames:v", "1", "-c:v", "libwebp"], false),
        ("logo-rgb24.png", "rgb24", vec!["-frames:v", "1"], false),
        ("logo-rgba.png", "rgba", vec!["-frames:v", "1"], false),
        ("grey.png", "gray", vec!["-frames:v", "1"], false),
        ("webcam.mkv", "yuvj420p", vec!["-c:v", "mjpeg", "-q:v", "3"], false),
        ("webcam-422.avi", "yuvj422p", vec!["-c:v", "mjpeg", "-q:v", "3"], false),
        (
            "webcam-422-limited.mkv",
            "yuv422p",
            vec!["-c:v", "mjpeg", "-q:v", "3", "-strict", "unofficial"],
            false,
        ),
        ("ndi.mov", "yuv422p", vec!["-c:v", "speedhq"], false),
        ("jpegls.avi", "rgb24", vec!["-c:v", "jpegls"], false),
        ("capture.mkv", "bgr0", vec!["-c:v", "ffv1"], false),
        ("capture-alpha.mkv", "bgra", vec!["-c:v", "ffv1"], false),
        (
            "obs-full-range.mp4",
            "yuv420p",
            h264.iter().chain(&full_709).copied().collect(),
            true,
        ),
    ];
    let (mut checked, mut named) = (0, 0);
    for (name, pix_fmt, encoder, names) in files {
        let path = dir.join(name);
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("{DETAIL},format={pix_fmt}"))
            .args(&encoder)
            .args(["-pix_fmt", pix_fmt])
            .arg(&path)
            .status_bounded()
            .expect("run ffmpeg");
        if !made.success() {
            eprintln!("skipped {name}: this ffmpeg cannot make it");
            continue;
        }
        let shape = Project::probe_asset(&path).expect("probe");
        let video = &shape.streams[0];
        eprintln!(
            "{name}: {} {} tag {:?} image {}",
            video.codec,
            video.pix_fmt.as_deref().unwrap_or("?"),
            video.color_space,
            video.image
        );
        if names {
            // The file's word: the untagged HD beside it is labelled, in every builder, whichever way round.
            assert!(
                video.names_a_matrix(),
                "{name}: a stated matrix on a full-range clip is the file's word"
            );
            for (top, base) in [(&hd, &shape), (&shape, &hd)] {
                let (tl, assets) = stack_of(base, top, plain);
                for (builder, graph) in graphs(&tl, &assets) {
                    assert_eq!(labelled(&graph), 1, "{name} beside untagged HD, {builder}");
                }
            }
            named += 1;
            continue;
        }
        assert!(!video.names_a_matrix(), "{name}: the probe's tag is the decoder's default");
        let mut twin = shape.clone();
        twin.streams[0].color_space = None;
        for (top, base) in [(&hd, &shape), (&shape, &hd)] {
            let (stacked, twinned) = (
                stack_of(base, top, plain),
                stack_of(
                    if base.id == shape.id { &twin } else { base },
                    if top.id == shape.id { &twin } else { top },
                    plain,
                ),
            );
            for ((builder, graph), (_, bare)) in graphs(&stacked.0, &stacked.1).into_iter().zip(graphs(&twinned.0, &twinned.1)) {
                assert_eq!(labelled(&graph), 0, "{name} beside untagged HD, {builder}");
                assert_eq!(graph, bare, "{name}: its tag changed the {builder} graph");
            }
        }
        checked += 1;
    }
    assert!(checked >= 8, "only {checked} files could be made");
    assert_eq!(named, 1, "the full-range BT.709 clip could not be made");
    let _ = std::fs::remove_dir_all(&dir);
}
