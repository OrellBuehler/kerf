//! Colour specs that are safe to splice into a filter value and that ffmpeg actually parses.
//!
//! `color` / `bg` / the chroma key's colour reach `fontcolor=`, `boxcolor=` and `chromakey=`
//! unquoted, so a comma or colon in one would start a new filter node or option. And a spec
//! ffmpeg cannot parse (`#fff`, `grey`, `0XAABBCC`) fails the whole render, not just the one
//! overlay. So the check is ffmpeg's own grammar (`av_parse_color`, libavutil/parseutils.c), not
//! "looks like a colour": a name from its table (case-insensitive), `#RRGGBB[AA]`, `0xRRGGBB[AA]`
//! (a lower-case `0x` only), each with an optional `@alpha` that is a decimal `0..1` or `0x0..0xff`.
//! Left out on purpose: `random` / `bikeshed` (a different colour on every parse, so a preview and
//! its export disagree), a bare `RRGGBB` (indistinguishable from a typo), and `#RGB`, which
//! ffmpeg rejects.

/// ffmpeg's colour-name table (`color_table` in libavutil/parseutils.c) with each name's RGB,
/// lower-cased and sorted (so a lookup is a binary search). Checked against the real binary by
/// `every_name_is_parsed_by_ffmpeg`.
const NAMES: &[(&str, [u8; 3])] = &[
    ("aliceblue", [240, 248, 255]),
    ("antiquewhite", [250, 235, 215]),
    ("aqua", [0, 255, 255]),
    ("aquamarine", [127, 255, 212]),
    ("azure", [240, 255, 255]),
    ("beige", [245, 245, 220]),
    ("bisque", [255, 228, 196]),
    ("black", [0, 0, 0]),
    ("blanchedalmond", [255, 235, 205]),
    ("blue", [0, 0, 255]),
    ("blueviolet", [138, 43, 226]),
    ("brown", [165, 42, 42]),
    ("burlywood", [222, 184, 135]),
    ("cadetblue", [95, 158, 160]),
    ("chartreuse", [127, 255, 0]),
    ("chocolate", [210, 105, 30]),
    ("coral", [255, 127, 80]),
    ("cornflowerblue", [100, 149, 237]),
    ("cornsilk", [255, 248, 220]),
    ("crimson", [220, 20, 60]),
    ("cyan", [0, 255, 255]),
    ("darkblue", [0, 0, 139]),
    ("darkcyan", [0, 139, 139]),
    ("darkgoldenrod", [184, 134, 11]),
    ("darkgray", [169, 169, 169]),
    ("darkgreen", [0, 100, 0]),
    ("darkkhaki", [189, 183, 107]),
    ("darkmagenta", [139, 0, 139]),
    ("darkolivegreen", [85, 107, 47]),
    ("darkorange", [255, 140, 0]),
    ("darkorchid", [153, 50, 204]),
    ("darkred", [139, 0, 0]),
    ("darksalmon", [233, 150, 122]),
    ("darkseagreen", [143, 188, 143]),
    ("darkslateblue", [72, 61, 139]),
    ("darkslategray", [47, 79, 79]),
    ("darkturquoise", [0, 206, 209]),
    ("darkviolet", [148, 0, 211]),
    ("deeppink", [255, 20, 147]),
    ("deepskyblue", [0, 191, 255]),
    ("dimgray", [105, 105, 105]),
    ("dodgerblue", [30, 144, 255]),
    ("firebrick", [178, 34, 34]),
    ("floralwhite", [255, 250, 240]),
    ("forestgreen", [34, 139, 34]),
    ("fuchsia", [255, 0, 255]),
    ("gainsboro", [220, 220, 220]),
    ("ghostwhite", [248, 248, 255]),
    ("gold", [255, 215, 0]),
    ("goldenrod", [218, 165, 32]),
    ("gray", [128, 128, 128]),
    ("green", [0, 128, 0]),
    ("greenyellow", [173, 255, 47]),
    ("honeydew", [240, 255, 240]),
    ("hotpink", [255, 105, 180]),
    ("indianred", [205, 92, 92]),
    ("indigo", [75, 0, 130]),
    ("ivory", [255, 255, 240]),
    ("khaki", [240, 230, 140]),
    ("lavender", [230, 230, 250]),
    ("lavenderblush", [255, 240, 245]),
    ("lawngreen", [124, 252, 0]),
    ("lemonchiffon", [255, 250, 205]),
    ("lightblue", [173, 216, 230]),
    ("lightcoral", [240, 128, 128]),
    ("lightcyan", [224, 255, 255]),
    ("lightgoldenrodyellow", [250, 250, 210]),
    ("lightgreen", [144, 238, 144]),
    ("lightgrey", [211, 211, 211]),
    ("lightpink", [255, 182, 193]),
    ("lightsalmon", [255, 160, 122]),
    ("lightseagreen", [32, 178, 170]),
    ("lightskyblue", [135, 206, 250]),
    ("lightslategray", [119, 136, 153]),
    ("lightsteelblue", [176, 196, 222]),
    ("lightyellow", [255, 255, 224]),
    ("lime", [0, 255, 0]),
    ("limegreen", [50, 205, 50]),
    ("linen", [250, 240, 230]),
    ("magenta", [255, 0, 255]),
    ("maroon", [128, 0, 0]),
    ("mediumaquamarine", [102, 205, 170]),
    ("mediumblue", [0, 0, 205]),
    ("mediumorchid", [186, 85, 211]),
    ("mediumpurple", [147, 112, 216]),
    ("mediumseagreen", [60, 179, 113]),
    ("mediumslateblue", [123, 104, 238]),
    ("mediumspringgreen", [0, 250, 154]),
    ("mediumturquoise", [72, 209, 204]),
    ("mediumvioletred", [199, 21, 133]),
    ("midnightblue", [25, 25, 112]),
    ("mintcream", [245, 255, 250]),
    ("mistyrose", [255, 228, 225]),
    ("moccasin", [255, 228, 181]),
    ("navajowhite", [255, 222, 173]),
    ("navy", [0, 0, 128]),
    ("oldlace", [253, 245, 230]),
    ("olive", [128, 128, 0]),
    ("olivedrab", [107, 142, 35]),
    ("orange", [255, 165, 0]),
    ("orangered", [255, 69, 0]),
    ("orchid", [218, 112, 214]),
    ("palegoldenrod", [238, 232, 170]),
    ("palegreen", [152, 251, 152]),
    ("paleturquoise", [175, 238, 238]),
    ("palevioletred", [216, 112, 147]),
    ("papayawhip", [255, 239, 213]),
    ("peachpuff", [255, 218, 185]),
    ("peru", [205, 133, 63]),
    ("pink", [255, 192, 203]),
    ("plum", [221, 160, 221]),
    ("powderblue", [176, 224, 230]),
    ("purple", [128, 0, 128]),
    ("red", [255, 0, 0]),
    ("rosybrown", [188, 143, 143]),
    ("royalblue", [65, 105, 225]),
    ("saddlebrown", [139, 69, 19]),
    ("salmon", [250, 128, 114]),
    ("sandybrown", [244, 164, 96]),
    ("seagreen", [46, 139, 87]),
    ("seashell", [255, 245, 238]),
    ("sienna", [160, 82, 45]),
    ("silver", [192, 192, 192]),
    ("skyblue", [135, 206, 235]),
    ("slateblue", [106, 90, 205]),
    ("slategray", [112, 128, 144]),
    ("snow", [255, 250, 250]),
    ("springgreen", [0, 255, 127]),
    ("steelblue", [70, 130, 180]),
    ("tan", [210, 180, 140]),
    ("teal", [0, 128, 128]),
    ("thistle", [216, 191, 216]),
    ("tomato", [255, 99, 71]),
    ("turquoise", [64, 224, 208]),
    ("violet", [238, 130, 238]),
    ("wheat", [245, 222, 179]),
    ("white", [255, 255, 255]),
    ("whitesmoke", [245, 245, 245]),
    ("yellow", [255, 255, 0]),
    ("yellowgreen", [154, 205, 50]),
];

