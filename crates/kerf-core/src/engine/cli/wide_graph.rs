//! A graph too wide for its threads or too long for argv: the caps and the script file.

use super::*;

/// Longest `-filter_complex` we are willing to hand over as an argv string.
///
/// An animated reframe's `sendcmd` list dwarfs an ordinary graph — a single
/// channel at 30 fps runs to roughly 50 KB a minute — and it is *argv*, not
/// ffmpeg, that gives out first: Linux caps one argument at 128 KiB
/// (`MAX_ARG_STRLEN`) and Windows caps the whole command line at 32767
/// characters, which is about ten seconds of animation. The threshold sits well
/// below both, since spilling to a file costs nothing.
pub const GRAPH_ARG_MAX: usize = 8192;

/// Index of the `-filter_complex` *value* when it is too long to pass in argv.
pub(super) fn oversized_graph_index(args: &[String]) -> Option<usize> {
    args.iter()
        .position(|a| a == "-filter_complex")
        .map(|i| i + 1)
        .filter(|&i| args.get(i).is_some_and(|g| g.len() > GRAPH_ARG_MAX))
}

/// A filtergraph spilled to a script file, removed when the render is done.
pub(super) struct GraphScript(pub(super) Option<PathBuf>);

impl Drop for GraphScript {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// The options this ffmpeg build knows that the engine's argv depends on, read
/// from `ffmpeg -h full` once per process. Only these booleans are kept — the help
/// text itself is over a megabyte. A binary that cannot run at all reads as
/// "knows none of them", which every probe below takes as the modern spelling: any
/// render on that binary is about to fail the same way regardless.
struct HelpFlags {
    filter_complex_script: bool,
    fps_mode: bool,
    vsync: bool,
}

fn help_flags() -> &'static HelpFlags {
    static FLAGS: OnceLock<HelpFlags> = OnceLock::new();
    FLAGS.get_or_init(|| {
        let help = output_bounded(
            command(&ffmpeg_bin()).args(["-hide_banner", "-loglevel", "quiet", "-h", "full"]),
            CAPABILITY_PROBE_TIMEOUT,
            0,
        )
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
        HelpFlags {
            filter_complex_script: help.contains("-filter_complex_script"),
            fps_mode: help.contains("-fps_mode"),
            vsync: help.contains("-vsync"),
        }
    })
}

/// The argv spelling that points *this* ffmpeg at a filtergraph file, probed
/// once per process: FFmpeg 8 removed `-filter_complex_script` (deprecated in
/// 7.0 as equivalent to the generic `-/filter_complex <file>` form), so on a
/// bundled FFmpeg 8 the old spelling aborts every spilled render with
/// `Unrecognized option` — while a pre-7.0 binary knows only the old one.
/// `-h full` still lists the option wherever it exists.
fn graph_script_flag() -> &'static str {
    static FLAG: OnceLock<&'static str> = OnceLock::new();
    FLAG.get_or_init(|| {
        let legacy = help_flags().filter_complex_script;
        tracing::debug!(legacy, "probed ffmpeg for -filter_complex_script");
        if legacy {
            "-filter_complex_script"
        } else {
            "-/filter_complex"
        }
    })
}

/// The flag that sets how this ffmpeg paces output frames, given which of the two
/// spellings its `-h full` lists (pure, unit-tested): `-fps_mode` since 5.1, which
/// FFmpeg 9 is left with — it removed `-vsync`, `Unrecognized option` — and
/// `-vsync` before that.
pub(super) fn fps_mode_flag_for(knows_fps_mode: bool, knows_vsync: bool) -> &'static str {
    if knows_fps_mode || !knows_vsync {
        "-fps_mode"
    } else {
        "-vsync"
    }
}

/// [`fps_mode_flag_for`] for this machine's ffmpeg, probed once per process: the spelling
/// every spawn outside the engine that passes `passthrough` uses.
pub fn fps_mode_flag() -> &'static str {
    static FLAG: OnceLock<&'static str> = OnceLock::new();
    FLAG.get_or_init(|| {
        let help = help_flags();
        fps_mode_flag_for(help.fps_mode, help.vsync)
    })
}

// ---- the threads of a wide graph --------------------------------------------------------------------

