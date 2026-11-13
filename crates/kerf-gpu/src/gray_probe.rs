//! Whether this FFmpeg takes a gray picture into a composite that is not BT.601 as it takes it into an untagged one — measured once per
//! process on the export's own chains and the still's, never assumed and never keyed on a version string. `GpuCaps::gray_untagged` is set only
//! on a [`Gray::Match`].
//!
//! A gray picture has no colourspace of its own in FFmpeg 9's graph: the `scale` that converts it to the overlay's `yuva420p` is handed the
//! **composite's** matrix as its destination, whatever the file states, and swscale's gray conversion is not the same arithmetic for every
//! destination (Y one level off on about 40 % of the samples in BT.709, BT.2020 and SMPTE 240M, U and V off for the first two). The compositor
//! draws the conversion the decode makes, which has no composite and is the untagged one, so a gray layer in such a composite is FFmpeg's
//! unless this finds that the conversion does not depend on the matrix. Where FFmpeg does not negotiate the composite's matrix (FFmpeg 6: the
//! composite is untagged whatever the layers are) nothing here is asked.
//!
//! The probe runs a gray picture and a 4:2:0 one **side by side on one canvas**, as the two clips of an export, in each order (the gray one under
//! and over), through the three chains a clip has (the still's `scale,pad,setsar` with no `format`, a plain export chain ending in `format=yuv420p`
//! and a clip on the alpha plane, `format=yuva420p`), with the 4:2:0 picture tagged BT.709, BT.2020 and SMPTE 240M, and holds each frame to the
//! same graph over an untagged picture (the composite the decode has), every sample of every plane with no tolerance. A run that fails or runs
//! out of the deadline is [`Gray::Unknown`]: asked again after a backoff, and no gray picture is drawn in such a composite until it answers.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::measure::{self, Budget, Probe, Timing, Verdict};

/// What the probe found: [`Verdict::Match`] (the gray picture's conversion does not depend on the composite's matrix, every case),
/// [`Verdict::Differs`] (it does: the detail names the case) or [`Verdict::Unknown`].
pub type Gray = Verdict;

/// One FFmpeg run is bounded at 20 s, the whole probe (about forty short runs) at 40 s, and a probe that could not tell is not asked again
/// before 30 s.
const TIMING: Timing = Timing {
    backoff: Duration::from_secs(30),
    overall: Duration::from_secs(40),
    per_run: Duration::from_secs(20),
};

static STATE: Probe = Probe::new();

/// What is known without running anything: the measured answer, else [`Verdict::Unknown`]. What the compositor's caps are made from.
pub fn measured() -> Gray {
    measure::known(&STATE)
}

/// The answer, measured on the first call: about seven short `ffmpeg` children that make the files and thirty-six that run the graphs, one after
/// the other, with no lock held (a blocking thread's, ahead of the frame that needs it: [`crate::prepare`]). A caller that arrives while it runs
/// waits for it, boundedly, and takes its answer. `Unknown` is not remembered but for a backoff.
pub fn gray() -> Gray {
    measure::cached(&STATE, Instant::now(), TIMING, probe)
}

/// A picture is `W` x `H`: wide enough that the polynomial takes every level of the gray plane, tall enough for 4:2:0's rows.
const PICTURE: (usize, usize) = (256, 16);

/// The matrices the composite can take that are not BT.601, as the tag the 4:2:0 picture carries.
const MATRICES: [(&str, &str); 3] = [
    ("BT.709", "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709"),
    (
        "BT.2020",
        "setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt2020-10",
    ),
    ("SMPTE 240M", "setparams=colorspace=smpte240m:range=tv"),
];

/// The luma every picture is made of: a polynomial of the coordinates mod 256 (not `random()`, which restarts for every slice thread).
const LUMA: &str = "mod(X*X*37+Y*Y*91+X*Y*53+X*11+Y*29,256)";

/// The source of the 4:2:0 picture and of the gray one, tagged `tag` (`None`: untagged).
fn source(gray: bool, tag: Option<&str>) -> String {
    let (w, h) = PICTURE;
    let tag = tag.map(|t| format!(",{t}")).unwrap_or_default();
    if gray {
        format!("nullsrc=s={w}x{h}:r=1:d=1,format=gray,geq=lum='{LUMA}'{tag}")
    } else {
        format!(
            "nullsrc=s={w}x{h}:r=1:d=1,format=yuv420p,geq=lum='{LUMA}':cb='mod(X*X*13+Y*Y*71+X*Y*29+X*7+Y*3,256)':cr='mod(X*X*53+Y*Y*19+X*Y*97+X*5+Y*17,256)'{tag}"
        )
    }
}

/// The three chains of a clip: what the engine writes for a plain clip of an FFmpeg still (the overlay asks for `yuva420p` itself), of an
/// export, and of a clip on the alpha plane (a dissolving one).
const CHAINS: [(&str, &str); 3] = [
    ("a still's chain", ""),
    ("an export's plain chain", ",fps=1,format=yuv420p"),
    ("a clip on the alpha plane", ",fps=1,format=yuva420p"),
];

