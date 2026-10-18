//! The fonts a project embeds, through the real process-wide registry (the unit tests of
//! `kerf_core::fonts` keep one per thread so that they cannot see each other): a font registered on one
//! thread is the one every other thread draws with, and taking it away puts the pictures back.

use kerf_core::fonts::{self, EmbeddedFont};
use kerf_core::svg::raster;

const BOLD: &[u8] = include_bytes!("../assets/fonts/Geist-Bold.ttf");

/// The shipped bold face under another family name (as many characters as "Geist").
fn renamed(to: &str) -> Vec<u8> {
    fn replace(data: &mut [u8], from: &[u8], to: &[u8]) {
        let mut at = 0;
        while at + from.len() <= data.len() {
            if &data[at..at + from.len()] == from {
                data[at..at + to.len()].copy_from_slice(to);
                at += from.len();
            } else {
                at += 1;
            }
        }
    }
    let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<u8>>();
    let mut data = BOLD.to_vec();
    replace(&mut data, b"Geist", to.as_bytes());
    replace(&mut data, &utf16("Geist"), &utf16(to));
    data
}

fn drawn(family: &str, weight: &str) -> Vec<u8> {
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 40"><text x="4" y="30" font-size="28" font-family="{family}" font-weight="{weight}">Kerf</text></svg>"#
    );
    // On a thread of its own: the registry is the process's, not the caller's.
    std::thread::spawn(move || raster::render(&svg, 120, 40, 1.0).unwrap().data().to_vec())
        .join()
        .unwrap()
}

#[test]
fn an_embedded_font_is_drawn_with_on_every_thread_and_taking_it_away_restores_the_picture() {
    assert_eq!(fonts::embedded_digest(), None);
    let fallback = drawn("Zeist", "400");
    let bold_geist = drawn("Geist", "700");
    assert_ne!(fallback, bold_geist, "the fixture needs a difference to see");

    assert!(fonts::register_embedded(&[EmbeddedFont::new("Zeist", renamed("Zeist"))]));
    let digest = fonts::embedded_digest().expect("a digest");
    assert!(fonts::has_family("Zeist"));
    assert_eq!(
        drawn("Zeist", "400"),
        bold_geist,
        "the embedded bold face, found by its own family"
    );

    // Replacing with the same set changes nothing; with another set it is another digest.
    assert!(!fonts::replace_embedded(fonts::embedded_fonts()));
    assert_eq!(fonts::embedded_digest().as_deref(), Some(digest.as_str()));
    assert!(fonts::replace_embedded(vec![EmbeddedFont::new("Zeyst", renamed("Zeyst"))]));
    assert_ne!(fonts::embedded_digest().unwrap(), digest);
    assert_eq!(
        drawn("Zeist", "400"),
        fallback,
        "the first font is gone with the project that had it"
    );

    assert!(fonts::replace_embedded(Vec::new()));
    assert_eq!(fonts::embedded_digest(), None);
    assert_eq!(drawn("Zeyst", "400"), fallback);
}