/// The most threads the swscale contexts of one `-filter_complex` graph may hold between them, counted as [`graph_contexts`]
/// counts, which is about twice the contexts a graph holds. A `scale` filter hands its graph's thread count
/// (`-filter_complex_threads`) to its swscale context, which starts a pool of that many threads of its own, and so does every
/// `scale` FFmpeg inserts where two filters' pixel formats differ; a slice-threaded filter that is no scaler shares the graph's one
/// pool (`hflip` 40 times: 2 threads more, `scale` 40 times: 160). A graph holds *contexts x threads*: 100 butted blended clips
/// 3,600 threads on a 4-core machine on 9.0.2 and 6.1 alike (about 10 contexts a clip), 300 plain clips of one file the same (3 a
/// clip). A process may not hold that many on macOS (`kern.num_taskthreads`, 2,560 as XNU ships it; not verified on a Mac): the
/// swscaler fails to initialise (`Resource temporarily unavailable`) and the encoder after it (`ff_frame_thread_encoder_init
/// failed`), reproduced with a cap of 2,560 threads a process. At 1,536 the pools hold about 800, and the rest of the process, its
/// decoders, its encoder and its muxer, has the room.
const GRAPH_POOL_THREADS: usize = 1536;

/// The filters of a graph that name or force a pixel format, where FFmpeg may put a scaler of its own: with the `scale` filters
/// themselves, an upper bound of the swscale contexts the graph holds (the 100 blended clips above: 1,700 against the 1,000 it
/// holds, 1.7 times; the 300 plain clips, 1.65). Nothing else is counted.
const FORMAT_FILTERS: [&str; 11] = [
    "scale",
    "format",
    "overlay",
    "blend",
    "lutyuv",
    "colorchannelmixer",
    "alphamerge",
    "alphaextract",
    "geq",
    "fade",
    "pad",
];

/// The filters of a graph, by name (pure): each piece between a `;` and a `,`, less the pads a link opens with. Quotes are not looked
/// into, so a piece of an expression is a "name" that no filter has, and a name inside one can be counted, which only overstates.
fn graph_filter_names(graph: &str) -> impl Iterator<Item = &str> {
    graph.split([';', ',']).map(|piece| {
        let mut rest = piece.trim_start();
        while let Some(after) = rest.strip_prefix('[') {
            rest = after.split_once(']').map_or("", |(_, tail)| tail).trim_start();
        }
        rest.split(['=', '[', ' ']).next().unwrap_or("")
    })
}

/// An upper bound of the swscale contexts `graph` holds (pure): its [`FORMAT_FILTERS`].
pub(super) fn graph_contexts(graph: &str) -> usize {
    graph_filter_names(graph).filter(|name| FORMAT_FILTERS.contains(name)).count()
}

/// The filters whose result depends on the number of slice threads the graph runs on: `chromakey` starts its window over at each slice,
/// and `gblur`'s kernel is not the recursion on every plane shape at every count (`gblur::KernelHazard`). The GPU compositor draws the
/// one at `cpu::exact_graph_threads` and the plan judges the other over `cpu::graph_thread_range`, both the budget's count, so a graph
/// that runs either is handed exactly that count ([`cap_wide_graph`] leaves it).
const SLICE_DEPENDENT_FILTERS: [&str; 2] = ["chromakey", "gblur"];

/// Whether `graph` runs a filter whose result depends on its slice threads ([`SLICE_DEPENDENT_FILTERS`]).
pub(super) fn graph_depends_on_slices(graph: &str) -> bool {
    graph_filter_names(graph).any(|name| SLICE_DEPENDENT_FILTERS.contains(&name))
}

/// The threads a graph of `contexts` swscale contexts may give each of them where the machine would give it `threads` (pure):
/// `threads` while the contexts' pools fit [`GRAPH_POOL_THREADS`], else what fits (down to 1, which starts no pool).
pub(super) fn pool_threads_for(contexts: usize, threads: usize) -> usize {
    let threads = threads.max(1);
    (GRAPH_POOL_THREADS / contexts.max(1)).clamp(1, threads)
}

/// What the cap on a wide graph ([`cap_wide_graph`]) reads of the graph it is spawned with: its swscale contexts
/// ([`graph_contexts`]) and whether it runs a filter whose result depends on its slice threads ([`graph_depends_on_slices`]).
/// Bundled so that the spawn and the plan's judgement of the threads a frame is drawn on ([`wide_graph_cap`]) read the same two numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphShape {
    pub contexts: usize,
    pub slice_dependent: bool,
}

