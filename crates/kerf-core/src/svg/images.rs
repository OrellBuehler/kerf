//! How big an embedded raster image may be.
//!
//! resvg decodes an `<image>` every time it draws one, and the decoders allocate the *whole* output
//! from the size the file's header claims: a few hundred bytes of PNG that say 60,000 x 60,000
//! pixels make `tiny-skia` ask for 14 GB and abort the process (an allocation failure is not an
//! error in Rust). So the header is read first — [`imagesize`], the crate usvg itself sizes an
//! image with — and an image over [`MAX_IMAGE_PIXELS`] is never stored (the sanitiser cuts its
//! `href`) and never handed to the decoder (the rasteriser's resolver answers "nothing").

use std::fmt;

/// The most pixels one embedded image may have (a 4K frame is 8.3 million). Decoded it is four
/// bytes a pixel, twice over while it is converted: 128 MB at the cap.
pub(crate) const MAX_IMAGE_PIXELS: u64 = 16_000_000;

/// The most decoded image pixels one frame may draw, every `<use>` of an `<image>` counted again
/// (each draw decodes the file again): four images at the cap, or a 1-megapixel logo 64 times.
pub(crate) const MAX_FRAME_IMAGE_PIXELS: u64 = 64_000_000;

/// The raster formats a title may embed; the same four the sanitiser lets through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl Format {
    pub(crate) fn of_mime(mime: &str) -> Option<Self> {
        match mime {
            "image/png" => Some(Format::Png),
            "image/jpeg" | "image/jpg" => Some(Format::Jpeg),
            "image/gif" => Some(Format::Gif),
            "image/webp" => Some(Format::Webp),
            _ => None,
        }
    }

    /// Whether `data` starts the way this format does. The decoder is chosen by the mime type, so
    /// bytes of another kind fail in it by themselves; this keeps [`imagesize`], which recognises a
    /// dozen formats by their bytes, from vouching for a size the chosen decoder would not read.
    fn has_magic(self, data: &[u8]) -> bool {
        match self {
            Format::Png => data.starts_with(b"\x89PNG\r\n\x1a\n"),
            Format::Jpeg => data.starts_with(&[0xFF, 0xD8]),
            Format::Gif => data.starts_with(b"GIF8"),
            Format::Webp => data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP",
        }
    }
}

/// Why an embedded image is not drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refused {
    /// Not a PNG, JPEG, GIF or WebP, or its header says nothing usable.
    Unreadable,
    /// Over [`MAX_IMAGE_PIXELS`] (the size is width x height as the header gives it).
    TooLarge(u64, u64),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::Unreadable => {
                f.write_str("an embedded image could not be read (only PNG, JPEG, GIF and WebP are) and was removed")
            }
            Refused::TooLarge(w, h) => write!(
                f,
                "an embedded image of {w} x {h} pixels is larger than {} megapixels and was removed",
                MAX_IMAGE_PIXELS / 1_000_000
            ),
        }
    }
}

/// The pixels of `data` as `mime` names its format, read from the header alone.
pub(crate) fn check(mime: &str, data: &[u8]) -> Result<u64, Refused> {
    let format = Format::of_mime(mime).ok_or(Refused::Unreadable)?;
    if !format.has_magic(data) {
        return Err(Refused::Unreadable);
    }
    let size = imagesize::blob_size(data).map_err(|_| Refused::Unreadable)?;
    let (w, h) = (size.width as u64, size.height as u64);
    if w == 0 || h == 0 {
        return Err(Refused::Unreadable);
    }
    let pixels = w.saturating_mul(h);
    if pixels > MAX_IMAGE_PIXELS {
        return Err(Refused::TooLarge(w, h));
    }
    Ok(pixels)
}

/// The mime type and bytes of a `data:` URL, decoded the way usvg decodes one (so what is checked
/// here is what the decoder is later given).
pub(crate) fn decode_data_url(href: &str) -> Option<(String, Vec<u8>)> {
    let url = data_url::DataUrl::process(href).ok()?;
    let (data, _) = url.decode_to_vec().ok()?;
    let mime = format!("{}/{}", url.mime_type().type_, url.mime_type().subtype);
    Some((mime, data))
}

