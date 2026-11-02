# Hosting Kerf's MCP server

Kerf's headless mode (`kerf-app --headless`) serves the editor's MCP tools over streamable HTTP
with no display. This directory packages it for a server that remote agents connect to:

| File | What it is |
| --- | --- |
| `Dockerfile` | The image: kerf-app built like a release (`--features whisper`), the pinned FFmpeg 9 the desktop bundles ship, a non-root user, a read-only-friendly layout |
| `entrypoint.sh` | Refuses to start without `KERF_MCP_TOKEN`, optionally prefetches models, then runs the server |
| `healthcheck.sh` | The image's `HEALTHCHECK`: an authenticated request to `/mcp` |
| `compose.yaml` + `Caddyfile` | Several tenants, one container each, behind Caddy with TLS |
| `smoke-test.sh` | What CI runs against the image (`.github/workflows/image.yml`) |

The image is x86_64 only (the pinned FFmpeg has no Linux arm64 build).

## Build

From the repository root (a cold build compiles the whole workspace in release: 30-40 minutes on
4 vCPUs, ~10 GB of disk for the build):

```bash
docker build -f deploy/Dockerfile -t kerf-mcp .
```

## Run one container

```bash
docker run -d --name kerf \
  -e KERF_MCP_TOKEN="$(openssl rand -hex 32)" \
  -p 127.0.0.1:7777:7777 \
  -v kerf-workspace:/workspace -v kerf-data:/data -v kerf-cache:/cache \
  --read-only --tmpfs /tmp \
  kerf-mcp
docker exec kerf printenv KERF_MCP_TOKEN    # the token clients need
```

| Setting | Meaning |
| --- | --- |
| `KERF_MCP_TOKEN` | **Required.** The bearer token (32+ characters, no whitespace); every request must carry it |
| `KERF_MCP_ADDR` | Where to listen inside the container (default `0.0.0.0:7777`) |
| `KERF_PREFETCH_MODELS` | Download models into `/cache` before serving, e.g. `whisper:base,kokoro,demucs,soundfont`; a bad list stops the start, a failed download only warns (it is fetched on first use) |
| `KERF_WHISPER_MODEL` | The speech model transcription uses (default `base`); match it to a prefetched `whisper:NAME` |
| `KERF_WORKSPACE` | Set to `/workspace`: builds with workspace confinement refuse paths outside it |
| `KERF_CACHE_DIR`, `KERF_CACHE_MAX_GB` | Cache location (`/cache`) and an optional ceiling in GiB, for builds that have the cache cap |
| arguments after the image name | Passed to `kerf-app`, e.g. `--project /workspace/cut.kerf`, `--review`, `--cpu-percent 75` |

Volumes: `/workspace` holds the media and projects the agent works on, `/data` settings, logs,
recovery files, voiceovers and generated music, `/cache` models, proxies, waveforms and stems.
They are owned by uid 10001; a bind mount must be writable by it. `/cache` must allow `exec`
(the ONNX Runtime library for voiceover and stems is downloaded there and loaded).

The container stops cleanly on `docker stop` (SIGTERM: jobs are cancelled, an untitled project
with work is kept for recovery in `/data`).

## Several tenants

A Kerf process edits one project, and its workspace confinement is per process, so tenants never
share a container. `compose.yaml` runs two tenants (own token, own three volumes, CPU / memory /
pid limits, read-only root, no capabilities) behind Caddy, which gets a certificate for
`KERF_HOST` and routes by path:

```
https://$KERF_HOST/tenant-a/mcp  ->  tenant-a:7777/mcp
https://$KERF_HOST/tenant-b/mcp  ->  tenant-b:7777/mcp
```

```bash
cd deploy
printf 'KERF_HOST=kerf.example.com\nTENANT_A_TOKEN=%s\nTENANT_B_TOKEN=%s\n' \
  "$(openssl rand -hex 32)" "$(openssl rand -hex 32)" > .env
docker compose up -d --build
```

Kerf listens on `0.0.0.0` inside each container, which accepts any `Host` header, so the proxied
host needs no rewriting (a server bound to a specific address checks `Host` against it). Caddy
flushes every response at once (`flush_interval -1`): MCP answers and progress arrive over SSE.
A new tenant is one more service (copy `tenant-b`, new token variable and volumes) and one more
`handle_path` block.

## Connect a client

```bash
claude mcp add --transport http kerf https://kerf.example.com/tenant-a/mcp \
  --header "Authorization: Bearer $TENANT_A_TOKEN"
```

Any MCP client that speaks streamable HTTP works the same way: the URL, plus the
`Authorization: Bearer` header. `?tools=core,titles` on the URL narrows the tool list.

## Getting media in and out

For now, put files in the tenant's workspace volume (`docker cp clip.mp4 kerf:/workspace/`, or
`scp` / `rsync` to the volume's directory on the host) and have the agent import them by path
under `/workspace`; exports written there are collected the same way. An HTTP upload endpoint
is coming.

## Hardware

Everything runs on the CPU (no GPU is used): slower than a desktop with hardware encoding, but
correct.

| | vCPU | RAM | Disk |
| --- | --- | --- | --- |
| Minimum, per tenant | 4 | 8 GB | ≈ 2 × the media + ~1 GB of models |
| Comfortable | 8 | 16 GB | the same, on SSD |

Disk is the media itself, proxies and renders (≈ the media again) and the models: whisper
`base` 142 MB (`small` 466 MB, `large-v3-turbo` 1.6 GB), Kokoro ~100 MB, Demucs ~170 MB, the
SoundFont 32 MB. Each tenant's cache holds its own copy. `--cpu-percent` (or the compose `cpus`
limit) keeps one tenant's export from starving the others.

## Security

- **A token per tenant**, from `openssl rand -hex 32`, kept in `.env` or a secret store, never in
  the image. Kerf compares it in constant time, never logs it, and refuses to listen beyond
  loopback without one. Revoking means a new token and a restart.
- **TLS in front.** The token is a bearer secret: never expose port 7777 itself to the network;
  let Caddy (or another proxy) terminate TLS.
- **One container per tenant.** Path confinement is enforced by the MCP API, not by the kernel:
  the container boundary is what keeps one tenant's files and processes from another's.
- **Least privilege.** The compose file runs with a read-only root filesystem, a `/tmp` tmpfs,
  all capabilities dropped, `no-new-privileges` and resource limits; the process runs as uid
  10001. Requests that carry a browser `Origin` are refused outright.
