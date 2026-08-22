#!/usr/bin/env bun
// Fetch `ffmpeg` + `ffprobe` for a Rust target triple into
// `crates/kerf-app/binaries/`, named with the Tauri sidecar `-<triple>` suffix
// (and `.exe` on Windows). The desktop app bundles these as `externalBin`
// sidecars so installs ship a known-good FFmpeg without one on the user's PATH.
//
// Usage:  bun scripts/fetch-ffmpeg.mjs [<target-triple>]
//         bun scripts/fetch-ffmpeg.mjs --print-hashes   (after bumping a pin)
// The triple defaults to the host (parsed from `rustc -vV`).
//
// Every archive is PINNED to an immutable upstream release and verified against
// a SHA-256 recorded here. These binaries are bundled into an installer that is
// code-signed and auto-installed by every user, so "whatever upstream published
// most recently" is not good enough: an unverified download would inherit the
// signature's trust. A mismatch aborts rather than shipping.
//
// To bump: change the version constants below, run `--print-hashes`, and paste
// the new digests in. Verify the run afterwards with the engine tests, which
// exercise the real filter graphs against the binary:
//   KERF_FFMPEG=<path> KERF_FFPROBE=<path> \
//     cargo test -p kerf-core --no-default-features -- --ignored --skip downloads_a_real_model
//
// FFmpeg is licensed separately (the Windows/Linux builds below are GPL); shipping
// them carries that license's obligations — see the FFmpeg project for details.

import { $ } from "bun";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { mkdir, mkdtemp, rm, chmod, readdir } from "node:fs/promises";
import { tmpdir } from "node:os";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const outDir = join(repoRoot, "crates", "kerf-app", "binaries");
const licenseDir = join(repoRoot, "crates", "kerf-app", "licenses");
// Bundled FFmpeg is GPL, so ship its license text next to the app. The upstream
// archive carries the authoritative copy matching this exact build.
const LICENSE_NAMES = ["LICENSE.txt", "LICENSE", "COPYING.txt", "COPYING"];

function hostTriple() {
  const { stdout } = Bun.spawnSync(["rustc", "-vV"]);
  const m = stdout.toString().match(/^host:\s*(.+)$/m);
  if (!m) throw new Error("could not determine host target triple from `rustc -vV`");
  return m[1].trim();
}

// FFmpeg 9.0 across all three platforms. BtbN's dated `autobuild-*` tags are
// immutable, unlike the rolling `latest` tag; the `-gpl-9.0` assets track the
// 9.0 release branch rather than a master snapshot. Note these builds are
// configured `--disable-whisper`, so the bundled binary has no `whisper` filter
// — transcription on a bundled platform needs the `whisper` cargo feature.
const BTBN_TAG = "autobuild-2026-08-21-13-40";
const BTBN_BUILD = "n9.0.1-6-g9d4ca21220";
const BTBN_BRANCH = "9.0";
const BTBN = `https://github.com/BtbN/FFmpeg-Builds/releases/download/${BTBN_TAG}`;
// evermeet.cx serves per-version URLs alongside its rolling `getrelease` ones.
const EVERMEET = "9.0.1";

// One or more archives per target; each contributes some of {ffmpeg, ffprobe}.
const SOURCES = {
  "x86_64-pc-windows-msvc": {
    ext: ".exe",
    archives: [
      {
        url: `${BTBN}/ffmpeg-${BTBN_BUILD}-win64-gpl-${BTBN_BRANCH}.zip`,
        sha256: "6c0a3c1256cba57c62a3bb012c1e8f5e794d38a16c6509d05349237d2b66340f",
        wants: ["ffmpeg.exe", "ffprobe.exe"],
      },
    ],
  },
  "x86_64-unknown-linux-gnu": {
    ext: "",
    archives: [
      {
        url: `${BTBN}/ffmpeg-${BTBN_BUILD}-linux64-gpl-${BTBN_BRANCH}.tar.xz`,
        sha256: "da7c861c44cc6f92fff7f3f6aefb47690e3e88702826d06fbf9ac592a5f24083",
        wants: ["ffmpeg", "ffprobe"],
      },
    ],
  },
  "x86_64-apple-darwin": {
    ext: "",
    archives: [
      {
        url: `https://evermeet.cx/ffmpeg/ffmpeg-${EVERMEET}.zip`,
        sha256: "8a8c9e549983409fe6604b9aa665648b7a5def9407fe814c39c8b2ea7f64a48f",
        wants: ["ffmpeg"],
      },
      {
        url: `https://evermeet.cx/ffmpeg/ffprobe-${EVERMEET}.zip`,
        sha256: "d13f35db03456b7f65b7edb6437c86e23810fbfe91795e571f5b77211343b4f1",
        wants: ["ffprobe"],
      },
    ],
  },
};