/// The graph of two clips side by side on a canvas twice a picture wide: input 0 under, input 1 over.
fn graph(tail: &str) -> String {
    let (w, h) = PICTURE;
    let fit = format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1{tail}");
    format!(
        "color=c=black:s={}x{h}:r=1:d=1,format=yuv420p[vbase];[0:v]{fit}[v0];[vbase][v0]overlay=x=0:y=0[o0];[1:v]{fit}[v1];[o0][v1]overlay=x={w}:y=0[o]",
        2 * w
    )
}

/// A scratch directory under the temp dir, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Option<Self> {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kerf-gpu-gray-probe-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).ok()?;
        Some(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One picture as a lossless one-frame file (FFV1 in Matroska) carrying its tag: what an export's inputs are (a tag set in the graph by
/// `setparams` is another constraint in the negotiation and moves who decides).
fn make_file(budget: &Budget, path: &Path, gray: bool, tag: Option<&str>) -> Option<()> {
    let src = source(gray, tag);
    let pix_fmt = if gray { "gray" } else { "yuv420p" };
    let path = path.to_string_lossy();
    budget
        .ffmpeg(
            &[
                "-f",
                "lavfi",
                "-i",
                &src,
                "-frames:v",
                "1",
                "-c:v",
                "ffv1",
                "-pix_fmt",
                pix_fmt,
                &path,
            ],
            Vec::new(),
        )
        .map(drop)
}

/// The frame two files make through `graph`, as `yuv420p`.
fn frame(budget: &Budget, files: &[PathBuf; 2], graph: &str) -> Option<Vec<u8>> {
    let (w, h) = PICTURE;
    let [a, b] = [&files[0], &files[1]].map(|f| f.to_string_lossy().into_owned());
    let out = budget.ffmpeg(
        &[
            "-i",
            &a,
            "-i",
            &b,
            "-filter_complex",
            graph,
            "-map",
            "[o]",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "pipe:1",
        ],
        Vec::new(),
    )?;
    (out.len() == 2 * w * h * 3 / 2).then_some(out)
}

fn probe(budget: &Budget) -> Gray {
    let Some(scratch) = Scratch::new() else {
        return Gray::Unknown;
    };
    let file = |name: &str| scratch.0.join(name);
    // The untagged 4:2:0 picture, the three tagged ones, and the gray picture tagged as each of them (the file's own tag is the decode's
    // and the stack's untagged reference is the same file: only the composite differs between the two frames of a case).
    let untagged = file("untagged.mkv");
    if make_file(budget, &untagged, false, None).is_none() {
        return Gray::Unknown;
    }
    for (n, (_, tag)) in MATRICES.iter().enumerate() {
        if make_file(budget, &file(&format!("tagged{n}.mkv")), false, Some(tag)).is_none()
            || make_file(budget, &file(&format!("gray{n}.mkv")), true, Some(tag)).is_none()
        {
            return Gray::Unknown;
        }
    }
    for (n, (matrix, _)) in MATRICES.iter().enumerate() {
        let (tagged, gray) = (file(&format!("tagged{n}.mkv")), file(&format!("gray{n}.mkv")));
        for (chain, tail) in CHAINS {
            let g = graph(tail);
            for gray_over in [true, false] {
                let order = |picture: &'_ PathBuf| -> [PathBuf; 2] {
                    if gray_over {
                        [picture.clone(), gray.clone()]
                    } else {
                        [gray.clone(), picture.clone()]
                    }
                };
                let (Some(in_matrix), Some(in_untagged)) =
                    (frame(budget, &order(&tagged), &g), frame(budget, &order(&untagged), &g))
                else {
                    return Gray::Unknown;
                };
                let apart = in_matrix.iter().zip(&in_untagged).filter(|(a, b)| a != b).count();
                if apart != 0 {
                    return Gray::Differs(format!(
                        "{chain} with the gray picture {} a {matrix} one: {apart} of {} samples are not the untagged composite's",
                        if gray_over { "over" } else { "under" },
                        in_matrix.len()
                    ));
                }
            }
        }
    }
    Gray::Match
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_graph_is_two_clips_side_by_side_through_the_chain() {
        let g = graph(",fps=1,format=yuva420p");
        assert!(g.contains("[0:v]scale=256:16") && g.contains("[1:v]scale=256:16"), "{g}");
        assert_eq!(g.matches("fps=1,format=yuva420p").count(), 2, "{g}");
        assert!(g.contains("overlay=x=256:y=0[o]"), "{g}");
        assert!(
            !graph("").contains("fps=1"),
            "a still's chain has no `fps` or `format` ahead of the overlay"
        );
    }

    #[test]
    fn the_pictures_are_gray_or_yuv420p_and_tagged_as_asked() {
        assert!(source(true, None).contains("format=gray") && !source(true, None).contains("setparams"));
        assert!(source(false, Some("setparams=colorspace=bt709")).ends_with(",setparams=colorspace=bt709"));
        assert!(source(false, None).contains("format=yuv420p"));
    }

    /// The probe itself, on the FFmpeg in use (`#[ignore]`d: it needs one). It must be able to tell, and says the same twice.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn this_ffmpeg_takes_a_gray_picture_into_a_tagged_composite_as_into_an_untagged_one_or_the_probe_says_where_not() {
        let first = probe(&Budget::new(&TIMING));
        assert_ne!(first, Gray::Unknown, "the probe could not run");
        assert_eq!(first, probe(&Budget::new(&TIMING)));
        eprintln!("a gray picture in a tagged composite: {first:?}");
    }
}
