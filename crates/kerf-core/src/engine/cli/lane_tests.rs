//! Which slot a job takes: the hardware's only when it is hardware from end to end (pure).

use super::*;
use crate::engine::cpu::Resource;
use crate::engine::decode_policy::{Chroma, Codec, InputFacts};

fn video(codec: Codec, chroma: Chroma) -> InputFacts {
    InputFacts {
        codec,
        chroma,
        bits: (chroma == Chroma::Yuv420).then_some(8),
        ..InputFacts::default()
    }
}

fn deep(codec: Codec, bits: Option<u8>) -> InputFacts {
    InputFacts {
        codec,
        chroma: Chroma::Yuv420,
        bits,
        ..InputFacts::default()
    }
}

fn h264_420() -> InputFacts {
    video(Codec::H264, Chroma::Yuv420)
}

// ---- proxies ---------------------------------------------------------------------------------

#[test]
fn a_proxy_that_is_hardware_from_end_to_end_takes_the_hardware_slot() {
    assert_eq!(
        proxy_resource(Some("h264_amf"), Some("d3d11va"), false, h264_420(), true),
        Resource::Gpu
    );
    assert_eq!(
        proxy_resource(
            Some("h264_nvenc"),
            Some("cuda"),
            false,
            video(Codec::Hevc, Chroma::Yuv420),
            true
        ),
        Resource::Gpu,
        "a named accelerator, whatever it is called"
    );
}

#[test]
fn every_doubt_is_the_cores() {
    let gpu = |enc, dec, tone, facts: InputFacts, lane| proxy_resource(enc, dec, tone, facts, lane);
    // The lane is off.
    assert_eq!(
        gpu(Some("h264_amf"), Some("d3d11va"), false, h264_420(), false),
        Resource::Cpu
    );
    // A software encoder.
    assert_eq!(gpu(None, Some("d3d11va"), false, h264_420(), true), Resource::Cpu);
    // No decoder asked for, or `auto`, which falls back to software without a word.
    assert_eq!(gpu(Some("h264_amf"), None, false, h264_420(), true), Resource::Cpu);
    assert_eq!(gpu(Some("h264_amf"), Some("auto"), false, h264_420(), true), Resource::Cpu);
    assert_eq!(gpu(Some("h264_amf"), Some(" AUTO "), false, h264_420(), true), Resource::Cpu);
    assert_eq!(gpu(Some("h264_amf"), Some(""), false, h264_420(), true), Resource::Cpu);
    // A tone-map is `zscale` on the cores.
    assert_eq!(gpu(Some("h264_amf"), Some("d3d11va"), true, h264_420(), true), Resource::Cpu);
    // What no hardware decoder is known to decode, or nobody has said what it is.
    for facts in [
        video(Codec::H264, Chroma::Other),
        video(Codec::Hevc, Chroma::Other),
        video(Codec::Other, Chroma::Yuv420),
        video(Codec::Unknown, Chroma::Yuv420),
        video(Codec::H264, Chroma::Unknown),
        InputFacts::default(),
    ] {
        assert_eq!(
            gpu(Some("h264_amf"), Some("d3d11va"), false, facts, true),
            Resource::Cpu,
            "{facts:?}"
        );
    }
}

#[test]
fn ten_bit_h264_has_no_hardware_decoder_and_ten_bit_hevc_has() {
    let dec = |facts: InputFacts| proxy_resource(Some("h264_amf"), Some("d3d11va"), false, facts, true);
    assert_eq!(dec(deep(Codec::H264, Some(8))), Resource::Gpu);
    assert_eq!(
        dec(deep(Codec::H264, Some(10))),
        Resource::Cpu,
        "High10 is decoded in software"
    );
    assert_eq!(
        dec(deep(Codec::H264, None)),
        Resource::Cpu,
        "a depth nobody recorded is not assumed to be 8"
    );
    assert_eq!(dec(deep(Codec::Hevc, Some(10))), Resource::Gpu, "Main10");
    assert_eq!(dec(deep(Codec::Hevc, Some(8))), Resource::Gpu);
    assert_eq!(dec(deep(Codec::Hevc, None)), Resource::Gpu);
}

#[test]
fn an_export_of_ten_bit_h264_is_the_cores_and_of_ten_bit_hevc_the_hardware() {
    let a = export("h264_amf", "-hwaccel d3d11va -i /a.mp4", PLAIN_GRAPH);
    let of = |facts: InputFacts| export_resource(&a, &by_path(vec![("/a.mp4", facts)]), true);
    assert_eq!(of(deep(Codec::H264, Some(8))), Resource::Gpu);
    assert_eq!(of(deep(Codec::H264, Some(10))), Resource::Cpu);
    assert_eq!(of(deep(Codec::H264, None)), Resource::Cpu);
    assert_eq!(of(deep(Codec::Hevc, Some(10))), Resource::Gpu);
}

