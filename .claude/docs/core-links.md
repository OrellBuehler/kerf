# kerf-core: Linked A/V (`model/links.rs`)

- **Linked A/V** (`model/links.rs`, pure + unit-tested; a child module of `model`) is a
  picture and its sound as one piece of material. **`Clip.link_id`** joins clips into a
  group (at most one clip per track; omitted when `None`); **`Clip.source_audio`**
  (default `true`, omitted) is whether the clip plays the audio of its *own* asset. Only
  `source_audio: false` reaches the graph — `clip_sounds` (`engine/cli.rs`) drops the clip
  from the audio mix, in the export gating, `validate_export` and `cut_summary` — so
  every existing graph is byte-identical and the golden oracle did not move.
  **The `extract_audio` doubling was verified, then fixed.** The export mixes the audio
  of every clip whose asset has an audio stream, video tracks included, so appending the
  asset's audio to A1 with its picture still on V1 summed the sound with itself: graph
  level (`amix=inputs=2`, two identical `atrim` chains) and on a real render (**+6.02 dB**
  over the clip alone; fixed, +0.00 dB; an A1 fader at 0.5 then reads -6.02 dB —
  `engine/cli/linked_audio.rs`). **`extract_audio(asset)`** now only **detaches**: each
  picture clip of the asset on a video track still playing its own sound (a clip on a
  locked track is *skipped and reported*, not a reason to fail the rest; nothing to
  detach is an error that says so — it never falls through to appending, which a second
  call used to do), in one revision, answering `DetachedMany {detached, skipped}`.
  Putting an asset's whole audio on an audio track is its own op, **`add_asset_audio`**
  (the bin's action for an asset that is not playing its own sound, music). **`detach_audio(clip)`**:
  an audio clip with the same source span, speed and position on the audio track at the
  picture's own position (V1 → A1) when it has room, else the first that does, else a new
  `A{n}`; linked to the picture, whose `source_audio` goes false; **`detach_audio_clips(ids)`**
  is the batch (one `Detach audio (N clips)` revision, skip-and-report, errors only when
  nothing detached). **What detaching keeps is the level, not the whole strip:** a video
  track's fader rides its clips' own sound and the audio track has one of its own, so the
  new clip's volume is `volume × picture fader ÷ audio fader` (a lane whose fader is at
  zero is never chosen, and the render check measures +0.00 dB through a V1 fader of 0.5
  into an A1 fader of 2.0) — **exact only while the clip's chain is linear**: folding the
  fader into the volume moves the gain *ahead of* a compressor or gate
  (`AudioEffect::is_dynamic`), which then reacts to a different level. So a clip with one
  goes to a lane whose fader **equals** the picture track's — an existing one with room,
  else a **new audio track at that fader**, there is no skip path — and its volume is left
  alone; filters and EQ commute with gain and still fold; the destination's **pan, duck flag and mute/solo** now decide
  the mix, and that difference is documented rather than hidden. The audio clip also carries audio
  effects, fades and the transition (the `audio_clip_chain` strings are pinned equal
  before / after at neutral faders); the picture keeps inert copies so **`reattach_audio`**
  (name either clip; `edit_timeline_exact`, never rippled) restores it — and **refuses when
  unmuting would double the sound**: a picture whose audio clip is gone, with some other
  audio clip already playing the same footage in step over the same time.
  **`reattach_audio_clips(ids)`** is the multi-select reattach (`Timeline::reattach_audio_many`, one
  `Reattach audio (N clips)` revision): **all or nothing**, unlike the detach batch, because skipping
  a pair would leave the selection half undone — each id names a picture or its sound (a pair named
  by both counts once), every reattach is judged against the cut the earlier ones left (so it runs on
  a copy that replaces the timeline only when all went through) and the first refusal is the error,
  naming its clip when there are several. Imports /
  `cut_clip` / `add_clip` do **not** auto-link an A/V asset's sound (a possible follow-up).
  **Edits carry their change to the partners**, and a partner on a locked track refuses
  the whole edit (a linked edit is a group edit, so it also checks the named clip's own
  lock, which the single-clip ops still do not): *move* by the same Δt on each partner's
  own track (a track change is the named clip's alone); *trim* (`carry_extent_edit`) moves
  the edge **a partner shares within 1 ms**, clamped to its footage, and then lane-checked
  **after the ripple** (`Timeline::check_carried_lanes`, run by `run_edit` between the sync lock and
  the guard on the partners `Project::trim` / `snap_to_beats` recorded in `edit_carried`): a partner
  that now overlaps a clip outside its group, where the two did not overlap before, refuses the edit
  naming the lane — the rule `move_clips` holds a moved partner to. It cannot be checked inside
  `carry_extent_edit`, because ripple legitimately makes room (a sound extended with its picture's tail
  pushes the voice-over behind it; a move by trim changes no length, so nothing ripples and the sound
  lands on it). The named clip's own lane is still not checked, as a trim never has; a sound carried before 0 loses its head (the lead
  is reported, below), a **picture** is never trimmed to fit and refuses, as does any clip
  left under `MIN_EDIT_CLIP` (0.05 s — refused, not stubbed) and one a trim would take
  entirely; *split* cuts every partner the time is inside and then
  re-forms the group **by side** (`relink_sides`): the left halves and any partner wholly
  before the cut keep the group, the right halves and any partner wholly after it get a new
  one — an unsplit partner lying after the cut used to stay linked to the *left* half and
  desync silently when the right half moved (17 of 1418 fuzz splits); *remove* / *ripple
  delete* take the partners; *cut a source range* takes the same stretch of **timeline**
  out of each overlapping partner (a partner whose head was inside the stretch resumes at
  the cut, one spanning it is cut in two) and relinks by side; what a partner keeps *after*
  the stretch is moved to the cut **explicitly** (`closing`), not left to the lock, because
  once the named clip keeps nothing after the cut the piece has no second group member to
  follow (V1 `X[0..10]` / A1 `S[9..15]`, cut X 8..10: S's remainder starts at 8, not 9), and
  a partner's leftover is a *linked* clip for the purpose of making room (`settle_linked`:
  linked now ∪ linked before, through `origin`), not an unlinked obstacle; *speed* applies the same
  **ratio**; *split-and-remove* cuts partners the time is inside; *roll* rolls each
  partner pair sharing the cut, *slip* the same timeline moment of footage (scaled by the
  speed ratio, stills skipped), *slide* each partner with its own neighbours — all clamped
  to the **intersection of the members' ranges**; the beat snap re-syncs afterwards
  (`carry_links_since`).
  **The sync lock is range-based (`Timeline::conform_links`)** — *what in step means* is
  equal **content offsets** (`content_offset`: the timeline time at which source time 0
  would play), so a sound that leads or trails its picture (a J- or L-cut) is in step and
  stays so. The per-lane ripple (`ripple_lanes`) and the ops above move clips nobody
  named — the next shot after a delete, its sound on another track — so after every
  edit each link group is put back in the relationship it had: every member's offset
  moved is measured against its own before (a clip an op *created*, the tail of a cut,
  is measured against the clip it was cut from, `origin`), the group's **authority** is
  the member the edit **named** (`anchors`, from `edit_named*`; the first in track order
  when it named several), else the member on a named clip's track, else the first member
  that moved, and the others — *every* other member, other named ones included — are
  *shifted* by the difference. Whether named members "moved apart" is judged on the
  timeline **as the edit left it**, before the per-lane ripple (`left`, taken in `run_edit`
  when ripple and a link both apply): a trim to the playhead names a picture *and* its
  sound (clicking selects partners) and cuts both at the same moment, so they agree there
  and only the ripple, which pulls each track by its own length, sets them apart — which
  the lock puts right (this used to refuse the edit 90 times in 101, steering to "unlink
  them first"). `move_clips` with partners at different deltas still moves them apart
  itself and stays refused. A shift keeps a clip's length, so a ripple's removed or inserted
  span reaches every linked track without ever cutting a partner (only a cut range,
  an explicit removal, cuts one). **Only clips in a group follow**; an unlinked clip on
  a partner's track stays where it was. **A picture is never silently cut.** A follower
  that lands on linked material wins against it only when the clip it ran into is a
  **sound** (`Track::settle_followers`: trimmed back, at least `MIN_EDIT_CLIP` left) and a
  sound stops at 0 by losing its head; **every sound so trimmed is reported** — the
  track's name goes to `Project::edit_notes` (a side channel, since the closures return
  their own types) and the revision label ends `(trimmed sound on A2)`, live and staged
  alike. It refuses — with a reason naming the lane — for a locked track, an **unlinked**
  clip in its way, a **picture** in its way or pushed before 0, or a clip it would leave
  under 0.05 s. The refusal rate is the price: on the J/L fuzz 13% of the moving edits are
  blocked (was ~5% when a picture could be cut) — 2.8% of those that name a picture, 23% of
  those that name a sound, where ripple pulls the *next shot's picture* up onto the one
  before; the message says so and offers Alt. A J-cut's lead lost at 0 is *trimmed and
  reported* (the picture is untouched and the lead is a sound's), not refused. `ripple_delete`
  closes the named clip's track by *its* length and leaves partner tracks to the lock (a
  J-cut pair closes by the picture removed); `reorder` carries partners the same way. Two
  *named* members moved apart by hand are left for **the sync guard**
  (`first_sync_break`, last in `run_edit`): it refuses with `out of step … unlink them
  first if they are meant to part` — it no longer steers anyone to `link: false`, which
  desyncs — and names the **lowest pair of tracks** (it used to take whichever group a
  `HashMap` met first, so the same refusal read differently run to run; the TS mirror picks
  the same pair). Measured on a J/L-cut fuzz (250 seeds × 10 edits, ripple on and off): none
  of ripple delete / remove / trim / speed / cut range / split-remove (one or both partners
  named) is refused for an unstated reason, and the blocks that remain name themselves — an
  unlinked clip or a picture in the way, a linked clip a follower would cover, a clip left
  under 0.05 s, a partner a trim would take entirely (the old per-track ripple refused
  these 10-65%). **`run_edit`** (`edit_timeline` / `edit_timeline_exact` /
  `edit_named*`): scratch snapshot only if ripple or links apply, then `f`, `ripple_lanes`,
  `conform_links`, the carried-lane check, the guard, and finally **`dissolve_all_orphans`** — a link left with
  one clip (its partner cut, deleted, or on a removed track) is cleared in the same edit.
  `edit_named*` take the named clip ids and a **label computed from the result** (a group
  edit counts the partners it carried; `trimmed_suffix` appends the reported sounds), so unlinked projects never reload the timeline
  for a label; `working_has_links` answers "does this timeline link anything" with one
  `instr` over the stored JSON, which is what keeps `trim` from loading every asset on a
  project that links nothing. `LinkIndex` (one pass) replaces `link_partners` per clip in
  the multi-clip paths and `linked_clip_ids` feeds `timeline_summary`. Pairs already apart,
  different assets, `link: false` and a project that links nothing (no snapshot taken) are
  exempt from the lock and the guard. Property edits (volume / fades / effects / colour /
  transitions), `set_clip_enabled` and captions are not carried (offsets do not change).
  **Paste / duplicate** give copies of a pasted group a fresh shared link id, and a muted
  picture pasted *without* an audio partner carrying the same footage gets its own sound
  back (it would be silent for good). **`Project::with_links(Option<bool>, ..)`** is
  `with_ripple`'s sibling — no project switch, links are on unless a call says `false`;
  `run_edit` hands it to the ripple pass and the lock. `link_clips` / `unlink_clips`
  (unlinking either half of a pair unlinks it; a group left with one clip dissolves),
  `detach_audio`, `reattach_audio` are one revision each; `Timeline::diff` reports `linked` /
  `unlinked` / `own sound off|on`. `Project::sample()` seeds its interview sound
  detached-then-*unlinked*.
  **The browser harness is held to all of it by a differential corpus**: `project/linked_corpus.rs`
  writes 102 edits (random J/L, mirrored and titled cuts, plus hand-made cases for every
  rule above — both partners named, a cut's leftover and its lone resumed piece, a picture
  victim, the 0.05 s floor, a lead lost at 0) with the answer `Project` gave — canonical timeline (no ids: clips an edit
  creates have random ones), revision label, report or the exact refusal — to
  `frontend/src/lib/fixtures/links-corpus.json`; `links-corpus.test.ts` replays each through
  `link-ops.ts` (the pure mirror of `Project`'s ops and of `run_edit`, which `api.ts` now
  composes) and demands the same. A freshness test fails a stale file; regenerate with
  `KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- links_corpus`.
  The fixture is pinned `eol=lf` in `.gitattributes` (like the golden argv files) *and* the
  freshness test compares with `\r\n` normalized, so a Windows checkout with `autocrlf`
  cannot fail it for a line ending.
