#!/usr/bin/env node
// Runs Kerf's MCP server on stdin / stdout: downloads the release that matches this package once,
// checks it against the updater's signing key, unpacks it into a cache and starts
// `kerf-app --headless --stdio`. Everything this script says goes to stderr; stdout is the protocol's.

import { spawn, spawnSync } from "node:child_process";
import { createHash, createPublicKey, verify } from "node:crypto";
import { createReadStream, createWriteStream, existsSync, readFileSync, realpathSync } from "node:fs";
import { chmod, mkdir, mkdtemp, readdir, rename, rm, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const pkg = JSON.parse(readFileSync(join(here, "..", "package.json"), "utf8"));
const REPO = "OrellBuehler/kerf";
// The updater's minisign public key (`plugins.updater.pubkey` in tauri.conf.json, decoded).
const PUBKEY = "RWSt85NC93I1lr/ufZqJKoFwD6WPOEB3VYBmdjeIGDVAMfJEUDtD1sjI";

const say = (line) => process.stderr.write(`kerf-mcp: ${line}\n`);
function fail(line) {
  say(line);
  process.exit(1);
}

const USAGE = `kerf-mcp ${pkg.version}: Kerf's MCP server over stdio

  npx -y kerf-mcp [kerf-app flags]   start the server (downloads Kerf on first use)
  npx -y kerf-mcp install            download and unpack only, so the first start is quick
  npx -y kerf-mcp where              print where this version is unpacked

Flags after the command go to \`kerf-app --headless --stdio\`: --project FILE, --new-project FILE,
--tools core,titles, --review, --data-dir DIR, …

Environment: KERF_VERSION picks another release, KERF_MCP_HOME moves the cache,
KERF_FFMPEG / KERF_FFPROBE name the FFmpeg to use.
`;

function cacheRoot() {
  if (process.env.KERF_MCP_HOME) return process.env.KERF_MCP_HOME;
  if (process.platform === "win32")
    return join(process.env.LOCALAPPDATA || join(homedir(), "AppData", "Local"), "kerf-mcp");
  if (process.platform === "darwin") return join(homedir(), "Library", "Caches", "kerf-mcp");
  return join(process.env.XDG_CACHE_HOME || join(homedir(), ".cache"), "kerf-mcp");
}

function asset(version) {
  const key = `${process.platform}-${process.arch}`;
  const name = {
    "linux-x64": `Kerf_${version}_amd64.AppImage`,
    "linux-arm64": `Kerf_${version}_aarch64.AppImage`,
    "darwin-arm64": `Kerf_${version}_aarch64.app.tar.gz`,
    "darwin-x64": `Kerf_${version}_x64.app.tar.gz`,
    "win32-x64": `Kerf_${version}_x64_en-US.msi`,
  }[key];
  if (!name) fail(`Kerf has no build for ${key}`);
  return { name, url: `https://github.com/${REPO}/releases/download/v${version}/${name}` };
}

async function download(url, dest, hash, label) {
  let res;
  for (let attempt = 1; ; attempt++) {
    try {
      res = await fetch(url, { redirect: "follow" });
      if (res.ok || (res.status < 500 && res.status !== 429) || attempt >= 4) break;
    } catch (err) {
      if (attempt >= 4) throw new Error(`${url}: ${err.cause?.message || err.message}`);
    }
    await new Promise((r) => setTimeout(r, attempt * 2000));
  }
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
  const total = Number(res.headers.get("content-length")) || 0;
  const digest = createHash(hash);
  let seen = 0;
  let told = 0;
  const meter = async function* (source) {
    for await (const chunk of source) {
      digest.update(chunk);
      seen += chunk.length;
      if (label && total && seen - told >= total / 5) {
        told = seen;
        say(`${label}: ${Math.round((seen / total) * 100)}% of ${Math.round(total / 1048576)} MB`);
      }
      yield chunk;
    }
  };
  await pipeline(Readable.fromWeb(res.body), meter, createWriteStream(dest));
  return digest.digest();
}

async function text(url) {
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
  return res.text();
}

// A Tauri `.sig` is a base64'd minisign signature: the file's BLAKE2b-512 signed with Ed25519, and a
// second signature over the first plus the trusted comment.
export function minisignOk(sigFile, blake2b512, pubkey = PUBKEY) {
  const lines = Buffer.from(sigFile.trim(), "base64").toString("utf8").split("\n");
  const sig = Buffer.from(lines[1] || "", "base64");
  const comment = (lines[2] || "").replace(/^trusted comment: /, "");
  const global = Buffer.from(lines[3] || "", "base64");
  const key = Buffer.from(pubkey, "base64");
  if (sig.length !== 74 || global.length !== 64 || key.length !== 42) return false;
  if (sig.subarray(0, 2).toString() !== "ED" || !sig.subarray(2, 10).equals(key.subarray(2, 10))) return false;
  const ed = createPublicKey({
    key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), key.subarray(10)]),
    format: "der",
    type: "spki",
  });
  const signature = sig.subarray(10);
  return (
    verify(null, blake2b512, ed, signature) &&
    verify(null, Buffer.concat([signature, Buffer.from(comment, "utf8")]), ed, global)
  );
}

