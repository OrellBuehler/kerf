# FFmpeg

This distribution of Kerf bundles **unmodified** FFmpeg executables
(`ffmpeg`, `ffprobe`) so the app can probe and process media without a
separately installed FFmpeg. They are obtained at build time from the
[BtbN FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds/releases) project.

These FFmpeg builds are licensed under the **GNU General Public License**.
The full, authoritative license text shipped with this build is in
`FFmpeg-LICENSE.txt` alongside this notice.

Kerf invokes these binaries as **separate external programs** (it does not
link against FFmpeg's libraries). Kerf's own source remains licensed under
PolyForm Noncommercial 1.0.0; the GPL applies to the bundled FFmpeg binaries.

## Corresponding source

The complete corresponding source for the bundled FFmpeg is published by the
FFmpeg project at <https://git.ffmpeg.org/ffmpeg.git> and
<https://github.com/FFmpeg/FFmpeg>. The exact build configuration used to
produce these binaries is published at
<https://github.com/BtbN/FFmpeg-Builds>.

For a copy of the corresponding source on physical media, contact
<me@orellbuehler.ch>. This written offer is valid for three years from the
date you received this distribution.

FFmpeg is a trademark of Fabrice Bellard, originator of the FFmpeg project.
