//! The differential corpus for the frontend's port of [`LayerGeometry`]
//! (`frontend/src/lib/layer-geometry.ts`), which the Preview's transform handles draw from.
//! Every case is a picture, a canvas, a fit, a transform, a chroma grid and a placement, and
//! what this module answered for it — the geometry, or the exact refusal. The port replays the
//! file (`layer-geometry.test.ts`), so a rounding rule changed here without the port following
//! fails there, naming the case.
//!
//! Regenerate after an intended change:
//! `KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- layer_geometry_corpus`

use serde_json::{json, Value};

use super::*;
use crate::model::pix_fmt_subsampling;

const CORPUS_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../frontend/src/lib/fixtures/layer-geometry-corpus.json"
);

const RANDOM_CASES: usize = 400;

/// xorshift64*: the corpus must not move between runs.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len() as u64) as usize]
    }

    /// A value in `-r..=r` with four decimals.
    fn frac(&mut self, r: f64) -> f64 {
        let steps = (r * 10_000.0) as u64;
        (self.below(2 * steps + 1) as f64 - steps as f64) / 10_000.0
    }
}

struct Case {
    src: (u32, u32),
    canvas: (u32, u32),
    fit: Fit,
    tf: Transform,
    sub: Option<Subsampling>,
    placement: Placement,
}

fn rect(r: Rect) -> Value {
    json!({ "x": r.x, "y": r.y, "w": r.w, "h": r.h })
}

fn geometry(g: &LayerGeometry) -> Value {
    json!({
        "stages": g.stages.iter().map(|s| json!({ "src": rect(s.src), "scaled": [s.scaled.0, s.scaled.1], "keep": rect(s.keep) })).collect::<Vec<_>>(),
        "picture": [g.picture.0, g.picture.1],
        "rotation": g.rotation.map(|r| json!({ "angle": r.angle, "out": [r.out.0, r.out.1] })),
        "layer": [g.layer.0, g.layer.1],
        "picture_at": [g.picture_at.0, g.picture_at.1],
        "picture_shows": [g.picture_shows.0, g.picture_shows.1],
        "matte": g.matte,
        "origin": [g.origin.0, g.origin.1],
        "opacity": f64::from(g.opacity),
        "translucent": g.translucent,
    })
}

fn sub_json(s: Subsampling) -> Value {
    json!([s.log2_w, s.log2_h])
}

fn hand_made() -> Vec<Case> {
    let t = Transform::default;
    let still = Placement::STILL;
    let c = |src, canvas, fit, tf, sub| Case {
        src,
        canvas,
        fit,
        tf,
        sub: Some(sub),
        placement: still,
    };
    let s420 = Subsampling::YUV420;
    vec![
        c((640, 360), (640, 360), Fit::Contain, t(), s420),
        c((640, 360), (360, 640), Fit::Contain, t(), s420),
        c((640, 360), (360, 640), Fit::Cover, t(), s420),
        c((1000, 1650), (1080, 1080), Fit::Contain, t(), s420),
        c(
            (640, 360),
            (640, 360),
            Fit::Contain,
            Transform {
                scale: 0.5,
                pos_x: 0.1,
                ..t()
            },
            s420,
        ),
        c(
            (640, 360),
            (640, 360),
            Fit::Contain,
            Transform { rotation: 30.0, ..t() },
            s420,
        ),
        c(
            (640, 360),
            (640, 360),
            Fit::Contain,
            Transform {
                crop_left: 11.0 / 640.0,
                crop_top: 7.0 / 360.0,
                crop_right: 6.0 / 640.0,
                crop_bottom: 6.0 / 360.0,
                ..t()
            },
            Subsampling::NONE,
        ),
        c(
            (640, 360),
            (640, 360),
            Fit::Contain,
            Transform {
                crop_left: 0.6,
                crop_right: 0.6,
                ..t()
            },
            s420,
        ),
        c(
            (641, 361),
            (640, 360),
            Fit::Contain,
            Transform {
                opacity: 0.5,
                scale: 0.999,
                ..t()
            },
            s420,
        ),
        c((640, 360), (640, 360), Fit::Contain, Transform { scale: 0.004, ..t() }, s420),
        c(
            (640, 360),
            (640, 360),
            Fit::Contain,
            Transform {
                pos_x: 1e12,
                pos_y: -0.3333,
                ..t()
            },
            s420,
        ),
        c((0, 360), (640, 360), Fit::Contain, t(), s420),
        c((640, 360), (640, 360), Fit::Contain, Transform { scale: -1.0, ..t() }, s420),
        Case {
            src: (640, 360),
            canvas: (640, 360),
            fit: Fit::Contain,
            tf: Transform {
                crop_left: 0.0123,
                ..t()
            },
            sub: None,
            placement: still,
        },
        Case {
            src: (640, 360),
            canvas: (640, 360),
            fit: Fit::Contain,
            tf: Transform { rotation: 10.0, ..t() },
            sub: Some(s420),
            placement: Placement {
                keyframed: Some(Keyed::all(true)),
                offset: (0.25, -0.1),
            },
        },
    ]
}