function run(cmd, args, opts = {}) {
  const out = spawnSync(cmd, args, { stdio: ["ignore", "ignore", "pipe"], ...opts });
  if (out.error) throw new Error(`${cmd}: ${out.error.message}`);
  if (out.status !== 0) throw new Error(`${cmd} ${args.join(" ")} failed (${out.status}): ${String(out.stderr).trim()}`);
}

async function find(root, wanted) {
  for (const entry of await readdir(root, { withFileTypes: true })) {
    const full = join(root, entry.name);
    if (entry.isDirectory()) {
      const hit = await find(full, wanted);
      if (hit) return hit;
    } else if (wanted(entry.name, full)) {
      return full;
    }
  }
  return null;
}

async function unpack(file, into) {
  if (process.platform === "linux") {
    // Unpacked, not mounted: an AppImage needs FUSE to run as one file, and containers rarely have it.
    await chmod(file, 0o755);
    run(file, ["--appimage-extract"], { cwd: into });
  } else if (process.platform === "darwin") {
    run("tar", ["-xzf", file, "-C", into]);
  } else {
    // An administrative "install" copies the files out and registers nothing.
    run("msiexec", [`/a "${file}" /qn TARGETDIR="${into}"`], { windowsVerbatimArguments: true });
  }
}

async function binary(dir) {
  if (process.platform === "linux") {
    const apprun = join(dir, "squashfs-root", "AppRun");
    return existsSync(apprun) ? apprun : null;
  }
  if (process.platform === "darwin") {
    const macos = await find(dir, (name, full) => /\.app\/Contents\/MacOS\/[^/]+$/.test(full) && !/^ff(mpeg|probe)$/.test(name));
    return macos;
  }
  return find(dir, (name) => name.toLowerCase() === "kerf-app.exe");
}

// Unpacks beside the final directory and renames into place, so a second client starting at the same
// moment either finds a whole install or makes its own and loses the rename.
async function install(version) {
  const root = cacheRoot();
  const dir = join(root, version);
  if (existsSync(join(dir, ".ready"))) return dir;
  await mkdir(root, { recursive: true });
  const { name, url } = asset(version);
  const work = await mkdtemp(join(root, `.${version}-`));
  try {
    say(`downloading Kerf ${version} (first use; kept in ${root})`);
    const file = join(work, name);
    const [hash, sig] = await Promise.all([download(url, file, "blake2b512", "Kerf"), text(`${url}.sig`)]);
    if (!minisignOk(sig, hash)) throw new Error(`${name} is not signed by Kerf's release key; nothing was run`);
    const tree = join(work, "app");
    await mkdir(tree);
    say("unpacking");
    await unpack(file, tree);
    await rm(file, { force: true });
    if (!(await binary(tree))) throw new Error(`${name} unpacked without a kerf-app in it`);
    await writeFile(join(tree, ".ready"), `${version}\n`);
    try {
      await rename(tree, dir);
    } catch (err) {
      if (!existsSync(join(dir, ".ready"))) throw err;
    }
  } finally {
    await rm(work, { recursive: true, force: true });
  }
  return dir;
}

function works(cmd) {
  return spawnSync(cmd, ["-version"], { stdio: "ignore" }).status === 0;
}

async function sha256(file) {
  const digest = createHash("sha256");
  await pipeline(createReadStream(file), digest);
  return digest.digest("hex");
}

