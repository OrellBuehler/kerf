# Screenshots & brand assets

This folder is the **single source of truth** for images used by both the repository
`README.md` and the Hugo landing site in [`../../site`](../../site). The Pages workflow
copies everything here into the site's `static/img/` at build time, so a file dropped
here appears in both places.

`kerf-mark.svg` is the Kerf logo — two clip bars split by the amber "cut" line.

## Shot list

Capture these from the running desktop app (dark theme, a real project loaded) and save
them here with the **exact filenames** below. PNG, retina if you can. Aim for a populated
timeline — a few clips across two or three tracks, a waveform, a scene marker or two.

| Filename                     | What to capture                                                                 | Used by            |
| ---------------------------- | ------------------------------------------------------------------------------- | ------------------ |
| `screenshot-editor.png`      | The **full editor** window — title bar, media bin, preview, multi-track timeline. Hero shot. | README + site hero |
| `screenshot-agent.png`       | The **agent panel** — task queue (a `ready` task with Apply/Dismiss) + history. | README + site      |
| `screenshot-inspector.png`   | The **inspector** — an effects chain, the Transform/keyframe panel, or overlays. | README + site      |

Only those three are referenced today. `screenshot-timeline.png` (a tight crop of the
timeline — clips, waveforms, beat ticks, playhead) is a nice-to-have you can add and wire
into the features section later.

Tips:
- Hide any OS chrome; capture just the app window.
- A 16:10-ish crop reads best in the README hero (≈ 880 px wide as displayed).
- Keep the amber playhead visible — it's the brand's signature.

Until these exist, the `README` and site show broken-image placeholders where they go.