#[test]
fn the_depth_of_a_4_2_0_name_is_read_from_it() {
    use crate::engine::decode_policy::pix_fmt_bits_420;
    for (name, bits) in [
        ("yuv420p", 8),
        ("yuvj420p", 8),
        ("nv12", 8),
        ("yuv420p10le", 10),
        ("yuv420p10be", 10),
        ("yuv420p9le", 9),
        ("p010le", 10),
    ] {
        assert_eq!(pix_fmt_bits_420(name), Some(bits), "{name}");
    }
    for name in [
        "yuv422p10le",
        "yuv444p",
        "yuv420p12le",
        "yuva420p",
        "rgb24",
        "gray",
        "something_new",
        "",
    ] {
        assert_eq!(pix_fmt_bits_420(name), None, "{name}");
    }
}

#[test]
fn the_asset_lists_depth_reaches_the_facts() {
    let mut asset = crate::engine::test_support::test_asset(vec![crate::engine::test_support::video_stream(1920, 1080, 30.0)]);
    asset.path = "/a.mp4".to_string();
    for (pix_fmt, codec, expected) in [
        ("yuv420p", "h264", Some(8)),
        ("yuv420p10le", "h264", Some(10)),
        ("yuv420p10le", "hevc", Some(10)),
        ("yuv422p10le", "hevc", None),
    ] {
        let video = asset.streams.iter_mut().find(|s| s.kind == StreamKind::Video).unwrap();
        video.pix_fmt = Some(pix_fmt.to_string());
        video.codec = codec.to_string();
        let facts = crate::engine::decode_policy::facts_from_assets(std::slice::from_ref(&asset))("/a.mp4");
        assert_eq!(facts.bits, expected, "{codec} {pix_fmt}");
    }
}

// ---- exports ---------------------------------------------------------------------------------

