#!/usr/bin/env bun
// Fetch `ffmpeg` + `ffprobe` for a Rust target triple into
// `crates/kerf-app/binaries/`, named with the Tauri sidecar `-<triple>` suffix
// (and `.exe` on Windows). The desktop app bundles these as `externalBin`
// sidecars so installs ship a known-good FFmpeg without one on the user's PATH.
//
// Usage:  bun scripts/fetch-ffmpeg.mjs [<target-triple>]
//         bun scripts/fetch-ffmpeg.mjs --print-hashes   (after bumping a pin)
//         bun scripts/fetch-ffmpeg.mjs --repin          (move every pin to the
//                                                        newest upstream build)
// The triple defaults to the host (parsed from `rustc -vV`).
//
// Every archive is PINNED to an immutable upstream release and verified against
// a SHA-256 recorded here. These binaries are bundled into an installer that is
// code-signed and auto-installed by every user, so "whatever upstream published
// most recently" is not good enough: an unverified download would inherit the
// signature's trust. A mismatch aborts rather than shipping.
//
// To bump: `--repin` finds the newest BtbN autobuild and evermeet release,
// downloads them, and rewrites the constants and digests below in place (it is
// what `prepare-release.yml` runs). By hand: change the version constants, run
// `--print-hashes`, and paste the new digests in. Verify the run afterwards with the engine tests, which
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
// 9.0 release branch rather than a master snapshot. They are also *pruned* after
// about two weeks, so a pin left alone eventually 404s the Windows release build
// (it did, for 0.21.0) — re-pin to a current tag with `--print-hashes` when it
// does, or before cutting a release that has been a while coming. Note these builds are
// configured `--disable-whisper`, so the bundled binary has no `whisper` filter
// — transcription on a bundled platform needs the `whisper` cargo feature.
const BTBN_TAG = "autobuild-2026-10-22-13-06";
const BTBN_BUILD = "n9.0.2-33-g4cf298e66a";
const BTBN_BRANCH = "9.0";
const BTBN = `https://github.com/BtbN/FFmpeg-Builds/releases/download/${BTBN_TAG}`;
// evermeet.cx serves per-version URLs alongside its rolling `getrelease` ones.
const EVERMEET = "9.0.2";

// One or more archives per target; each contributes some of {ffmpeg, ffprobe}.
const SOURCES = sources({ btbn: BTBN, build: BTBN_BUILD, branch: BTBN_BRANCH, evermeet: EVERMEET });

function sources({ btbn: BTBN, build: BTBN_BUILD, branch: BTBN_BRANCH, evermeet: EVERMEET }) {
  return {
    "x86_64-pc-windows-msvc": {
      ext: ".exe",
      archives: [
        {
          url: `${BTBN}/ffmpeg-${BTBN_BUILD}-win64-gpl-${BTBN_BRANCH}.zip`,
          sha256: "5a8898e31f2c452062d1f1be475befd8ffba04db11e9f8251da3d10d9a1ae2e5",
          wants: ["ffmpeg.exe", "ffprobe.exe"],
        },
      ],
    },
    "x86_64-unknown-linux-gnu": {
      ext: "",
      archives: [
        {
          url: `${BTBN}/ffmpeg-${BTBN_BUILD}-linux64-gpl-${BTBN_BRANCH}.tar.xz`,
          sha256: "1cb0a72cf393a1fdb661cbbbd0cfe89dc8b197bb21e21473bbde505bb4f2a2a7",
          wants: ["ffmpeg", "ffprobe"],
        },
      ],
    },
    "x86_64-apple-darwin": {
      ext: "",
      archives: [
        {
          url: `https://evermeet.cx/ffmpeg/ffmpeg-${EVERMEET}.zip`,
          sha256: "4acc0be580f9b2788029eb7bd4d645ff87968911b0a62aeeb3940d42d54558d5",
          wants: ["ffmpeg"],
        },
        {
          url: `https://evermeet.cx/ffmpeg/ffprobe-${EVERMEET}.zip`,
          sha256: "24a9c968cd4da72d99c7245e914b921815835eb6dff01d99868031aebaf1d439",
          wants: ["ffprobe"],
        },
      ],
    },
  };
}

// Mirrors rate-limit CI runners (evermeet answers 429 or refuses the connection
// when several jobs fetch at once), so a refused or throttled request is retried
// with backoff (about six and a half minutes in all), honouring Retry-After.
// The digest check still guards the bytes.
const ATTEMPTS = 8;

async function download(url) {
  for (let attempt = 1; ; attempt++) {
    console.log(`\u2193 ${url}${attempt > 1 ? ` (attempt ${attempt})` : ""}`);
    let res;
    try {
      res = await fetch(url);
    } catch (err) {
      if (attempt >= ATTEMPTS) throw err;
      await backoff(attempt, null, String(err));
      continue;
    }
    if (res.ok) {
      const bytes = new Uint8Array(await res.arrayBuffer());
      const digest = new Bun.CryptoHasher("sha256").update(bytes).digest("hex");
      return { bytes, digest };
    }
    const transient = res.status === 429 || res.status >= 500;
    if (!transient || attempt >= ATTEMPTS) throw new Error(`download failed (${res.status}) for ${url}`);
    await backoff(attempt, res.headers.get("retry-after"), `HTTP ${res.status}`);
  }
}

// Pins not already on GitHub are mirrored as assets of this repo's
// `ffmpeg-mirror` prerelease (`mirror-ffmpeg.yml`), named like a kept archive, and
// tried once before the upstream mirror. A missing or altered asset falls through.
const MIRROR = "https://github.com/OrellBuehler/kerf/releases/download/ffmpeg-mirror";

