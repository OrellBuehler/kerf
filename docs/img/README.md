# Screenshots & brand assets

This folder is the **single source of truth** for images used by both the repository
`README.md` and the Hugo landing site in [`../../site`](../../site). The Pages workflow
copies everything here into the site's `static/img/` at build time, so a file dropped
here appears in both places.

`kerf-mark.svg` is the Kerf logo — two clip bars split by the amber "cut" line. The
app icon is the same mark on navy: `crates/kerf-app/icons/icon-source.svg` is the 1024px
master every bundled icon is generated from (`bunx @tauri-apps/cli@2 icon <png> -o crates/kerf-app/icons`).

## Shot list

Capture these from the running desktop app (dark theme, a real project loaded) and save
them here with the **exact filenames** below. PNG, retina if you can. Aim for a populated
timeline — a few clips across two or three tracks, a waveform, a scene marker or two.

| Filename                     | What to capture                                                                 | Used by                  |
| ---------------------------- | ------------------------------------------------------------------------------- | ------------------------ |
| `screenshot-editor.png`      | The **full editor** window — title bar, media bin, preview, multi-track timeline. Hero shot. | README                   |
| `screenshot-agent.png`       | The **agent panel** — task queue (a `ready` task with Apply/Dismiss) + history. | README                   |
| `screenshot-inspector.png`   | The **inspector** — an effects chain, the Transform/keyframe panel, or overlays. | README                   |

Only those three are referenced today. `screenshot-timeline.png` (a tight crop of the
timeline — clips, waveforms, beat ticks, playhead) is a nice-to-have you can add and wire
into the features section later.

Tips:
- Hide any OS chrome; capture just the app window.
- A 16:10-ish crop reads best in the README hero (≈ 880 px wide as displayed).
- Keep the amber playhead visible — it's the brand's signature.

Until these exist, the `README` shows broken-image placeholders where they go. The
landing site no longer embeds them — it renders animated CSS/SVG mockups instead.

## `og.png` — the social-card image

`og.png` (1200×630) is the site's `og:image`, and it is **generated, not captured**:
[`../og-source.html`](../og-source.html) is the layout, rendered headless at exactly
that size. It lives one level up so the Pages build, which mounts *this* folder at
`static/img/`, does not publish it as a page.

```bash
google-chrome --headless=new --disable-gpu --hide-scrollbars \
  --screenshot=docs/img/og.png --window-size=1200,630 \
  --virtual-time-budget=6000 "file://$PWD/docs/og-source.html"
```

The budget matters: the page pulls Space Grotesk / Inter / JetBrains Mono from Google
Fonts, and a shorter one screenshots the fallback faces.