/// [`check`] for the `href` of an `<image>` / `<feImage>`: `Ok(pixels)` for a `data:` image that
/// can be drawn.
pub(crate) fn check_href(href: &str) -> Result<u64, Refused> {
    let (mime, data) = decode_data_url(href).ok_or(Refused::Unreadable)?;
    check(&mime, &data)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A 4 x 4 red PNG, base64: a logo in a lower third is this, larger.
    pub(crate) const RED_4X4: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAYAAACp8Z5+AAAAEklEQVR4nGP4z8DwHxkzkC4AADxAH+HggXe0AAAAAElFTkSuQmCC";

    /// A PNG header (and nothing else) claiming `w` x `h` pixels: enough for [`imagesize`], and
    /// what makes a decoder allocate `w * h * 4` bytes before it notices the data is not there.
    pub(crate) fn png_claiming(w: u32, h: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&w.to_be_bytes());
        png.extend_from_slice(&h.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png.extend_from_slice(&[0, 0, 0, 0]);
        png
    }

    pub(crate) fn data_url(mime: &str, bytes: &[u8]) -> String {
        use std::fmt::Write;
        let mut out = format!("data:{mime};base64,");
        const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
            for i in 0..4 {
                if i <= chunk.len() {
                    let _ = out.write_char(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn a_size_is_read_from_the_header_and_held_to_the_cap() {
        assert_eq!(check("image/png", &png_claiming(4, 4)), Ok(16));
        assert_eq!(check("image/png", &png_claiming(4000, 4000)), Ok(16_000_000));
        assert_eq!(
            check("image/png", &png_claiming(4001, 4000)),
            Err(Refused::TooLarge(4001, 4000))
        );
        assert_eq!(
            check("image/png", &png_claiming(60_000, 60_000)),
            Err(Refused::TooLarge(60_000, 60_000))
        );
        assert_eq!(check("image/png", &png_claiming(0, 10)), Err(Refused::Unreadable));
    }

    #[test]
    fn bytes_of_another_kind_or_no_kind_are_not_vouched_for() {
        // The mime type chooses the decoder: a PNG called a JPEG is unreadable, not "a small PNG".
        assert_eq!(check("image/jpeg", &png_claiming(4, 4)), Err(Refused::Unreadable));
        assert_eq!(check("image/svg+xml", &png_claiming(4, 4)), Err(Refused::Unreadable));
        assert_eq!(check("image/png", b"not an image"), Err(Refused::Unreadable));
        assert_eq!(check("image/png", b""), Err(Refused::Unreadable));
        assert_eq!(check("image/gif", &png_claiming(4, 4)), Err(Refused::Unreadable));
        assert_eq!(check("image/webp", &png_claiming(4, 4)), Err(Refused::Unreadable));
    }

    #[test]
    fn gif_and_webp_headers_are_read_too() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&200u16.to_le_bytes());
        gif.extend_from_slice(&100u16.to_le_bytes());
        gif.extend_from_slice(&[0, 0, 0]);
        assert_eq!(check("image/gif", &gif), Ok(20_000));
        gif[6..8].copy_from_slice(&u16::MAX.to_le_bytes());
        gif[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(matches!(check("image/gif", &gif), Err(Refused::TooLarge(65535, 65535))));

        // A lossless WebP: signature byte, then 14 bits of width - 1 and of height - 1.
        let webp = |w: u32, h: u32| {
            let mut v = b"RIFF\x1a\x00\x00\x00WEBPVP8L\x0d\x00\x00\x00\x2f".to_vec();
            let bits = (w - 1) | (h - 1) << 14;
            v.extend_from_slice(&bits.to_le_bytes());
            v.extend_from_slice(&[0; 8]);
            v
        };
        assert_eq!(check("image/webp", &webp(64, 32)), Ok(2048));
        assert!(matches!(check("image/webp", &webp(16384, 16384)), Err(Refused::TooLarge(..))));
    }

    #[test]
    fn a_data_url_is_decoded_as_usvg_decodes_it() {
        let png = png_claiming(8, 2);
        let href = data_url("image/png", &png);
        let (mime, data) = decode_data_url(&href).unwrap();
        assert_eq!((mime.as_str(), data), ("image/png", png));
        assert_eq!(check_href(&href), Ok(16));
        assert_eq!(
            check_href(&data_url("image/png", &png_claiming(40_000, 40_000))),
            Err(Refused::TooLarge(40_000, 40_000))
        );
        assert!(decode_data_url("https://example.com/a.png").is_none());
        assert_eq!(check_href("data:image/png;base64,@@@@"), Err(Refused::Unreadable));
    }

    #[test]
    fn a_refusal_says_what_was_removed() {
        assert!(Refused::TooLarge(60_000, 50_000).to_string().contains("60000 x 50000"));
        assert!(Refused::TooLarge(1, 1).to_string().contains("16 megapixels"));
        assert!(Refused::Unreadable.to_string().contains("PNG, JPEG, GIF and WebP"));
    }
}