async function download(url) {
  console.log(`\u2193 ${url}`);
  const res = await fetch(url);
  if (!res.ok) throw new Error(`download failed (${res.status}) for ${url}`);
  const bytes = new Uint8Array(await res.arrayBuffer());
  const digest = new Bun.CryptoHasher("sha256").update(bytes).digest("hex");
  return { bytes, digest };
}

// `--print-hashes` fetches every pinned archive and prints its digest, so
// bumping a version is paste-in rather than a hash computed by hand (or skipped).
if (process.argv.includes("--print-hashes")) {
  for (const [triple, source] of Object.entries(SOURCES)) {
    for (const { url } of source.archives) {
      const { digest } = await download(url);
      console.log(`  ${triple}\n    ${url}\n    sha256: "${digest}",`);
    }
  }
  process.exit(0);
}

async function findFile(root, base) {
  for (const e of await readdir(root, { withFileTypes: true })) {
    const full = join(root, e.name);
    if (e.isDirectory()) {
      const hit = await findFile(full, base);
      if (hit) return hit;
    } else if (e.name === base) {
      return full;
    }
  }
  return null;
}

const triple = (process.argv[2] || hostTriple()).trim();
const source = SOURCES[triple];
if (!source) {
  console.error(`No FFmpeg source configured for target '${triple}'.`);
  console.error(`Known targets: ${Object.keys(SOURCES).join(", ")}`);
  process.exit(1);
}

await mkdir(outDir, { recursive: true });
await mkdir(licenseDir, { recursive: true });
const work = await mkdtemp(join(tmpdir(), "kerf-ffmpeg-"));
let licenseWritten = false;
try {
  for (const { url, wants, sha256 } of source.archives) {
    const { bytes, digest } = await download(url);
    // Verified before anything unpacks it — a tampered archive is never handed
    // to `tar`, let alone bundled into a signed installer.
    if (digest !== sha256) {
      throw new Error(
        `checksum mismatch for ${url}\n  expected ${sha256}\n  got      ${digest}\n` +
          "If upstream legitimately republished, re-pin with --print-hashes.",
      );
    }
    console.log(`✓ sha256 ${digest}`);
    const archive = join(work, url.split("/").pop().replace(/[^\w.-]/g, "_") || "archive");
    await Bun.write(archive, bytes);
    // bsdtar (Windows/macOS) extracts .zip; GNU tar (Linux) handles .tar.xz.
    await $`tar -xf ${archive} -C ${work}`.quiet();

    for (const member of wants) {
      const src = await findFile(work, member);
      if (!src) throw new Error(`'${member}' not found inside ${url}`);
      const name = member.replace(/\.exe$/, "");
      const dest = join(outDir, `${name}-${triple}${source.ext}`);
      await Bun.write(dest, Bun.file(src));
      if (source.ext === "") await chmod(dest, 0o755);
      console.log(`✓ ${dest}`);
    }

    if (!licenseWritten) {
      for (const lic of LICENSE_NAMES) {
        const src = await findFile(work, lic);
        if (src) {
          const dest = join(licenseDir, "FFmpeg-LICENSE.txt");
          await Bun.write(dest, Bun.file(src));
          console.log(`✓ ${dest}`);
          licenseWritten = true;
          break;
        }
      }
    }
  }
  if (!licenseWritten) {
    console.warn("⚠ no LICENSE file found in the archive(s); ship FFmpeg's license manually.");
  }
} finally {
  await rm(work, { recursive: true, force: true });
}
