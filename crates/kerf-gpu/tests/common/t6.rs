//! The photographs of the T6 checker (a still on a Ken Burns move), made once by name in `target/tmp/t6-photos` and probed as an import
//! does: shared by the Still and the export-frame suites. `python3` with Pillow and ImageMagick's `convert` make the files FFmpeg's
//! encoders do not (progressive and CMYK JPEG, EXIF, PNG ancillary chunks); a file the tool cannot make is left out, and says so.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use kerf_core::{Asset, Project};

fn ffmpeg(args: &[&str]) {
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn probed(path: &Path) -> Asset {
    Project::probe_asset(path).unwrap_or_else(|e| panic!("probe {}: {e}", path.display()))
}

/// The directory of this test binary's photographs (a binary of its own, so that two suites that run side by side do not write one file).
pub fn dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("t6-photos-{}", env!("CARGO_CRATE_NAME")))
}

pub fn t2(w: u32, h: u32) -> String {
    format!("testsrc2=size={w}x{h}:rate=1:duration=1")
}

/// A gradient with grain over it: smooth where a codec keeps it and busy where a scaler shows its taps.
pub fn grain(w: u32, h: u32) -> String {
    format!(
        "nullsrc=size={w}x{h}:rate=1:duration=1,format=rgb24,geq=r='clip((X*255/W+Y*255/H)/2+random(1)*70-35,0,255)':g='clip(255-X*255/W+random(2)*60-30,0,255)':b='clip(Y*255/H+random(3)*90-45,0,255)'"
    )
}

pub fn content(w: u32, h: u32) -> String {
    if u64::from(w) * u64::from(h) <= 2_200_000 {
        grain(w, h)
    } else {
        t2(w, h)
    }
}

/// A photograph made once (by name) in the checker's own directory and probed as an import does.
pub fn photo(name: &str, lavfi: &str, args: &[&str]) -> Asset {
    static MADE: OnceLock<Mutex<BTreeMap<String, Asset>>> = OnceLock::new();
    let made = MADE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut made = made.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(a) = made.get(name) {
        return a.clone();
    }
    let dir = dir();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let mut full = vec!["-f", "lavfi", "-i", lavfi, "-frames:v", "1"];
    full.extend_from_slice(args);
    full.push(path.to_str().unwrap());
    ffmpeg(&full);
    let asset = probed(&path);
    made.insert(name.to_string(), asset.clone());
    asset
}

pub fn jpeg(w: u32, h: u32) -> Asset {
    photo(
        &format!("jpeg-{w}x{h}.jpg"),
        &content(w, h),
        &["-q:v", "3", "-pix_fmt", "yuvj420p"],
    )
}

pub fn png(w: u32, h: u32) -> Asset {
    photo(&format!("png-{w}x{h}.png"), &content(w, h), &["-pix_fmt", "rgb24"])
}

// ---- files of every make: the PNG and JPEG variants a camera, a phone and an editor write ----------------------

/// A file made once from the checker's base picture by `python3 -I -c code base out` (Pillow); `None` when the tool cannot make it.
pub fn py(name: &str, code: &str) -> Option<Asset> {
    static MADE: OnceLock<Mutex<BTreeMap<String, Option<Asset>>>> = OnceLock::new();
    let made = MADE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut made = made.lock().unwrap();
    if let Some(a) = made.get(name) {
        return a.clone();
    }
    let base = png(1200, 800);
    let dir = dir();
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join(name);
    let run = Command::new("python3")
        .args(["-I", "-c", code, &base.path])
        .arg(&out)
        .stdin(Stdio::null())
        .output();
    let asset = match run {
        Ok(o) if o.status.success() && out.exists() => std::panic::catch_unwind(|| probed(&out)).ok(),
        Ok(o) => {
            eprintln!("T6 could not make {name}: {}", String::from_utf8_lossy(&o.stderr).trim());
            None
        }
        Err(e) => {
            eprintln!("T6 could not make {name}: {e}");
            None
        }
    };
    made.insert(name.to_string(), asset.clone());
    asset
}

/// A PNG of the base picture with `extra` (a Python expression for bytes: chunks) after its header.
pub fn png_with(name: &str, extra: &str) -> Option<Asset> {
    let code = format!(
        "import sys,struct,zlib\n\
         d=open(sys.argv[1],'rb').read()\n\
         def chunk(t,b): return struct.pack('>I',len(b))+t+b+struct.pack('>I',zlib.crc32(t+b)&0xffffffff)\n\
         open(sys.argv[2],'wb').write(d[:33]+({extra})+d[33:])\n"
    );
    py(name, &code)
}

pub fn exif(orientation: u8) -> String {
    format!(
        "b'MM\\x00*\\x00\\x00\\x00\\x08\\x00\\x01\\x01\\x12\\x00\\x03\\x00\\x00\\x00\\x01\\x00{}\\x00\\x00\\x00\\x00\\x00\\x00'",
        { format!("\\x{orientation:02x}") }
    )
}