/// The shape of `graph` (pure).
pub fn graph_shape(graph: &str) -> GraphShape {
    GraphShape {
        contexts: graph_contexts(graph),
        slice_dependent: graph_depends_on_slices(graph),
    }
}

/// The filter threads a graph of this `shape` is lowered to where the machine would give it `granted` (pure): `Some(n)` when it is
/// wide enough to hold more than [`GRAPH_POOL_THREADS`] between its contexts, and does not run a slice-dependent filter. The one
/// arithmetic of [`cap_wide_graph`] (the spawn) and of `RenderPlan::ten_bit_thread_range` (the plan's judgement), which is why a
/// 4:2:0 10-bit frame is drawn on the threads the export runs on and not the budget's.
pub fn wide_graph_cap(shape: GraphShape, granted: usize) -> Option<usize> {
    let granted = granted.max(1);
    let each = pool_threads_for(shape.contexts, granted);
    (each < granted && !shape.slice_dependent).then_some(each)
}

/// One run of ffmpeg that draws a stretch of the export's picture, as the plan needs to know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRun {
    /// The output frames the run draws (the whole export's: `0..u64::MAX` for a cut that runs whole).
    pub frames: std::ops::Range<u64>,
    /// Its `-filter_complex` graph.
    pub shape: GraphShape,
}

/// The parts an export of `timeline` is rendered in at `limits` (`None`: parts are off), else `None`: the one run. The **one** decision of
/// whole or parts, which [`render_attempt`] and the plan's account of the export ([`export_graph_runs`]) both ask, so the plan cannot
/// drift from the export.
pub(super) fn export_parts_within(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    limits: Option<segments::SegmentLimits>,
) -> Option<segments::PartsPlan> {
    limits.and_then(|limits| segments::plan_export(timeline, assets, opts, limits))
}

