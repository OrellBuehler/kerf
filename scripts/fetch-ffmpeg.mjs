#!/usr/bin/env bun
// Fetch `ffmpeg` + `ffprobe` for a Rust target triple into
// `crates/kerf-app/binaries/`, named with the Tauri sidecar `-<triple>` suffix
// (and `.exe` on Windows). The desktop app bundles these as `externalBin`
// sidecars so installs ship a known-good FFmpeg without one on the user's PATH.
//
// Usage:  bun scripts/fetch-ffmpeg.mjs [<target-triple>]
// The triple defaults to the host (parsed from `rustc -vV`).
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

// One or more archives per target; each contributes some of {ffmpeg, ffprobe}.
const BTBN = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest";
const SOURCES = {
  "x86_64-pc-windows-msvc": {
    ext: ".exe",
    archives: [{ url: `${BTBN}/ffmpeg-master-latest-win64-gpl.zip`, wants: ["ffmpeg.exe", "ffprobe.exe"] }],
  },
  "x86_64-unknown-linux-gnu": {
    ext: "",
    archives: [{ url: `${BTBN}/ffmpeg-master-latest-linux64-gpl.tar.xz`, wants: ["ffmpeg", "ffprobe"] }],
  },
  "x86_64-apple-darwin": {
    ext: "",
    archives: [
      { url: "https://evermeet.cx/ffmpeg/getrelease/ffmpeg/zip", wants: ["ffmpeg"] },
      { url: "https://evermeet.cx/ffmpeg/getrelease/ffprobe/zip", wants: ["ffprobe"] },
    ],
  },
};

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
  for (const { url, wants } of source.archives) {
    console.log(`↓ ${url}`);
    const res = await fetch(url);
    if (!res.ok) throw new Error(`download failed (${res.status}) for ${url}`);
    const archive = join(work, url.split("/").pop().replace(/[^\w.-]/g, "_") || "archive");
    await Bun.write(archive, await res.arrayBuffer());
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