/// Whether `name` is one of ffmpeg's colour names (any case).
fn is_name(name: &str) -> bool {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    rgb_of_name(name).is_some()
}

fn rgb_of_name(name: &str) -> Option<[u8; 3]> {
    let lower = name.to_ascii_lowercase();
    NAMES
        .binary_search_by_key(&lower.as_str(), |(n, _)| n)
        .ok()
        .map(|i| NAMES[i].1)
}

/// `#RRGGBB[AA]` / `0xRRGGBB[AA]` — six or eight hex digits after the prefix. ffmpeg checks the
/// `0x` prefix case-sensitively (`0XAABBCC` is read as a name and fails).
fn is_hex(base: &str) -> bool {
    let digits = base.strip_prefix('#').or_else(|| base.strip_prefix("0x"));
    digits.is_some_and(|d| matches!(d.len(), 6 | 8) && d.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The part after `@`: a decimal in `0..=1` (digits and at most one point, no sign or exponent)
/// or a lower-case `0x` and one or two hex digits (`0x00..=0xff`).
fn is_alpha(alpha: &str) -> bool {
    if let Some(hex) = alpha.strip_prefix("0x") {
        return matches!(hex.len(), 1 | 2) && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    alpha.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && alpha.bytes().filter(|&b| b == b'.').count() <= 1
        && alpha.bytes().any(|b| b.is_ascii_digit())
        && alpha.parse::<f64>().is_ok_and(|a| (0.0..=1.0).contains(&a))
}

/// Whether `s` is a colour ffmpeg parses and that is safe to splice unquoted into a filter value
/// (see the module docs for the grammar).
pub(crate) fn valid_color(s: &str) -> bool {
    let (base, alpha) = s.split_once('@').map_or((s, None), |(b, a)| (b, Some(a)));
    if alpha.is_some_and(|a| !is_alpha(a)) {
        return false;
    }
    is_hex(base) || is_name(base)
}

/// The red, green and blue `av_parse_color` reads from a [`valid_color`] (its alpha left out), or
/// `None` for anything else.
pub fn color_rgb(s: &str) -> Option<[u8; 3]> {
    if !valid_color(s) {
        return None;
    }
    let base = s.split_once('@').map_or(s, |(b, _)| b);
    match base.strip_prefix('#').or_else(|| base.strip_prefix("0x")) {
        Some(hex) => {
            let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
            Some([byte(0)?, byte(2)?, byte(4)?])
        }
        None => rgb_of_name(base),
    }
}

/// `s` if [`valid_color`], else `fallback` — the safe way to splice a
/// caller-controlled colour into a filter value; rendering must never fail or
/// inject over a bad one.
pub(crate) fn safe_color<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if valid_color(s) {
        s
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_lower_case_and_sorted_so_the_lookup_can_bisect() {
        assert!(NAMES.windows(2).all(|w| w[0].0 < w[1].0), "sorted and unique");
        assert!(NAMES.iter().all(|(n, _)| n.bytes().all(|b| b.is_ascii_lowercase())));
    }

    #[test]
    fn names_hex_and_alpha_in_ffmpegs_forms_are_accepted() {
        for ok in [
            "white",
            "AliceBlue",
            "LIGHTGREY",
            "#fff000",
            "#FFAA00CC",
            "0x000000",
            "0xAABBCCDD",
            "white@0.5",
            "black@1",
            "black@0",
            "black@1.0",
            "black@.5",
            "black@0.",
            "yellow@0x80",
            "yellow@0xFF",
            "yellow@0x8",
            "#00ff00@0.9",
        ] {
            assert!(valid_color(ok), "{ok}");
        }
    }

    #[test]
    fn what_ffmpeg_rejects_or_that_could_inject_is_rejected() {
        for bad in [
            "white,drawtext=textfile='/etc/passwd'",
            "white:x=1",
            "white;black",
            "white'",
            "white\n",
            "white ",
            " white",
            "",
            "@0.5",
            "#",
            "#ff",
            "#fff",
            "#12345",
            "#ggggggg",
            "0xZZZZZZ",
            "0x",
            "0xfff",
            "0XAABBCCDD",
            "ff0000",
            "notacolour",
            "grey",
            "darkgrey",
            "lightgray",
            "rebeccapurple",
            "transparent",
            "random",
            "bikeshed",
            "none",
            "white@",
            "white@@1",
            "white@1.2.3",
            "white@nan",
            "white@inf",
            "white@-0.1",
            "white@1.5",
            "white@2",
            "white@1e-1",
            "white@+0.5",
            "white@0.5@0.5",
            "white@0x",
            "white@0X80",
            "white@0x100",
            "white@0x1ff",
            "white@0xzz",
            "white@.",
            "white@0.5,x",
            "çolor",
        ] {
            assert!(!valid_color(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_bad_colour_falls_back() {
        assert_eq!(safe_color("not a colour", "green"), "green");
        assert_eq!(safe_color("Red@0.5", "green"), "Red@0.5");
    }

    /// Every name in the table, the hex forms and the alpha forms through the real parser: a
    /// colour `valid_color` lets into a graph must be one ffmpeg renders, or a typo costs an
    /// export. `cargo test -p kerf-core --no-default-features -- --ignored every_name_is_parsed`
    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn every_name_is_parsed_by_ffmpeg() {
        use crate::engine::cli::{command, ffmpeg_bin};
        use crate::engine::test_support::StatusBounded;
        use std::process::Stdio;

        let renders = |spec: &str| -> bool {
            command(&ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
                .arg(format!("color=c={spec}:s=16x16:d=0.04"))
                .args(["-frames:v", "1", "-f", "null", "-"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status_bounded()
                .expect("run ffmpeg")
                .success()
        };
        let mut specs: Vec<String> = NAMES.iter().map(|(n, _)| (*n).to_string()).collect();
        specs.extend(NAMES.iter().map(|(n, _)| n.to_uppercase()));
        specs.extend(
            [
                "#fff000",
                "#FFAA00CC",
                "0x000000",
                "0xAABBCCDD",
                "white@0.5",
                "black@1",
                "black@0",
                "black@.5",
                "black@0.",
                "yellow@0x80",
                "yellow@0xFF",
                "yellow@0x8",
                "#00ff00@0.9",
                "AliceBlue@0.25",
            ]
            .map(String::from),
        );
        let failed: Vec<&String> = specs.iter().filter(|s| valid_color(s) && !renders(s)).collect();
        assert!(failed.is_empty(), "accepted by valid_color but refused by ffmpeg: {failed:?}");
        let table_unaccepted: Vec<&String> = specs.iter().filter(|s| !valid_color(s)).collect();
        assert!(table_unaccepted.is_empty(), "in the table but rejected: {table_unaccepted:?}");
    }

    #[test]
    fn a_colours_rgb_is_read_from_its_name_or_its_hex_digits_and_its_alpha_is_left_out() {
        assert_eq!(color_rgb("green"), Some([0, 128, 0]));
        assert_eq!(color_rgb("LimeGreen"), Some([50, 205, 50]));
        assert_eq!(color_rgb("#FFAA00"), Some([255, 170, 0]));
        assert_eq!(color_rgb("#ffaa00cc"), Some([255, 170, 0]));
        assert_eq!(color_rgb("0x123456"), Some([0x12, 0x34, 0x56]));
        assert_eq!(color_rgb("0xabcdef80"), Some([0xab, 0xcd, 0xef]));
        assert_eq!(color_rgb("white@0.5"), Some([255, 255, 255]));
        assert_eq!(color_rgb("#00ff00@0x80"), Some([0, 255, 0]));
        for bad in ["grey", "#fff", "0XAABBCC", "white,black", "white@2", ""] {
            assert_eq!(color_rgb(bad), None, "{bad}");
        }
    }

    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn every_colour_is_the_rgb_ffmpeg_draws_it_in() {
        use crate::engine::cli::{command, ffmpeg_bin, output_bounded};

        let drawn = |spec: &str| -> Vec<u8> {
            let mut cmd = command(&ffmpeg_bin());
            cmd.args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
                .arg(format!("color=c={spec}:s=2x2,format=rgb24"))
                .args(["-frames:v", "1", "-f", "rawvideo", "-"]);
            let out = output_bounded(&mut cmd, std::time::Duration::from_secs(60), 0).expect("run ffmpeg");
            out.stdout[..3].to_vec()
        };
        let mut specs: Vec<&str> = NAMES.iter().map(|(n, _)| *n).collect();
        specs.extend(["#FFAA00CC", "0x123456", "0xabcdef80", "white@0.5", "#00ff00@0.9", "LimeGreen"]);
        for spec in specs {
            assert_eq!(color_rgb(spec).map(Vec::from), Some(drawn(spec)), "{spec}");
        }
        assert_eq!(color_rgb("grey"), None);
        assert_eq!(color_rgb("white,black"), None);
    }

    /// The other direction, for what is left out: ffmpeg really does refuse these, so keeping
    /// them out is not squeamishness.
    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn what_valid_color_rejects_that_looks_like_a_colour_is_refused_by_ffmpeg_too() {
        use crate::engine::cli::{command, ffmpeg_bin};
        use crate::engine::test_support::StatusBounded;
        use std::process::Stdio;

        for spec in [
            "#fff",
            "0xfff",
            "0XAABBCCDD",
            "grey",
            "darkgrey",
            "lightgray",
            "rebeccapurple",
            "white@1.5",
            "white@0x1ff",
        ] {
            assert!(!valid_color(spec), "{spec}");
            let ok = command(&ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
                .arg(format!("color=c={spec}:s=16x16:d=0.04"))
                .args(["-frames:v", "1", "-f", "null", "-"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status_bounded()
                .expect("run ffmpeg")
                .success();
            assert!(!ok, "{spec} was accepted by ffmpeg after all");
        }
    }
}