// Kerf's Windows build carries its own FFmpeg; elsewhere it uses the one on the PATH. Where there is
// none, the builds the Windows installer is made from are fetched, each checked against its pin.
async function ffmpeg(env) {
  if (process.platform === "win32") return;
  const need = [
    ["KERF_FFMPEG", "ffmpeg"],
    ["KERF_FFPROBE", "ffprobe"],
  ].filter(([name, cmd]) => !env[name] && !works(cmd));
  if (!need.length) return;
  const pins = JSON.parse(readFileSync(join(here, "..", "ffmpeg-pins.json"), "utf8"));
  const pin = pins.targets[`${process.platform}-${process.arch}`];
  if (!pin) {
    const how = process.platform === "darwin" ? "brew install ffmpeg" : "install it with your package manager";
    fail(`FFmpeg is not installed and there is no pinned build for ${process.platform}-${process.arch}: ${how}`);
  }
  const dir = join(cacheRoot(), "ffmpeg", pin.archives[0].sha256.slice(0, 16));
  if (pin.archives.some((archive) => archive.name.endsWith(".xz")) && spawnSync("xz", ["--version"], { stdio: "ignore" }).status !== 0)
    fail("FFmpeg is not installed, and unpacking the pinned build needs xz: install ffmpeg or xz (xz-utils) with your package manager");
  if (!existsSync(join(dir, ".ready"))) {
    const work = await mkdtemp(join(cacheRoot(), ".ffmpeg-"));
    try {
      say(`downloading FFmpeg ${pins.version} (none on the PATH)`);
      const out = join(work, "bin");
      await mkdir(out);
      for (const archive of pin.archives) {
        const file = join(work, archive.name);
        let got = null;
        for (const url of [archive.url, `${pins.mirror}/${archive.sha256}-${archive.name}`]) {
          try {
            await download(url, file, "sha256", "FFmpeg");
            got = await sha256(file);
            if (got === archive.sha256) break;
          } catch (err) {
            say(String(err.message));
          }
        }
        if (got !== archive.sha256) throw new Error(`could not fetch ${archive.name} with the pinned checksum`);
        const tree = join(work, `x-${archive.sha256.slice(0, 8)}`);
        await mkdir(tree);
        run("tar", ["-xf", file, "-C", tree]);
        for (const member of archive.wants) {
          const src = await find(tree, (name) => name === member);
          if (!src) throw new Error(`${member} is not in ${archive.name}`);
          await rename(src, join(out, member));
          await chmod(join(out, member), 0o755);
        }
      }
      await writeFile(join(out, ".ready"), `${pins.version}\n`);
      await mkdir(dirname(dir), { recursive: true });
      try {
        await rename(out, dir);
      } catch (err) {
        if (!existsSync(join(dir, ".ready"))) throw err;
      }
    } finally {
      await rm(work, { recursive: true, force: true });
    }
  }
  for (const [name, cmd] of need) env[name] = join(dir, cmd);
}

async function main() {
  const args = process.argv.slice(2);
  if (args[0] === "--help" || args[0] === "-h" || args[0] === "help") {
    process.stderr.write(USAGE);
    return;
  }
  const version = process.env.KERF_VERSION || pkg.version;
  if (!/^\d+\.\d+\.\d+$/.test(version)) fail(`'${version}' is not a Kerf version`);
  const command = ["install", "where"].includes(args[0]) ? args.shift() : "serve";
  if (command === "where") {
    say(join(cacheRoot(), version));
    return;
  }
  const env = { ...process.env };
  const dir = await install(version);
  await ffmpeg(env);
  const bin = await binary(dir);
  if (command === "install") {
    say(`ready: ${bin}`);
    return;
  }
  const child = spawn(bin, ["--headless", "--stdio", ...args], { stdio: "inherit", env });
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) process.on(signal, () => child.kill(signal));
  child.on("error", (err) => fail(`could not start ${bin}: ${err.message}`));
  child.on("exit", (code, signal) => {
    // The AppImage leaves what every desktop has to the system; a container or a server may not have it.
    if (code === 127 && process.platform === "linux")
      say("Kerf needs a desktop's base libraries (X11, GL, fontconfig, harfbuzz). On a server or in a container, install the .deb instead: scripts/cloud-setup.sh in the Kerf repository does it");
    process.exit(signal ? 1 : (code ?? 1));
  });
}

if (process.argv[1] && realpathSync(fileURLToPath(import.meta.url)) === realpathSync(process.argv[1])) {
  main().catch((err) => fail(err.message));
}