async function fromMirror(name, sha256) {
  try {
    const res = await fetch(`${MIRROR}/${name}`, { signal: AbortSignal.timeout(120_000) });
    if (!res.ok) return null;
    const bytes = new Uint8Array(await res.arrayBuffer());
    const digest = new Bun.CryptoHasher("sha256").update(bytes).digest("hex");
    if (digest !== sha256) return null;
    console.log(`\u2713 mirrored ${name}`);
    return { bytes, digest };
  } catch {
    return null;
  }
}

// `KERF_FFMPEG_ARCHIVES` names a directory that keeps verified archives between
// runs (CI caches it), so a pin already fetched is never asked of a mirror again.
// The digest is checked either way; a stale or damaged copy is fetched afresh.
async function fetchPinned(url, sha256) {
  const name = `${sha256}-${url.split("/").pop().replace(/[^\w.-]/g, "_")}`;
  const keep = process.env.KERF_FFMPEG_ARCHIVES;
  const kept = keep && join(keep, name);
  if (kept && (await Bun.file(kept).exists())) {
    const bytes = new Uint8Array(await Bun.file(kept).arrayBuffer());
    const digest = new Bun.CryptoHasher("sha256").update(bytes).digest("hex");
    if (digest === sha256) {
      console.log(`\u2713 kept ${url}`);
      return { bytes, digest };
    }
  }
  const got = (!url.startsWith("https://github.com/") && (await fromMirror(name, sha256))) || (await download(url));
  if (kept && got.digest === sha256) {
    await mkdir(keep, { recursive: true });
    await Bun.write(kept, got.bytes);
  }
  return got;
}

async function backoff(attempt, retryAfter, why) {
  const asked = Number(retryAfter);
  const ms = Number.isFinite(asked) && asked > 0 ? Math.min(asked, 120) * 1000 : Math.min(2 ** attempt * 2500, 120_000);
  console.log(`  ${why}; retrying in ${Math.round(ms / 1000)}s`);
  await Bun.sleep(ms);
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

async function json(url) {
  const headers = { accept: "application/json" };
  const token = process.env.GH_TOKEN || process.env.GITHUB_TOKEN;
  if (token && url.startsWith("https://api.github.com/")) headers.authorization = `Bearer ${token}`;
  const res = await fetch(url, { headers });
  if (!res.ok) throw new Error(`${url}: ${res.status}`);
  return res.json();
}

// `--repin` moves every pin to the newest upstream build on the same branch and
// rewrites this file. The digests are taken from what was just downloaded —
// the same trust-on-first-use as `--print-hashes`, minus the copy-paste.
if (process.argv.includes("--repin")) {
  const releases = await json("https://api.github.com/repos/BtbN/FFmpeg-Builds/releases?per_page=20");
  const branch = BTBN_BRANCH.replaceAll(".", "\\.");
  const assetRe = new RegExp(`^ffmpeg-(n${branch}[^-]*(?:-\\d+-g[0-9a-f]+)?)-win64-gpl-${branch}\\.zip$`);
  let next = null;
  for (const r of releases) {
    if (!r.tag_name.startsWith("autobuild-")) continue;
    const m = r.assets.map((a) => a.name.match(assetRe)).find(Boolean);
    if (m && r.assets.some((a) => a.name === `ffmpeg-${m[1]}-linux64-gpl-${BTBN_BRANCH}.tar.xz`)) {
      next = { tag: r.tag_name, build: m[1] };
      break;
    }
  }
  if (!next) throw new Error(`no BtbN autobuild carries a ${BTBN_BRANCH} win64 + linux64 build`);
  const [ev, evProbe] = await Promise.all([
    json("https://evermeet.cx/ffmpeg/info/ffmpeg/release"),
    json("https://evermeet.cx/ffmpeg/info/ffprobe/release"),
  ]);
  if (ev.version !== evProbe.version) throw new Error(`evermeet ffmpeg ${ev.version} vs ffprobe ${evProbe.version}`);

  const fresh = sources({
    btbn: `https://github.com/BtbN/FFmpeg-Builds/releases/download/${next.tag}`,
    build: next.build,
    branch: BTBN_BRANCH,
    evermeet: ev.version,
  });
  const self = fileURLToPath(import.meta.url);
  let text = await Bun.file(self).text();
  const swap = (from, to) => {
    if (!text.includes(from)) throw new Error(`could not find ${from} in ${self}`);
    text = text.replace(from, to);
  };
  swap(`const BTBN_TAG = "${BTBN_TAG}";`, `const BTBN_TAG = "${next.tag}";`);
  swap(`const BTBN_BUILD = "${BTBN_BUILD}";`, `const BTBN_BUILD = "${next.build}";`);
  swap(`const EVERMEET = "${EVERMEET}";`, `const EVERMEET = "${ev.version}";`);
  for (const [triple, source] of Object.entries(fresh)) {
    for (const [i, { url }] of source.archives.entries()) {
      const { digest } = await download(url);
      const old = SOURCES[triple].archives[i].sha256;
      if (old !== digest) swap(`sha256: "${old}"`, `sha256: "${digest}"`);
    }
  }
  await Bun.write(self, text);
  console.log(`pinned BtbN ${next.tag} (${next.build}), evermeet ${ev.version}`);
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
    const { bytes, digest } = await fetchPinned(url, sha256);
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