fn args(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

const PLAIN_GRAPH: &str = "color=c=black:s=1920x1080:r=30:d=8,format=yuv420p[vbase];[0:v]trim=start=0:end=8,setpts=PTS-STARTPTS+0/TB,scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,format=yuv420p[v0];[vbase][v0]overlay=eof_action=pass:enable='between(t,0,8)'[outv]";

fn export(codec: &str, inputs: &str, graph: &str) -> Vec<String> {
    let mut a = args("-y -hide_banner -nostats");
    a.extend(args(inputs));
    a.extend(["-filter_complex".to_string(), graph.to_string()]);
    a.extend(args(&format!("-map [outv] -c:v {codec} -pix_fmt yuv420p out.mp4")));
    a
}

fn by_path(path_facts: Vec<(&'static str, InputFacts)>) -> impl Fn(&str) -> InputFacts {
    move |p| {
        path_facts
            .iter()
            .find(|(name, _)| *name == p)
            .map_or(InputFacts::default(), |(_, f)| *f)
    }
}

#[test]
fn an_export_that_is_hardware_from_end_to_end_takes_the_hardware_slot() {
    let a = export("h264_amf", "-hwaccel d3d11va -ss 1 -i /a.mp4", PLAIN_GRAPH);
    let facts = by_path(vec![("/a.mp4", h264_420())]);
    assert_eq!(export_resource(&a, &facts, true), Resource::Gpu);
    // Sound through the filters sound uses.
    let with_sound = format!("{PLAIN_GRAPH};[0:a]atrim=start=0:end=8,asetpts=PTS-STARTPTS,volume=1,aformat=sample_rates=48000:channel_layouts=stereo,adelay=0:all=1[a0];[a0]amix=inputs=1:normalize=0,alimiter=limit=0.84:attack=5:release=100:level=0[outa]");
    let a = export("hevc_nvenc", "-hwaccel cuda -i /a.mp4", &with_sound);
    assert_eq!(export_resource(&a, &facts, true), Resource::Gpu);
}

#[test]
fn two_inputs_each_handed_a_decoder_and_a_picture_over_them_are_hardware_too() {
    let a = export(
        "h264_amf",
        "-hwaccel d3d11va -i /a.mp4 -hwaccel d3d11va -i /b.mp4 -loop 1 -framerate 30 -t 4 -i /title.png",
        PLAIN_GRAPH,
    );
    let image = InputFacts {
        image: true,
        ..InputFacts::default()
    };
    let facts = by_path(vec![
        ("/a.mp4", h264_420()),
        ("/b.mp4", video(Codec::Hevc, Chroma::Yuv420)),
        ("/title.png", image),
    ]);
    assert_eq!(export_resource(&a, &facts, true), Resource::Gpu);
}

#[test]
fn an_export_with_any_doubt_is_the_cores() {
    let facts = by_path(vec![
        ("/a.mp4", h264_420()),
        ("/r5.mp4", video(Codec::Hevc, Chroma::Other)),
        ("/music.wav", InputFacts::default()),
    ]);
    let ok = export("h264_amf", "-hwaccel d3d11va -i /a.mp4", PLAIN_GRAPH);
    assert_eq!(export_resource(&ok, &facts, true), Resource::Gpu, "the control");
    // The lane is off.
    assert_eq!(export_resource(&ok, &facts, false), Resource::Cpu);
    // A software encoder.
    assert_eq!(
        export_resource(&export("libx264", "-hwaccel d3d11va -i /a.mp4", PLAIN_GRAPH), &facts, true),
        Resource::Cpu
    );
    // No hardware decode: the rerun after a failure, `none`, or a bare input.
    assert_eq!(
        export_resource(&export("h264_amf", "-i /a.mp4", PLAIN_GRAPH), &facts, true),
        Resource::Cpu
    );
    assert_eq!(
        export_resource(&export("h264_amf", "-hwaccel none -i /a.mp4", PLAIN_GRAPH), &facts, true),
        Resource::Cpu
    );
    // `auto` may be software.
    assert_eq!(
        export_resource(&export("h264_amf", "-hwaccel auto -i /a.mp4", PLAIN_GRAPH), &facts, true),
        Resource::Cpu
    );
    // A source no hardware decoder takes (4:2:2), a file nobody knows, an audio file.
    for input in ["-hwaccel d3d11va -i /r5.mp4", "-hwaccel d3d11va -i /unknown.mp4"] {
        assert_eq!(
            export_resource(&export("h264_amf", input, PLAIN_GRAPH), &facts, true),
            Resource::Cpu,
            "{input}"
        );
    }
    assert_eq!(
        export_resource(
            &export("h264_amf", "-hwaccel d3d11va -i /a.mp4 -i /music.wav", PLAIN_GRAPH),
            &facts,
            true
        ),
        Resource::Cpu,
        "one input that is not hardware makes the run not"
    );
    // One of two inputs without a decoder, though the other has one.
    assert_eq!(
        export_resource(
            &export("h264_amf", "-hwaccel d3d11va -i /a.mp4 -i /a.mp4", PLAIN_GRAPH),
            &facts,
            true
        ),
        Resource::Cpu,
        "options belong to the input they precede"
    );
    // No graph, no inputs, no encoder.
    assert_eq!(export_resource(&args("-i /a.mp4 out.mp4"), &facts, true), Resource::Cpu);
    assert_eq!(export_resource(&[], &facts, true), Resource::Cpu);
}

#[test]
fn a_graph_with_work_for_the_cores_in_it_is_the_cores() {
    let facts = by_path(vec![("/a.mp4", h264_420())]);
    let with = |filter: &str| {
        let graph = PLAIN_GRAPH.replace("setsar=1", &format!("setsar=1,{filter}"));
        export_resource(&export("h264_amf", "-hwaccel d3d11va -i /a.mp4", &graph), &facts, true)
    };
    assert_eq!(with("null"), Resource::Gpu, "the control: a filter that is allowed");
    for heavy in [
        "zscale=t=linear:npl=100,tonemap=mobius,zscale=t=bt709",
        "geq=a='if(lt(X,10),0,255)'",
        "rotate=0.1:fillcolor=none",
        "v360=e:flat:yaw=0",
        "drawtext=text=hi:fontsize=20",
        "gblur=sigma=4",
        "eq=brightness=0.1",
        "chromakey=green",
        "colorchannelmixer=aa=0.5",
        "unsharp=5:5:1",
        "hue=s=0",
        "vignette",
        "negate",
        "xfade=transition=fade:duration=1",
        "sendcmd=f=cmds.txt",
        "somethingnew=x=1",
    ] {
        assert_eq!(with(heavy), Resource::Cpu, "{heavy}");
    }
}

#[test]
fn filter_names_are_read_past_labels_and_quoted_expressions() {
    assert_eq!(
        filter_names(PLAIN_GRAPH),
        ["color", "format", "trim", "setpts", "scale", "pad", "setsar", "fps", "format", "overlay"]
    );
    // Commas and semicolons inside quotes belong to the expression.
    assert_eq!(
        filter_names("[0:v]scale=w='iw*(if(lt(t,3),1,2))':h=-2,geq=a='if(lt(X,1),0,1)'[x];[x]null[outv]"),
        ["scale", "geq", "null"]
    );
    // An escaped comma and a quote that is escaped do not open or end anything.
    assert_eq!(filter_names(r"[0:v]drawtext=text=a\,b,null[o]"), ["drawtext", "null"]);
    assert_eq!(filter_names(r"[0:v]drawtext=text=it\'s:x=1,null[o]"), ["drawtext", "null"]);
    // Several labels in front, spaces, and nothing at all.
    assert_eq!(filter_names("[a][b] overlay=x=1 [o]"), ["overlay"]);
    assert!(filter_names("").is_empty());
    assert!(filter_names("[a][b]").is_empty());
    assert_eq!(filter_names("a;b,c"), ["a", "b", "c"]);
}

// ---- the argv the real builders write ---------------------------------------------------------

mod built {
    use super::*;
    use crate::engine::test_support::{make_clip, single, test_asset, video_stream};
    use crate::model::{Asset, Timeline, VideoEffect};

    fn h264_asset() -> Asset {
        let mut stream = video_stream(3840, 2160, 30.0);
        stream.pix_fmt = Some("yuv420p".into());
        let mut asset = test_asset(vec![stream]);
        asset.path = "/media/a.mp4".into();
        asset
    }

    /// The tuned argv of an export of `timeline`, the way `render_attempt` hands it to be judged.
    fn tuned(timeline: &Timeline, assets: &[Asset], codec: &str, os: HostOs) -> Vec<String> {
        let opts = ExportOptions {
            video_codec: Some(codec.to_string()),
            hwaccel: Some("auto".to_string()),
            resolution: Some((1920, 1080)),
            ..ExportOptions::default()
        };
        let mut args = build_export_args(timeline, assets, "out.mp4", &opts).unwrap();
        tune_export_args(&mut args, assets, os);
        args
    }

    #[test]
    fn a_plain_cut_of_h264_to_a_hardware_encoder_is_hardware_on_windows_and_not_elsewhere() {
        let a = h264_asset();
        let timeline = single(vec![make_clip(a.id, 0.0, 8.0, 0.0)]);
        let assets = [a];
        let facts = facts_from_assets(&assets);
        // `auto` becomes `d3d11va` on Windows: named, so it fails where there is no device.
        let windows = tuned(&timeline, &assets, "h264_amf", HostOs::Windows);
        assert!(windows.join(" ").contains("-hwaccel d3d11va"), "{windows:?}");
        assert_eq!(export_resource(&windows, &facts, true), Resource::Gpu);
        // On Linux it stays `auto`, which may be software: the cores.
        let linux = tuned(&timeline, &assets, "h264_nvenc", HostOs::Other);
        assert_eq!(export_resource(&linux, &facts, true), Resource::Cpu);
        // And a software encoder is the cores' anywhere.
        let soft = tuned(&timeline, &assets, "libx264", HostOs::Windows);
        assert_eq!(export_resource(&soft, &facts, true), Resource::Cpu);
    }

    #[test]
    fn a_blurred_or_tone_mapped_or_titled_cut_is_the_cores_whatever_encodes_it() {
        let a = h264_asset();
        let mut blurred = make_clip(a.id, 0.0, 8.0, 0.0);
        blurred.effects.push(VideoEffect::Blur { sigma: 3.0 });
        let assets = [a.clone()];
        let facts = facts_from_assets(&assets);
        let args = tuned(&single(vec![blurred]), &assets, "h264_amf", HostOs::Windows);
        assert_eq!(export_resource(&args, &facts, true), Resource::Cpu, "gblur");

        let mut hdr = a.clone();
        hdr.streams[0].color_transfer = Some("arib-std-b67".into());
        let assets = [hdr.clone()];
        let args = tuned(
            &single(vec![make_clip(hdr.id, 0.0, 8.0, 0.0)]),
            &assets,
            "h264_amf",
            HostOs::Windows,
        );
        assert_eq!(
            export_resource(&args, &facts_from_assets(&assets), true),
            Resource::Cpu,
            "zscale"
        );

        let mut titled = single(vec![make_clip(a.id, 0.0, 8.0, 0.0)]);
        titled.overlays.push(crate::model::TextOverlay::new("hello", 1.0, 3.0));
        let assets = [a];
        let args = tuned(&titled, &assets, "h264_amf", HostOs::Windows);
        assert_eq!(
            export_resource(&args, &facts_from_assets(&assets), true),
            Resource::Cpu,
            "drawtext"
        );
    }

    #[test]
    fn the_rerun_after_a_hardware_failure_asks_for_the_cores() {
        let a = h264_asset();
        let timeline = single(vec![make_clip(a.id, 0.0, 8.0, 0.0)]);
        let assets = [a];
        let opts = ExportOptions {
            video_codec: Some("h264_amf".to_string()),
            hwaccel: None,
            resolution: Some((1920, 1080)),
            ..ExportOptions::default()
        };
        let mut args = build_export_args(&timeline, &assets, "out.mp4", &opts).unwrap();
        tune_export_args(&mut args, &assets, HostOs::Windows);
        assert_eq!(
            export_resource(&args, &facts_from_assets(&assets), true),
            Resource::Cpu,
            "no input is handed a decoder, so it is a software decode however it encodes"
        );
    }
}
