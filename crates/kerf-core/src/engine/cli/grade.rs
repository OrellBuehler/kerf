//! A clip's [`Grade`] as filters: `lut3d` (the LUT), `lutrgb` (the curves), `colorbalance` (the
//! wheels), in that order, right after the `eq` of its [`Color`] — and nothing at all for a part
//! that is unused, so every graph without one is byte-identical.
//!
//! **Curves are `lutrgb` expressions, not a file.** Each channel's expression *is* the model's
//! cubic: the input as `val / maxval`, the channel's curve and then the master as a balanced tree
//! of comparisons over their points (`if(lt(..))`, `log2` levels however many points) down to one
//! segment's Horner polynomial, with the very constants [`CurveShape`] evaluates, and
//! `round(maxval * clip(y, 0, 1))` — so the integer FFmpeg stores for every input value is the
//! model's at any bit depth, and the builder stays pure (no file to write or to lose). `lutrgb`,
//! `st` / `ld`, `round` and `clip` are all in FFmpeg 4.4. (`curves` would draw its own natural
//! spline through the points, which overshoots; `lut1d` truncates where this rounds and needs a
//! file.)
//!
//! **The LUT is `lut3d` over Kerf's own copy of the file** ([`crate::color_lut`]), tetrahedral, its
//! intensity baked into the copy. A file that is gone or no longer a usable LUT is left out.
//!
//! **The wheels are `colorbalance`**, each range's three numbers [`Wheel::rgb`], a zero omitted.
//!
//! All three filters work in RGB: FFmpeg converts the picture once on the way in (they share one
//! negotiated format, read with the frames' own matrix) and once on the way out. **The way out is
//! Kerf's** for a tagged picture: FFmpeg 6.1 would convert back as BT.601 and leave the matrix tag
//! `unknown` (a BT.709 HD clip measured 4 levels off its ungraded neighbour, and delivered
//! mis-tagged), so the grade ends in a `scale` to the frames' matrix, a 4:4:4 format of the
//! negotiated depth, and the tag set again (`setparams`). An untagged picture is BT.601 both ways
//! already, and gets nothing.

use super::*;
use crate::model::{CurveSegment, CurveShape, Curves, Grade, Wheel, Wheels};

/// A constant for an expression: the exact text of the `f64`, a negative one in brackets.
fn num(v: f64) -> String {
    let s = fnum(v);
    if v < 0.0 {
        format!("({s})")
    } else {
        s
    }
}

fn segment_expr(s: &CurveSegment, var: &str) -> String {
    format!(
        "(st(3,({var}-{x0})*{k});{a}+ld(3)*({b}+ld(3)*({c}+ld(3)*{d})))",
        x0 = num(s.x0),
        k = num(s.k),
        a = num(s.a),
        b = num(s.b),
        c = num(s.c),
        d = num(s.d)
    )
}

/// The segments `lo..hi` as a balanced tree: the same choice as [`CurveShape::segment`].
fn segment_tree(shape: &CurveShape, lo: usize, hi: usize, var: &str) -> String {
    if hi - lo == 1 {
        return segment_expr(&shape.segments[lo], var);
    }
    let mid = (lo + hi) / 2;
    format!(
        "if(lt({var},{x}),{left},{right})",
        x = num(shape.points[mid].x),
        left = segment_tree(shape, lo, mid, var),
        right = segment_tree(shape, mid, hi, var)
    )
}

/// The curve of `var`, held flat outside its points: [`CurveShape::eval`].
pub(super) fn curve_expr(shape: &CurveShape, var: &str) -> String {
    let (first, last) = (shape.points[0], shape.points[shape.points.len() - 1]);
    format!(
        "if(lt({var},{x0}),{y0},if(gte({var},{xn}),{yn},{tree}))",
        x0 = num(first.x),
        y0 = num(first.y),
        xn = num(last.x),
        yn = num(last.y),
        tree = segment_tree(shape, 0, shape.segments.len(), var)
    )
}