fn random(rng: &mut Rng) -> Case {
    let src = match rng.below(4) {
        0 => rng.pick(&[(1920, 1080), (1080, 1920), (3840, 2160), (1280, 720), (720, 1280), (640, 480)]),
        _ => (2 + rng.below(2400) as u32, 2 + rng.below(2400) as u32),
    };
    let canvas = match rng.below(5) {
        0 => (2 + rng.below(2000) as u32, 2 + rng.below(2000) as u32),
        _ => rng.pick(&[
            (1920, 1080),
            (1080, 1920),
            (1080, 1080),
            (1080, 1350),
            (1280, 720),
            (640, 360),
        ]),
    };
    let mut tf = Transform::default();
    if rng.below(5) > 0 {
        tf.scale = rng.pick(&[1.0, 0.5, 1.0 / 3.0, 0.75, 1.7, 2.5, 0.005, 0.0123]);
        tf.pos_x = if rng.below(3) == 0 { 0.0 } else { rng.frac(0.7) };
        tf.pos_y = if rng.below(3) == 0 { 0.0 } else { rng.frac(0.7) };
        tf.rotation = rng.pick(&[0.0, 0.0, 15.0, 90.0, -45.0, 180.0, 33.3, -270.0, 7.5]);
        tf.opacity = rng.pick(&[1.0, 1.0, 1.0, 0.5, 0.999, 0.25]);
        for edge in [&mut tf.crop_left, &mut tf.crop_right, &mut tf.crop_top, &mut tf.crop_bottom] {
            if rng.below(3) == 0 {
                *edge = rng.below(3500) as f64 / 10_000.0;
            }
        }
    }
    let sub = if rng.below(8) == 0 {
        None
    } else {
        Some(rng.pick(&Subsampling::ALL))
    };
    let placement = if rng.below(4) == 0 {
        let rotates = rng.below(2) == 0;
        Placement {
            keyframed: Some(Keyed {
                scale: rng.below(2) == 0,
                rotation: rng.below(2) == 0,
                rotates,
                opacity: rng.below(2) == 0,
            }),
            offset: if rng.below(2) == 0 {
                (0.0, 0.0)
            } else {
                (rng.frac(1.0), rng.frac(1.0))
            },
        }
    } else {
        Placement::STILL
    };
    Case {
        src,
        canvas,
        fit: if rng.below(3) == 0 { Fit::Cover } else { Fit::Contain },
        tf,
        sub,
        placement,
    }
}

fn answer(c: &Case) -> Value {
    let got = match c.sub {
        Some(sub) => LayerGeometry::resolve_with(c.src, c.canvas, c.fit, &c.tf, sub, c.placement),
        None => LayerGeometry::resolve_any_grid_with(c.src, c.canvas, c.fit, &c.tf, c.placement),
    };
    match got {
        Ok(g) => json!({ "ok": geometry(&g) }),
        Err(e) => json!({ "err": e.0 }),
    }
}

const PIX_FMTS: [&str; 22] = [
    "yuv420p",
    "yuvj420p",
    "yuv420p10le",
    "nv12",
    "p010le",
    "p210be",
    "p416le",
    "yuv422p",
    "yuv422p10",
    "nv16",
    "yuv440p",
    "yuv411p",
    "yuv410p",
    "yuv444p12le",
    "gray",
    "gray16le",
    "gbrp10le",
    "rgb24",
    "bgr0",
    "yuva420p",
    "YUV420P",
    "rgb565le",
];

fn corpus() -> Value {
    let mut rng = Rng(0x6b65_7266_6236);
    let mut cases = hand_made();
    cases.extend((0..RANDOM_CASES).map(|_| random(&mut rng)));
    json!({
        "about": "Written by kerf-core (layer_geometry/corpus.rs); replayed by layer-geometry.test.ts. Regenerate: KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- layer_geometry_corpus",
        "cases": cases.iter().map(|c| json!({
            "src": [c.src.0, c.src.1],
            "canvas": [c.canvas.0, c.canvas.1],
            "fit": c.fit,
            "tf": serde_json::to_value(c.tf).expect("serializes"),
            "sub": c.sub.map(sub_json),
            "keyed": c.placement.keyframed.map(|k| json!({ "scale": k.scale, "rotation": k.rotation, "rotates": k.rotates, "opacity": k.opacity })),
            "offset": [c.placement.offset.0, c.placement.offset.1],
            "expect": answer(c),
        })).collect::<Vec<_>>(),
        "pix_fmts": PIX_FMTS.iter().map(|n| json!({ "name": n, "sub": pix_fmt_subsampling(n).map(sub_json) })).collect::<Vec<_>>(),
    })
}

#[test]
fn layer_geometry_corpus_is_what_this_engine_does() {
    let fresh = format!("{}\n", serde_json::to_string(&corpus()).expect("serializes"));
    if std::env::var_os("KERF_BLESS_CORPUS").is_some() {
        std::fs::write(CORPUS_PATH, &fresh).expect("writes the corpus");
        return;
    }
    let on_disk = std::fs::read_to_string(CORPUS_PATH)
        .expect("frontend/src/lib/fixtures/layer-geometry-corpus.json — generate it with KERF_BLESS_CORPUS=1")
        .replace("\r\n", "\n");
    assert!(
        on_disk == fresh,
        "the layer geometry corpus is stale: the engine now places a layer where the checked-in answers do not say. \
         Regenerate with KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- layer_geometry_corpus, \
         then run `bun test` — the replay says what the port has to follow."
    );
}

#[test]
fn the_corpus_has_answers_of_every_kind() {
    let c = corpus();
    let cases = c["cases"].as_array().expect("cases");
    let count = |f: &dyn Fn(&Value) -> bool| cases.iter().filter(|x| f(x)).count();
    assert!(count(&|x| x["expect"].get("err").is_some()) >= 5);
    assert!(count(&|x| x["expect"]["ok"]["matte"] == true) >= 3);
    assert!(count(&|x| !x["expect"]["ok"]["rotation"].is_null() && x["expect"].get("ok").is_some()) >= 40);
    assert!(count(&|x| x["expect"]["ok"]["translucent"] == true) >= 10);
    assert!(count(&|x| !x["keyed"].is_null() && x["expect"].get("ok").is_some()) >= 40);
}
