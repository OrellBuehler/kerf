# kerf-mcp

[Kerf](https://github.com/OrellBuehler/kerf)'s video-editing MCP server, started with one command.
Nothing to install first: the launcher downloads the Kerf release that matches its own version, checks
it against Kerf's release signing key, keeps it in a cache and runs it headless on stdin / stdout.

```bash
claude mcp add kerf -- npx -y kerf-mcp
```

…or in a `.mcp.json` / `mcpServers` config:

```json
{
  "mcpServers": {
    "kerf": { "command": "npx", "args": ["-y", "kerf-mcp", "--project", "/path/to/cut.kerf"] }
  }
}
```

The first start downloads about 110 MB (and FFmpeg, where the machine has none), which can outlast
a client's start-up timeout. Run it once by hand to have it ready:

```bash
npx -y kerf-mcp install
```

Flags after the command go to `kerf-app --headless --stdio`: `--project FILE`, `--new-project FILE`,
`--tools core,titles`, `--review`, `--data-dir DIR`, `--proxies`, `--cpu-percent N`.

## What it needs

- Node 18 or newer.
- macOS (Apple silicon or Intel), Windows x64, or a desktop Linux (x64 or arm64). On a server or in a
  container without a desktop's libraries, install the `.deb` instead
  ([`scripts/cloud-setup.sh`](https://github.com/OrellBuehler/kerf/blob/main/scripts/cloud-setup.sh)).
- FFmpeg: the Windows build carries its own. Elsewhere the one on the PATH is used; where there is none,
  a pinned build is downloaded on Linux x64 and Intel macOS. On Apple silicon and Linux arm64 install it
  yourself (`brew install ffmpeg`).

## Environment

| Variable | What it does |
| --- | --- |
| `KERF_VERSION` | Run another Kerf release than the one this package matches |
| `KERF_MCP_HOME` | Where downloads are kept (default: the user's cache directory, `kerf-mcp/`) |
| `KERF_FFMPEG`, `KERF_FFPROBE` | The FFmpeg binaries to use |

This is a separate, windowless Kerf: it edits the project file it is given, and you open that file in
the desktop app to see the result. To have an agent work in the window you are watching, connect it to
the running app instead (Settings > Agent).