/// The `lutrgb` of a clip's curves, or `None` when they draw the identity.
pub(super) fn curves_filter(curves: &Curves) -> Option<String> {
    let (channels, master) = curves.shapes();
    let mut parts = Vec::new();
    for (name, channel) in ["r", "g", "b"].iter().zip(&channels) {
        if channel.is_none() && master.is_none() {
            continue;
        }
        let mut e = "st(0,val/maxval);".to_string();
        for shape in [channel, &master].into_iter().flatten() {
            e.push_str(&format!("st(0,{});", curve_expr(shape, "ld(0)")));
        }
        e.push_str("round(maxval*clip(ld(0),0,1))");
        parts.push(format!("{name}='{e}'"));
    }
    (!parts.is_empty()).then(|| format!("lutrgb={}", parts.join(":")))
}

/// The `colorbalance` of a clip's wheels, or `None` when they are neutral.
pub(super) fn wheels_filter(wheels: &Wheels) -> Option<String> {
    let ranges: [(&str, &Wheel); 3] = [("s", &wheels.shadows), ("m", &wheels.midtones), ("h", &wheels.highlights)];
    let parts: Vec<String> = ranges
        .iter()
        .flat_map(|(range, wheel)| {
            ["r", "g", "b"]
                .iter()
                .zip(wheel.rgb())
                .filter(|(_, v)| *v != 0.0)
                .map(move |(c, v)| format!("{c}{range}={}", fnum(v)))
        })
        .collect();
    (!parts.is_empty()).then(|| format!("colorbalance={}", parts.join(":")))
}

/// The `lut3d` reading `path` (Kerf's copy of the LUT).
pub(super) fn lut_filter(path: &Path) -> String {
    format!(
        "lut3d=file='{}':interp=tetrahedral",
        escape_drawtext_path(&path.to_string_lossy())
    )
}

/// `scale`'s name for a matrix tag.
pub(super) fn scale_matrix(tag: &str) -> &str {
    match tag {
        "bt470bg" => "bt470",
        "bt2020nc" => "bt2020",
        other => other,
    }
}

/// Back from RGB into the frames' YCbCr matrix `tag`, tagged so (see the module docs).
pub(super) fn back_to_yuv(tag: &str) -> [String; 3] {
    [
        format!("scale=out_color_matrix={}", scale_matrix(tag)),
        "format=yuv444p|yuv444p10le|yuv444p12le|yuv444p16le".to_string(),
        format!("setparams=colorspace={tag}"),
    ]
}

