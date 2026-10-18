# Third-party notices

Parts of Kerf are adapted from [FilmCraft](https://github.com/storytold/filmcraft),
used under the MIT License below. The adapted code is listed here.

## Adapted from FilmCraft

- `crates/kerf-core/src/interchange/mod.rs` — from FilmCraft `crates/interchange/src/lib.rs` (the `Report` merge design), `crates/interchange/src/common.rs` (`path_to_file_url`) and `crates/time/src/lib.rs` (drop-frame timecode)
- `crates/kerf-core/src/interchange/edl.rs` — from FilmCraft `crates/interchange/src/edl.rs`
- `crates/kerf-core/src/interchange/otio.rs` — from FilmCraft `crates/interchange/src/otio.rs`
- `crates/kerf-core/src/interchange/markers.rs` — from FilmCraft `crates/engine/src/marker_export.rs` (CSV formula escaping)
- `crates/kerf-app/src/mcp/args.rs` — from FilmCraft `crates/automation/src/server.rs` (the strict-argument check)
- `crates/kerf-app/src/mcp.rs` (`every_tool_is_titled_and_annotated`) — from FilmCraft `crates/automation/src/server.rs` (`tools_are_titled_and_annotated`)
- `crates/kerf-core/src/engine/vad.rs` — from FilmCraft `crates/speech/src/vad.rs`
- `crates/kerf-core/src/engine/diarize.rs` — from FilmCraft `crates/speech/src/diarize.rs`
- `crates/kerf-core/src/engine/mel.rs` — from FilmCraft `crates/speech/src/mel.rs`
- `crates/kerf-core/src/project/store.rs` (`vacuum_into_atomic` and its tests) — from FilmCraft `crates/format/src/atomic.rs`
- `crates/kerf-core/src/timecode.rs` — from FilmCraft `crates/time/src/lib.rs` (`parse_timecode`, `frames_to_fields`, `fields_to_frames`)
- `frontend/src/lib/timecode.ts` (drop-frame fields and timecode parsing) — from FilmCraft `crates/time/src/lib.rs`
- `frontend/src/lib/keymap.ts` (`KEY_PRESETS`) — from FilmCraft `crates/engine/src/shortcut_presets.rs`
- `crates/kerf-core/src/engine/audio_sync.rs` — from FilmCraft `crates/audio-dsp/src/sync.rs`

## FilmCraft license

```text
MIT License

Copyright (c) 2026 ArtCraft Team and the FilmCraft contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