pub fn jpeg_with(name: &str, save: &str) -> Option<Asset> {
    let code = format!("import sys\nfrom PIL import Image, ImageCms\nim=Image.open(sys.argv[1]).convert('RGB')\n{save}\n");
    py(name, &code)
}

/// The variants of a PNG file whose pixels are the base picture's: every colour chunk and ancillary chunk a writer adds, and the layouts
/// FFmpeg decodes to another pixel format.
pub fn png_variants() -> Vec<(String, Asset)> {
    let mut v: Vec<(String, Asset)> = Vec::new();
    let mut add = |name: &str, a: Option<Asset>| {
        if let Some(a) = a {
            v.push((name.to_string(), a));
        }
    };
    add("plain", Some(png(1200, 800)));
    add(
        "gAMA 1/2.2",
        png_with("png-gama.png", "chunk(b'gAMA',struct.pack('>I',45455))"),
    );
    add(
        "gAMA linear",
        png_with("png-gama-lin.png", "chunk(b'gAMA',struct.pack('>I',100000))"),
    );
    add("sRGB", png_with("png-srgb.png", "chunk(b'sRGB',b'\\x00')"));
    add(
        "cHRM BT.709",
        png_with(
            "png-chrm.png",
            "chunk(b'cHRM',struct.pack('>8I',31270,32900,64000,33000,30000,60000,15000,6000))",
        ),
    );
    add(
        "cICP BT.709 full",
        png_with("png-cicp-709.png", "chunk(b'cICP',bytes([1,13,0,1]))"),
    );
    add(
        "cICP BT.2020 PQ full",
        png_with("png-cicp-2020.png", "chunk(b'cICP',bytes([9,16,0,1]))"),
    );
    add(
        "cICP BT.709 tv-range flag",
        png_with("png-cicp-tv.png", "chunk(b'cICP',bytes([1,1,0,0]))"),
    );
    add(
        "cICP BT.2020 PQ tv-range flag",
        png_with("png-cicp-2020-tv.png", "chunk(b'cICP',bytes([9,16,0,0]))"),
    );
    add(
        "pHYs 1:2",
        png_with("png-phys.png", "chunk(b'pHYs',struct.pack('>IIB',2835,5670,1))"),
    );
    add("sBIT 5/6/5", png_with("png-sbit.png", "chunk(b'sBIT',bytes([5,6,5]))"));
    add(
        "tEXt and tIME",
        png_with(
            "png-text.png",
            "chunk(b'tEXt',b'Comment\\x00hello')+chunk(b'tIME',bytes([7,234,1,2,3,4,5]))",
        ),
    );
    add(
        "iCCP sRGB profile",
        py(
            "png-iccp.png",
            "import sys\nfrom PIL import Image, ImageCms\nim=Image.open(sys.argv[1]).convert('RGB')\nicc=ImageCms.ImageCmsProfile(ImageCms.createProfile('sRGB')).tobytes()\nim.save(sys.argv[2],icc_profile=icc)\n",
        ),
    );
    add(
        "iCCP AdobeRGB-like (cHRM + gAMA)",
        png_with(
            "png-wide.png",
            "chunk(b'gAMA',struct.pack('>I',45455))+chunk(b'cHRM',struct.pack('>8I',31270,32900,64000,33000,21000,71000,15000,6000))",
        ),
    );
    add(
        "Adam7 interlaced",
        py(
            "png-interlaced.png",
            "import sys,subprocess\nsubprocess.run(['convert',sys.argv[1],'-interlace','PNG',sys.argv[2]],check=True)\n",
        ),
    );
    for o in [2u8, 6, 8] {
        add(
            &format!("eXIf orientation {o}"),
            png_with(&format!("png-exif-{o}.png"), &format!("chunk(b'eXIf',{})", exif(o))),
        );
    }
    v
}

/// PNG layouts that decode to another pixel format than `rgb24`: refused (no chain was measured for them), or drawn exactly.
pub fn other_formats() -> Vec<(String, Asset)> {
    let mut v = Vec::new();
    for (name, file, pix) in [
        ("16-bit RGB png", "other-rgb48.png", "rgb48be"),
        ("palette png", "other-pal8.png", "pal8"),
        ("gray png", "other-gray.png", "gray"),
        ("gray+alpha png", "other-ya8.png", "ya8"),
        ("rgba png (opaque)", "other-rgba.png", "rgba"),
        ("gray 16-bit png", "other-gray16.png", "gray16be"),
    ] {
        v.push((name.to_string(), photo(file, &grain(1200, 800), &["-pix_fmt", pix])));
    }
    v.push((
        "tiff rgb24".to_string(),
        photo("other-rgb24.tiff", &grain(1200, 800), &["-pix_fmt", "rgb24"]),
    ));
    v.push((
        "bmp bgr24".to_string(),
        photo("other-bgr24.bmp", &grain(1200, 800), &["-pix_fmt", "bgr24"]),
    ));
    v.push((
        "webp (yuv420p)".to_string(),
        photo(
            "other-420.webp",
            &grain(1200, 800),
            &["-pix_fmt", "yuv420p", "-c:v", "libwebp", "-lossless", "0", "-q:v", "90"],
        ),
    ));
    v.push((
        "mjpeg 4:2:0 limited (yuv420p jpeg)".to_string(),
        photo(
            "other-yuv420-jpeg.jpg",
            &grain(1200, 800),
            &["-pix_fmt", "yuv420p", "-strict", "-1", "-q:v", "3"],
        ),
    ));
    for (name, file, pix) in [
        ("4:2:2 jpeg", "other-422.jpg", "yuvj422p"),
        ("4:4:4 jpeg", "other-444.jpg", "yuvj444p"),
    ] {
        v.push((
            name.to_string(),
            photo(file, &grain(1200, 800), &["-q:v", "3", "-pix_fmt", pix]),
        ));
    }
    v
}