/// The grade's filters, in chain order, for frames tagged `matrix`; empty for an identity grade.
///
/// Reads the disk for a LUT (its copy, [`crate::color_lut::lut_file`]): the builders run off the
/// project lock.
pub(super) fn grade_filters(grade: &Grade, matrix: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(path) = grade.lut().as_ref().and_then(crate::color_lut::lut_file) {
        out.push(lut_filter(&path));
    }
    out.extend(grade.curves().as_ref().and_then(curves_filter));
    out.extend(grade.wheels().as_ref().and_then(wheels_filter));
    if let Some(tag) = matrix.filter(|_| !out.is_empty()) {
        out.extend(back_to_yuv(tag));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ClipLut, CurvePoint};

    fn pts(p: &[(f64, f64)]) -> Vec<CurvePoint> {
        p.iter().map(|&(x, y)| CurvePoint { x, y }).collect()
    }

    /// The expression language, enough of it to evaluate what [`curves_filter`] writes:
    /// numbers, `+ - * /`, brackets, `;` and the functions it calls. Evaluated in the order
    /// FFmpeg's `av_expr` does (left operand first).
    struct Eval<'a> {
        s: &'a [u8],
        i: usize,
        vars: [f64; 10],
        val: f64,
        maxval: f64,
    }

    impl Eval<'_> {
        fn peek(&self) -> u8 {
            self.s.get(self.i).copied().unwrap_or(0)
        }
        fn eat(&mut self, c: u8) {
            assert_eq!(self.peek() as char, c as char, "at {}", self.i);
            self.i += 1;
        }
        fn seq(&mut self) -> f64 {
            let mut v = self.sum();
            while self.peek() == b';' {
                self.i += 1;
                v = self.sum();
            }
            v
        }
        fn sum(&mut self) -> f64 {
            let mut v = self.product();
            loop {
                match self.peek() {
                    b'+' => {
                        self.i += 1;
                        v += self.product();
                    }
                    b'-' => {
                        self.i += 1;
                        v -= self.product();
                    }
                    _ => return v,
                }
            }
        }
        fn product(&mut self) -> f64 {
            let mut v = self.unary();
            loop {
                match self.peek() {
                    b'*' => {
                        self.i += 1;
                        v *= self.unary();
                    }
                    b'/' => {
                        self.i += 1;
                        v /= self.unary();
                    }
                    _ => return v,
                }
            }
        }
        fn unary(&mut self) -> f64 {
            if self.peek() == b'-' {
                self.i += 1;
                return -self.unary();
            }
            self.atom()
        }
        fn args(&mut self) -> Vec<f64> {
            self.eat(b'(');
            let mut a = vec![self.seq()];
            while self.peek() == b',' {
                self.i += 1;
                a.push(self.seq());
            }
            self.eat(b')');
            a
        }
        fn atom(&mut self) -> f64 {
            let c = self.peek();
            if c == b'(' {
                self.i += 1;
                let v = self.seq();
                self.eat(b')');
                return v;
            }
            if c.is_ascii_digit() || c == b'.' {
                let start = self.i;
                while self.peek().is_ascii_digit() || self.peek() == b'.' {
                    self.i += 1;
                }
                return std::str::from_utf8(&self.s[start..self.i]).unwrap().parse().unwrap();
            }
            let start = self.i;
            while self.peek().is_ascii_alphabetic() {
                self.i += 1;
            }
            let name = std::str::from_utf8(&self.s[start..self.i]).unwrap().to_string();
            match name.as_str() {
                "val" => self.val,
                "maxval" => self.maxval,
                "if" => {
                    // Both branches are parsed; only one is taken.
                    self.eat(b'(');
                    let c = self.seq();
                    self.eat(b',');
                    let saved = self.vars;
                    let a = self.seq();
                    let after_a = self.vars;
                    self.vars = saved;
                    self.eat(b',');
                    let b = self.seq();
                    self.eat(b')');
                    if c != 0.0 {
                        self.vars = after_a;
                        a
                    } else {
                        b
                    }
                }
                "st" => {
                    self.eat(b'(');
                    let n = self.seq() as usize;
                    self.eat(b',');
                    let v = self.seq();
                    self.eat(b')');
                    self.vars[n] = v;
                    v
                }
                "ld" => self.vars[self.args()[0] as usize],
                "lt" => {
                    let a = self.args();
                    f64::from(u8::from(a[0] < a[1]))
                }
                "gte" => {
                    let a = self.args();
                    f64::from(u8::from(a[0] >= a[1]))
                }
                "clip" => {
                    let a = self.args();
                    a[0].clamp(a[1], a[2])
                }
                "round" => self.args()[0].round(),
                other => panic!("unknown name {other}"),
            }
        }
    }

    fn eval(expr: &str, val: f64, maxval: f64) -> f64 {
        let mut e = Eval {
            s: expr.as_bytes(),
            i: 0,
            vars: [0.0; 10],
            val,
            maxval,
        };
        let v = e.seq();
        assert_eq!(e.i, expr.len(), "the whole expression is read");
        v
    }

    /// The `r` / `g` / `b` expressions of a `lutrgb` filter.
    fn channels(filter: &str) -> Vec<(char, String)> {
        let body = filter.strip_prefix("lutrgb=").unwrap();
        body.split(":")
            .map(|part| {
                let (name, e) = part.split_once('=').unwrap();
                (name.chars().next().unwrap(), e.trim_matches('\'').to_string())
            })
            .collect()
    }

    #[test]
    fn the_expression_is_the_model_for_every_value_at_every_depth() {
        let curves = Curves {
            master: pts(&[(0.0, 0.05), (0.3, 0.2), (0.7, 0.85), (1.0, 0.95)]),
            red: pts(&[
                (0.0, 0.0),
                (0.1, 0.3),
                (0.2, 0.25),
                (0.4, 0.6),
                (0.5, 0.5),
                (0.6, 0.9),
                (0.8, 0.7),
                (1.0, 1.0),
            ]),
            green: Vec::new(),
            blue: pts(&[(0.2, 0.8), (0.9, 0.1)]),
        };
        let f = curves_filter(&curves).unwrap();
        let found = channels(&f);
        assert_eq!(found.iter().map(|c| c.0).collect::<String>(), "rgb");
        for (ch, (_, e)) in found.iter().enumerate() {
            for maxval in [255.0, 1023.0] {
                for v in 0..=(maxval as u32) {
                    let x = f64::from(v) / maxval;
                    let want = (maxval * curves.apply(ch, x)).round();
                    assert_eq!(eval(e, f64::from(v), maxval), want, "channel {ch} value {v} of {maxval}");
                }
            }
        }
    }

    #[test]
    fn a_channel_untouched_by_every_curve_is_left_out() {
        let curves = Curves {
            red: pts(&[(0.0, 1.0), (1.0, 0.0)]),
            ..Curves::default()
        };
        let f = curves_filter(&curves).unwrap();
        assert!(f.starts_with("lutrgb=r='") && !f.contains(":g=") && !f.contains(":b="), "{f}");
        assert_eq!(eval(&channels(&f)[0].1, 0.0, 255.0), 255.0);
        assert_eq!(curves_filter(&Curves::default()), None);
    }

    #[test]
    fn many_points_nest_as_a_tree() {
        let points: Vec<(f64, f64)> = (0..16)
            .map(|i| (i as f64 / 15.0, if i % 2 == 0 { 0.2 } else { 0.8 }))
            .collect();
        let f = curves_filter(&Curves {
            master: pts(&points),
            ..Curves::default()
        })
        .unwrap();
        let depth = f
            .chars()
            .scan(0i32, |d, c| {
                *d += match c {
                    '(' => 1,
                    ')' => -1,
                    _ => 0,
                };
                Some(*d)
            })
            .max()
            .unwrap();
        assert!(depth < 40, "nesting {depth}");
    }

    #[test]
    fn wheels_are_colorbalance_numbers_and_zero_is_left_out() {
        let w = Wheels {
            shadows: Wheel {
                x: 1.0,
                y: 0.0,
                lightness: 0.0,
            },
            midtones: Wheel::default(),
            highlights: Wheel {
                x: 0.0,
                y: 0.0,
                lightness: -0.25,
            },
        };
        assert_eq!(
            wheels_filter(&w).unwrap(),
            "colorbalance=rs=1:gs=-0.5:bs=-0.5:rh=-0.25:gh=-0.25:bh=-0.25"
        );
        assert_eq!(wheels_filter(&Wheels::default()), None);
    }

    #[test]
    fn a_lut_path_is_escaped_for_the_option_and_the_graph() {
        assert_eq!(
            lut_filter(Path::new("C:\\Users\\O'Neil\\luts\\a b.cube")),
            "lut3d=file='C\\:\\\\Users\\\\O\\'\\''Neil\\\\luts\\\\a b.cube':interp=tetrahedral"
        );
    }

    #[test]
    fn an_identity_grade_adds_nothing_and_a_missing_lut_is_left_out() {
        assert!(grade_filters(&Grade::default(), None).is_empty());
        assert!(grade_filters(&Grade::default(), Some("bt709")).is_empty());
        let g = Grade {
            lut: Some(ClipLut {
                path: "/definitely/not/here.cube".into(),
                intensity: 1.0,
            }),
            wheels: Some(Wheels {
                midtones: Wheel {
                    x: 0.0,
                    y: 0.0,
                    lightness: 0.1,
                },
                ..Wheels::default()
            }),
            ..Grade::default()
        };
        assert_eq!(grade_filters(&g, None), vec!["colorbalance=rm=0.1:gm=0.1:bm=0.1".to_string()]);
    }

    #[test]
    fn an_export_refuses_a_lut_it_cannot_apply() {
        use crate::engine::test_support::{make_clip, single};
        let mut clip = make_clip(uuid::Uuid::new_v4(), 0.0, 1.0, 0.0);
        clip.grade.lut = Some(ClipLut {
            path: "/definitely/not/here.cube".into(),
            intensity: 1.0,
        });
        let e = check_export_luts(&single(vec![clip.clone()])).unwrap_err().to_string();
        assert!(e.contains("/definitely/not/here.cube") && e.contains("cannot export"), "{e}");
        // At 0 % it changes nothing and is left out, so it does not stop the export; nor does a
        // disabled clip.
        clip.grade.lut.as_mut().unwrap().intensity = 0.0;
        assert!(check_export_luts(&single(vec![clip.clone()])).is_ok());
        clip.grade.lut.as_mut().unwrap().intensity = 1.0;
        clip.enabled = false;
        assert!(check_export_luts(&single(vec![clip])).is_ok());
    }

    /// The argv text of a grade, pinned the way the golden oracle pins a graph: a change here is a
    /// change to every graded export.
    #[test]
    fn the_grade_text_is_pinned() {
        let g = Grade {
            curves: Some(Curves {
                master: pts(&[(0.0, 0.1), (0.5, 0.6), (1.0, 0.9)]),
                blue: pts(&[(0.0, 1.0), (1.0, 0.0)]),
                ..Curves::default()
            }),
            wheels: Some(Wheels {
                shadows: Wheel {
                    x: -0.3,
                    y: 0.0,
                    lightness: 0.0,
                },
                highlights: Wheel {
                    x: 0.0,
                    y: 0.2,
                    lightness: 0.05,
                },
                ..Wheels::default()
            }),
            lut: None,
        };
        assert_eq!(
            grade_filters(&g, Some("bt709")).join(","),
            concat!(
                r"lutrgb=r='st(0,val/maxval);st(0,if(lt(ld(0),0),0.1,if(gte(ld(0),1),0.9,if(lt(ld(0),0.5),(st(3,(ld(0)-0)*2);0.1+ld(3)*(0.5+ld(3)*(0.09999999999999998+ld(3)*(-0.09999999999999998)))),(st(3,(ld(0)-0.5)*2);0.6+ld(3)*(0.4+ld(3)*((-0.19999999999999996)+ld(3)*0.09999999999999998)))))));round(maxval*clip(ld(0),0,1))':g='st(0,val/maxval);st(0,if(lt(ld(0),0),0.1,if(gte(ld(0),1),0.9,if(lt(ld(0),0.5),(st(3,(ld(0)-0)*2);0.1+ld(3)*(0.5+ld(3)*(0.09999999999999998+ld(3)*(-0.09999999999999998)))),(st(3,(ld(0)-0.5)*2);0.6+ld(3)*(0.4+ld(3)*((-0.19999999999999996)+ld(3)*0.09999999999999998)))))));round(maxval*clip(ld(0),0,1))':b='st(0,val/maxval);st(0,if(lt(ld(0),0),1,if(gte(ld(0),1),0,(st(3,(ld(0)-0)*1);1+ld(3)*((-1)+ld(3)*(0+ld(3)*0))))));st(0,if(lt(ld(0),0),0.1,if(gte(ld(0),1),0.9,if(lt(ld(0),0.5),(st(3,(ld(0)-0)*2);0.1+ld(3)*(0.5+ld(3)*(0.09999999999999998+ld(3)*(-0.09999999999999998)))),(st(3,(ld(0)-0.5)*2);0.6+ld(3)*(0.4+ld(3)*((-0.19999999999999996)+ld(3)*0.09999999999999998)))))));round(maxval*clip(ld(0),0,1))'",
                r",colorbalance=rs=-0.3:gs=0.15:bs=0.15:rh=0.05:gh=0.2232:bh=-0.1232,scale=out_color_matrix=bt709,format=yuv444p|yuv444p10le|yuv444p12le|yuv444p16le,setparams=colorspace=bt709"
            )
        );
        assert_eq!(
            grade_filters(&g, Some("bt470bg")).last().unwrap(),
            "setparams=colorspace=bt470bg"
        );
        assert!(grade_filters(&g, Some("bt470bg")).contains(&"scale=out_color_matrix=bt470".to_string()));
    }
}