/// [`export_parts_within`] at the process's limits (`KERF_EXPORT_PARTS`, which is constant for a process and read here as the export reads it).
pub(super) fn export_parts(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Option<segments::PartsPlan> {
    export_parts_within(timeline, assets, opts, segments::limits())
}

/// The runs the export of `timeline` spawns its picture graph in, built by the export's own builders (pure): the parts [`export_parts`]
/// cuts it into, each with the graph [`segments::build_part_args`] writes, else the one run of [`build_export_args`]. What a part holds depends
/// on which clips share it: a long clip under many short ones is one wide run, the same clips in a cut that can be cut are several narrow ones.
/// `None` where the graphs are not known without more than a build: an asset whose tick is not recorded and whose blended clips could fold if
/// it were ([`time_base_inputs`]: the export probes it first), or a build that fails.
///
/// The threads of a run are the budget's, lowered to [`wide_graph_cap`] of its shape at spawn. These are the runs of an export that renders
/// this delivery alone: a multi-format export written in one run (`together`) has every variant's graph in one, and is not these.
pub fn export_graph_runs(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Option<Vec<GraphRun>> {
    export_graph_runs_within(timeline, assets, opts, segments::limits())
}

/// [`export_graph_runs`] for the part `limits` (`None`: parts are off), pure, for the tests.
pub(super) fn export_graph_runs_within(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    limits: Option<segments::SegmentLimits>,
) -> Option<Vec<GraphRun>> {
    if !time_base_inputs(timeline, assets, opts).is_empty() {
        return None;
    }
    match export_parts_within(timeline, assets, opts, limits) {
        Some(parts) => {
            let floating = svg_layer::svg_sequences(&parts.cut, &parts.fmt);
            parts
                .parts
                .iter()
                .map(|part| {
                    let args = segments::build_part_args(&parts, assets, opts, part, &floating, "part.mkv", "-fps_mode").ok()?;
                    Some(GraphRun {
                        frames: part.first..part.end(),
                        shape: graph_shape(&filter_graph_text(&args)?),
                    })
                })
                .collect()
        }
        None => {
            let args = build_export_args(timeline, assets, "export.mkv", opts).ok()?;
            Some(vec![GraphRun {
                frames: 0..u64::MAX,
                shape: graph_shape(&filter_graph_text(&args)?),
            }])
        }
    }
}

/// The text of the `-filter_complex` graph an argv runs (inline, or the script it points to), `None` when it has none.
fn filter_graph_text(args: &[String]) -> Option<String> {
    let at = args
        .iter()
        .position(|a| matches!(a.as_str(), "-filter_complex" | "-filter_complex_script" | "-/filter_complex"))?;
    let value = args.get(at + 1)?;
    if args[at] == "-filter_complex" {
        Some(value.clone())
    } else {
        std::fs::read_to_string(value).ok()
    }
}

/// Cap the filter threads of a graph that is wide enough to hold more than [`GRAPH_POOL_THREADS`] between its swscale contexts
/// (see there) to what fits: `-filter_complex_threads` with the smaller of what the argv has and the cap, at the head of the argv.
/// Nothing for a graph that fits, so every ordinary export is handed what it always was, and nothing for one that runs a filter
/// whose result depends on the slice threads ([`graph_depends_on_slices`]): the plan and the compositor draw it at the budget's count,
/// and a lowered one would put the export's frame off theirs (such a wide graph is exposed to a process's thread limit as it was).
pub(super) fn cap_wide_graph(args: &mut Vec<String>, budget: cpu::Budget) {
    let Some(graph) = filter_graph_text(args) else {
        return;
    };
    let granted = budget.threads().max(1);
    let Some(each) = wide_graph_cap(graph_shape(&graph), granted) else {
        return;
    };
    let first_input = args.iter().position(|a| a == "-i").unwrap_or(args.len());
    match args[..first_input].iter().position(|a| a == "-filter_complex_threads") {
        Some(at) if at + 1 < first_input => {
            let had = args[at + 1].parse::<usize>().unwrap_or(granted);
            args[at + 1] = had.min(each).to_string();
        }
        _ => {
            args.splice(0..0, ["-filter_complex_threads".to_string(), each.to_string()]);
        }
    }
}

/// The thread caps an ffmpeg run is spawned with at `budget` (`sinks`: the indices of its outputs, when it has several): the
/// budget's ([`cpu::limit_args`]), then a wide graph's ([`cap_wide_graph`]). The one place both are written, which a test that
/// renders a graph through ffmpeg uses too, so that it is handed what an export is.
pub(super) fn spawn_caps(args: &mut Vec<String>, budget: cpu::Budget, sinks: &[usize]) {
    if sinks.len() > 1 {
        cpu::limit_outputs_args(args, budget, sinks);
    } else {
        cpu::limit_args(args, budget);
    }
    cap_wide_graph(args, budget);
}

/// Move an oversized filtergraph out of argv into a script file, pointing ffmpeg
/// at it with [`graph_script_flag`]'s spelling. Leaves ordinary exports
/// untouched, so their argv stays byte-identical (and every pure arg-builder
/// test with it).
pub(super) fn externalize_filter_complex(args: &mut [String], tag: &str) -> Result<GraphScript> {
    spill_graph(args, tag, graph_script_flag())
}

/// The pure half of [`externalize_filter_complex`], with the flag decided.
///
/// Either flag takes the path as its own argv token, which is why this beats
/// the obvious alternative of `sendcmd=f=`: that would bury the path *inside* a
/// filtergraph value, where `\` escapes and `:` separates options, so a Windows
/// path would have to be mangled first.
pub(super) fn spill_graph(args: &mut [String], tag: &str, flag: &str) -> Result<GraphScript> {
    let Some(i) = oversized_graph_index(args) else {
        return Ok(GraphScript(None));
    };
    // Unique per call: two renders in one process (the GUI previewing while an
    // agent exports, two playback streams) share a pid and a tag, and the
    // second to finish used to delete the script the other was still reading.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("kerf-graph-{}-{tag}-{seq}.txt", std::process::id()));
    std::fs::write(&path, &args[i]).map_err(|e| Error::Engine(format!("could not write the filtergraph script: {e}")))?;
    args[i] = path.to_string_lossy().into_owned();
    args[i - 1] = flag.to_string();
    Ok(GraphScript(Some(path)))
}

/// A filtergraph given to `ffmpeg` in a script file, because it was too long for the command line; the
/// file is removed when this is dropped.
pub struct GraphFile(#[allow(dead_code)] GraphScript);

/// Move the `-filter_complex` graph of `args` into a script file if it is too long to pass in argv (the
/// engine's own rule, [`GRAPH_ARG_MAX`]), pointing `ffmpeg` at it with the spelling this build takes.
/// Nothing moves otherwise. For a long graph of a caller outside the engine (the compositor measures a text
/// with an instance of `drawtext` per prefix); a graph that holds a line feed in a quoted value is better left
/// in argv.
pub fn spill_long_graph(args: &mut [String], tag: &str) -> Result<GraphFile> {
    externalize_filter_complex(args, tag).map(GraphFile)
}