/// The variants of a JPEG file: the writers' choices that the decoder reads differently.
pub fn jpeg_variants() -> Vec<(String, Asset)> {
    let mut v: Vec<(String, Asset)> = Vec::new();
    let mut add = |name: &str, a: Option<Asset>| {
        if let Some(a) = a {
            v.push((name.to_string(), a));
        }
    };
    add("plain 4:2:0", Some(jpeg(1200, 800)));
    add(
        "progressive 4:2:0",
        jpeg_with(
            "jpg-prog.jpg",
            "im.save(sys.argv[2],quality=88,progressive=True,subsampling=2)",
        ),
    );
    add(
        "progressive 4:4:4",
        jpeg_with(
            "jpg-prog444.jpg",
            "im.save(sys.argv[2],quality=88,progressive=True,subsampling=0)",
        ),
    );
    add(
        "optimised huffman",
        jpeg_with("jpg-opt.jpg", "im.save(sys.argv[2],quality=90,optimize=True,subsampling=2)"),
    );
    add(
        "quality 100",
        jpeg_with("jpg-q100.jpg", "im.save(sys.argv[2],quality=100,subsampling=2)"),
    );
    add(
        "quality 10",
        jpeg_with("jpg-q10.jpg", "im.save(sys.argv[2],quality=10,subsampling=2)"),
    );
    add(
        "4:4:4",
        jpeg_with("jpg-444.jpg", "im.save(sys.argv[2],quality=90,subsampling=0)"),
    );
    add(
        "4:2:2",
        jpeg_with("jpg-422.jpg", "im.save(sys.argv[2],quality=90,subsampling=1)"),
    );
    add(
        "4:4:0",
        py(
            "jpg-440.jpg",
            "import sys,subprocess\nsubprocess.run(['convert',sys.argv[1],'-sampling-factor','1x2','-quality','90',sys.argv[2]],check=True)\n",
        ),
    );
    add(
        "4:1:1",
        py(
            "jpg-411.jpg",
            "import sys,subprocess\nsubprocess.run(['convert',sys.argv[1],'-sampling-factor','4:1:1','-quality','90',sys.argv[2]],check=True)\n",
        ),
    );
    add(
        "restart intervals",
        py(
            "jpg-restart.jpg",
            "import sys,subprocess\nsubprocess.run(['convert',sys.argv[1],'-define','jpeg:restart-interval=2','-quality','90',sys.argv[2]],check=True)\n",
        ),
    );
    add(
        "grayscale",
        jpeg_with("jpg-gray.jpg", "im.convert('L').save(sys.argv[2],quality=90)"),
    );
    add(
        "CMYK",
        jpeg_with("jpg-cmyk.jpg", "im.convert('CMYK').save(sys.argv[2],quality=90)"),
    );
    add(
        "ICC profile",
        jpeg_with(
            "jpg-icc.jpg",
            "icc=ImageCms.ImageCmsProfile(ImageCms.createProfile('sRGB')).tobytes()\nim.save(sys.argv[2],quality=90,icc_profile=icc,subsampling=2)",
        ),
    );
    add(
        "JFIF 1:2 aspect",
        jpeg_with(
            "jpg-sar.jpg",
            "im.save(sys.argv[2],quality=90,subsampling=2,jfif_unit=0,jfif_density=(1,2))",
        ),
    );
    add(
        "JFIF 72 dpi square",
        jpeg_with("jpg-dpi.jpg", "im.save(sys.argv[2],quality=90,subsampling=2,dpi=(72,72))"),
    );
    for o in 1u8..=8 {
        add(
            &format!("EXIF orientation {o}"),
            jpeg_with(
                &format!("jpg-exif-{o}.jpg"),
                &format!("ex=Image.Exif()\nex[0x0112]={o}\nim.save(sys.argv[2],quality=90,subsampling=2,exif=ex)"),
            ),
        );
    }
    v
}
