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

/// ffmpeg's colour-name table, lower-cased and sorted (so a lookup is a binary search). Checked
/// against the real binary by `every_name_is_parsed_by_ffmpeg`.
const NAMES: &[&str] = &[
    "aliceblue",
    "antiquewhite",
    "aqua",
    "aquamarine",
    "azure",
    "beige",
    "bisque",
    "black",
    "blanchedalmond",
    "blue",
    "blueviolet",
    "brown",
    "burlywood",
    "cadetblue",
    "chartreuse",
    "chocolate",
    "coral",
    "cornflowerblue",
    "cornsilk",
    "crimson",
    "cyan",
    "darkblue",
    "darkcyan",
    "darkgoldenrod",
    "darkgray",
    "darkgreen",
    "darkkhaki",
    "darkmagenta",
    "darkolivegreen",
    "darkorange",
    "darkorchid",
    "darkred",
    "darksalmon",
    "darkseagreen",
    "darkslateblue",
    "darkslategray",
    "darkturquoise",
    "darkviolet",
    "deeppink",
    "deepskyblue",
    "dimgray",
    "dodgerblue",
    "firebrick",
    "floralwhite",
    "forestgreen",
    "fuchsia",
    "gainsboro",
    "ghostwhite",
    "gold",
    "goldenrod",
    "gray",
    "green",
    "greenyellow",
    "honeydew",
    "hotpink",
    "indianred",
    "indigo",
    "ivory",
    "khaki",
    "lavender",
    "lavenderblush",
    "lawngreen",
    "lemonchiffon",
    "lightblue",
    "lightcoral",
    "lightcyan",
    "lightgoldenrodyellow",
    "lightgreen",
    "lightgrey",
    "lightpink",
    "lightsalmon",
    "lightseagreen",
    "lightskyblue",
    "lightslategray",
    "lightsteelblue",
    "lightyellow",
    "lime",
    "limegreen",
    "linen",
    "magenta",
    "maroon",
    "mediumaquamarine",
    "mediumblue",
    "mediumorchid",
    "mediumpurple",
    "mediumseagreen",
    "mediumslateblue",
    "mediumspringgreen",
    "mediumturquoise",
    "mediumvioletred",
    "midnightblue",
    "mintcream",
    "mistyrose",
    "moccasin",
    "navajowhite",
    "navy",
    "oldlace",
    "olive",
    "olivedrab",
    "orange",
    "orangered",
    "orchid",
    "palegoldenrod",
    "palegreen",
    "paleturquoise",
    "palevioletred",
    "papayawhip",
    "peachpuff",
    "peru",
    "pink",
    "plum",
    "powderblue",
    "purple",
    "red",
    "rosybrown",
    "royalblue",
    "saddlebrown",
    "salmon",
    "sandybrown",
    "seagreen",
    "seashell",
    "sienna",
    "silver",
    "skyblue",
    "slateblue",
    "slategray",
    "snow",
    "springgreen",
    "steelblue",
    "tan",
    "teal",
    "thistle",
    "tomato",
    "turquoise",
    "violet",
    "wheat",
    "white",
    "whitesmoke",
    "yellow",
    "yellowgreen",
];

/// Whether `name` is one of ffmpeg's colour names (any case).
fn is_name(name: &str) -> bool {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    NAMES.binary_search(&lower.as_str()).is_ok()
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
        assert!(NAMES.windows(2).all(|w| w[0] < w[1]), "sorted and unique");
        assert!(NAMES.iter().all(|n| n.bytes().all(|b| b.is_ascii_lowercase())));
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
        let mut specs: Vec<String> = NAMES.iter().map(|n| (*n).to_string()).collect();
        specs.extend(NAMES.iter().map(|n| n.to_uppercase()));
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
