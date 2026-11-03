//! Lane B round five, R4: the frontend's port of `LayerGeometry` against the engine on cases the checked-in corpus does not hold
//! (`checker_b`'s own seed and shape): pictures at the frame's width with an odd last row, off the origin and at it, in every chroma
//! grid, with crops, positions, a scale, a fit and the keyed placements. The engine writes its answers, `bun` replays them through
//! `frontend/src/lib/layer-geometry.ts`; a difference names the case.
//!
//! ```text
//! cargo test -p kerf-core --no-default-features --test checker_b_r5_geometry -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

use std::path::Path;
use std::process::Command;

use kerf_core::layer_geometry::{Keyed, LayerGeometry, Placement};
use kerf_core::model::{Fit, Subsampling, Transform};
use serde_json::{json, Value};

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

    fn frac(&mut self, r: f64) -> f64 {
        let steps = (r * 10_000.0) as u64;
        (self.below(2 * steps + 1) as f64 - steps as f64) / 10_000.0
    }
}

fn rect(r: kerf_core::layer_geometry::Rect) -> Value {
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

fn case(rng: &mut Rng) -> Value {
    // The frame, and a picture that is the frame's width (the unresampled shape) or near it, with an odd or even last row.
    let canvas = rng.pick(&[
        (64, 36),
        (96, 54),
        (640, 360),
        (1280, 720),
        (320, 200),
        (36, 64),
        (100, 60),
        (1920, 1080),
    ]);
    let (cw, ch) = canvas;
    let src = match rng.below(6) {
        0..=2 => (cw, 1 + rng.below(u64::from(ch)) as u32),
        3 => (cw, ch - 1 - rng.below(8) as u32 % ch.max(2)),
        4 => (cw - rng.below(5) as u32 % cw, 1 + rng.below(u64::from(ch)) as u32),
        _ => (
            1 + rng.below(u64::from(cw) * 2) as u32,
            1 + rng.below(u64::from(ch) * 2) as u32,
        ),
    };
    let src = (src.0.max(1), src.1.max(1));
    let mut tf = Transform::default();
    if rng.below(3) == 0 {
        tf.scale = rng.pick(&[1.0, 1.0, 0.5, 0.75, 1.3, 0.999]);
        tf.pos_x = if rng.below(2) == 0 { 0.0 } else { rng.frac(0.3) };
        tf.pos_y = if rng.below(2) == 0 { 0.0 } else { rng.frac(0.3) };
        tf.rotation = rng.pick(&[0.0, 0.0, 0.0, 15.0, 90.0]);
        tf.opacity = rng.pick(&[1.0, 1.0, 0.5]);
        for edge in [&mut tf.crop_left, &mut tf.crop_right, &mut tf.crop_top, &mut tf.crop_bottom] {
            if rng.below(3) == 0 {
                *edge = rng.below(2000) as f64 / 10_000.0;
            }
        }
    }
    let sub = if rng.below(10) == 0 {
        None
    } else {
        Some(rng.pick(&Subsampling::ALL))
    };
    let placement = if rng.below(4) == 0 {
        Placement {
            keyframed: Some(Keyed {
                scale: rng.below(2) == 0,
                rotation: rng.below(2) == 0,
                rotates: rng.below(2) == 0,
                opacity: rng.below(2) == 0,
            }),
            offset: if rng.below(2) == 0 {
                (0.0, 0.0)
            } else {
                (rng.frac(1.0), rng.frac(1.0))
            },
            placed: None,
        }
    } else {
        Placement::STILL
    };
    let fit = rng.pick(&[Fit::Contain, Fit::Contain, Fit::Cover, Fit::Blur]);
    let got = match sub {
        Some(sub) => LayerGeometry::resolve_with(src, canvas, fit, &tf, sub, placement),
        None => LayerGeometry::resolve_any_grid_with(src, canvas, fit, &tf, placement),
    };
    let expect = match got {
        Ok(g) => json!({ "ok": geometry(&g) }),
        Err(e) => json!({ "err": e.0 }),
    };
    json!({
        "src": [src.0, src.1],
        "canvas": [canvas.0, canvas.1],
        "fit": fit,
        "tf": serde_json::to_value(tf).expect("serializes"),
        "sub": sub.map(|s| json!([s.log2_w, s.log2_h])),
        "keyed": placement.keyframed.map(|k| json!({ "scale": k.scale, "rotation": k.rotation, "rotates": k.rotates, "opacity": k.opacity })),
        "offset": [placement.offset.0, placement.offset.1],
        "expect": expect,
    })
}

#[test]
#[ignore = "needs bun"]
fn checker_b_r5_the_frontend_port_agrees_with_the_engine_on_odd_rows_in_every_grid() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let port = root.join("frontend/src/lib/layer-geometry.ts");
    assert!(port.exists(), "{}", port.display());
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-b-r5-geometry");
    std::fs::create_dir_all(&dir).unwrap();
    let mut rng = Rng(0x6234_7235_c0ff_ee01);
    let cases: Vec<Value> = (0..20_000).map(|_| case(&mut rng)).collect();
    let oks = cases.iter().filter(|c| c["expect"].get("ok").is_some()).count();
    let odd_rows_kept_off_origin = cases
        .iter()
        .filter(|c| {
            c["expect"]["ok"]["matte"] == true
                && c["expect"]["ok"]["picture"][1].as_u64().is_some_and(|h| h % 2 == 1)
                && c["expect"]["ok"]["picture_shows"] == c["expect"]["ok"]["picture"]
        })
        .count();
    let odd_rows_dropped = cases
        .iter()
        .filter(|c| {
            c["expect"]["ok"]["matte"] == true
                && c["expect"]["ok"]["picture"][1].as_u64().is_some_and(|h| h % 2 == 1)
                && c["expect"]["ok"]["picture_shows"][1].as_u64() == c["expect"]["ok"]["picture"][1].as_u64().map(|h| h - 1)
        })
        .count();
    eprintln!(
        "{} cases, {oks} answered, {odd_rows_kept_off_origin} keep an odd row, {odd_rows_dropped} drop it",
        cases.len()
    );
    assert!(
        odd_rows_kept_off_origin > 100 && odd_rows_dropped > 100,
        "the cases do not reach the odd row"
    );
    std::fs::write(
        dir.join("cases.json"),
        serde_json::to_string(&json!({ "cases": cases })).unwrap(),
    )
    .unwrap();
    let script = format!(
        r#"import {{ isDeepStrictEqual }} from 'node:util';
import {{ resolveLayer, resolveLayerAnyGrid }} from '{}';
const cases = JSON.parse(await Bun.file('{}').text()).cases;
let bad = 0;
for (const [i, c] of cases.entries()) {{
  const placement = {{ keyframed: c.keyed, offset: c.offset }};
  const got = c.sub ? resolveLayer(c.src, c.canvas, c.fit, c.tf, c.sub, placement) : resolveLayerAnyGrid(c.src, c.canvas, c.fit, c.tf, placement);
  if (!isDeepStrictEqual(JSON.parse(JSON.stringify(got)), c.expect)) {{
    bad += 1;
    if (bad <= 10) console.log('DIFF', i, JSON.stringify({{ src: c.src, canvas: c.canvas, fit: c.fit, sub: c.sub, keyed: c.keyed, tf: c.tf }}), 'engine', JSON.stringify(c.expect), 'port', JSON.stringify(got));
  }}
}}
console.log('CHECKED', cases.length, 'DIFFERING', bad);
process.exit(bad === 0 ? 0 : 1);
"#,
        port.canonicalize().unwrap().display(),
        dir.join("cases.json").display()
    );
    let runner = dir.join("replay.ts");
    std::fs::write(&runner, script).unwrap();
    let out = Command::new("bun").arg("run").arg(&runner).output().expect("run bun");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    eprintln!("{text}");
    assert!(out.status.success(), "the port differs from the engine:\n{text}");
}
