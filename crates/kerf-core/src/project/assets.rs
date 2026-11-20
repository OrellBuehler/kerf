//! Assets on a project: import, relink and removal, the missing-file check and the analysis kept for each.

use super::*;

impl Project {
    // ---- assets -----------------------------------------------------------

    /// Probe a media file and store its asset record, stitching an Insta360 lens
    /// pair into one 360 asset on the way (see [`Project::probe_import`]).
    pub fn import_asset(&self, media_path: impl AsRef<Path>) -> Result<Asset> {
        let asset = Self::probe_import(media_path.as_ref(), &mut |_| {})?;
        self.insert_or_get_asset(&asset)
    }

    /// Probe `path` into an importable [`Asset`], *without* `&self` like
    /// [`Project::probe_asset`] — and, when `path` turns out to be one lens of an
    /// Insta360 capture, stitch its pair into a single equirectangular video and
    /// describe that instead.
    ///
    /// An Insta360 capture is written as two files, one circular fisheye per
    /// lens, neither of which is 360 footage on its own: reframing one would show
    /// half the sphere. Stitching at import means the rest of Kerf — reframing,
    /// proxies, thumbnails, export — only ever sees ordinary equirect media, and
    /// the (slow, cached) re-encode happens once per capture instead of on every
    /// preview. `progress` reports that encode; it is never called for an
    /// ordinary file, which is probe-only and instant.
    pub fn probe_import(path: &Path, progress: &mut dyn FnMut(ExportProgress)) -> Result<Asset> {
        Self::probe_import_with(path, progress, &|| false)
    }

    /// [`Project::probe_import`] that can be stopped: `cancel` is polled while the stitch waits
    /// for its slot and while it encodes, and a stop is [`Error::Cancelled`] with the
    /// half-written stitch removed (an ordinary file is probe-only and never sees it).
    pub fn probe_import_with(path: &Path, progress: &mut dyn FnMut(ExportProgress), cancel: &dyn Fn() -> bool) -> Result<Asset> {
        let asset = Self::probe_asset_with(path, cancel)?;
        let video = asset.streams.iter().find(|s| s.kind == StreamKind::Video);
        let Some((front, rear)) = video.and_then(|v| engine::insta360_pair(path, v.width, v.height)) else {
            return Ok(asset);
        };

        let stitched = engine::stitch_insta360_with(&front, &rear, asset.duration, progress, cancel)?;
        let mut stitched_asset = Self::probe_asset_with(&stitched, cancel)?;
        // The stitch is plain h264 with no spherical metadata of its own (the
        // ffmpeg CLI cannot write the `sv3d` box), so the projection we just
        // rendered it into is recorded here rather than re-detected.
        for stream in stitched_asset.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
            stream.projection = Some(Projection::Equirect);
        }
        stitched_asset.name = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(engine::insta360_pair_name)
            .unwrap_or(asset.name);
        stitched_asset.source_paths = vec![front.to_string_lossy().into_owned(), rear.to_string_lossy().into_owned()];
        Ok(stitched_asset)
    }

    /// Probe a media file into a fresh [`Asset`] record *without* `&self` — the
    /// ffprobe run doesn't need the project lock, so callers importing several
    /// files can probe them concurrently and take the lock only for the quick
    /// [`Project::insert_asset`].
    ///
    /// The path must be absolute and name an existing regular file: FFmpeg reads `http:` /
    /// `concat:` / `pipe:` strings as protocols, and a relative path means whatever the process's
    /// working directory makes it (see [`crate::paths::validate_import_path`]).
    pub fn probe_asset(path: &Path) -> Result<Asset> {
        Self::probe_asset_with(path, &|| false)
    }

    /// [`Project::probe_asset`] that can be stopped: `cancel` is polled while ffprobe runs, and a
    /// stop is [`Error::Cancelled`] with the child killed. The probe is bounded to minutes, not
    /// seconds, so a file on a slow share is read — and a job someone cancelled does not sit out
    /// the wait.
    pub fn probe_asset_with(path: &Path, cancel: &dyn Fn() -> bool) -> Result<Asset> {
        crate::paths::validate_import_path(path)?;
        let probe = engine::probe_cancellable(path, cancel)?;
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".to_string());
        // A still image probes with no duration; give it a default timeline length
        // so it's placeable (the clip can be trimmed afterwards like any other).
        let is_image = probe.streams.iter().any(|s| s.image);
        let duration = if is_image && probe.duration <= 0.0 {
            crate::model::DEFAULT_IMAGE_DURATION
        } else {
            probe.duration
        };
        Ok(Asset {
            id: Uuid::new_v4(),
            path: path.to_string_lossy().into_owned(),
            name,
            duration,
            streams: probe.streams,
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
            music_score: None,
            sound_effect: None,
            tags: Vec::new(),
        })
    }

    /// Insert an asset record directly, or update the one with the same id in place.
    ///
    /// An *update*, never a `REPLACE`: replacing deletes the old row first, and `analysis` hangs off
    /// it with `ON DELETE CASCADE`, so every rewrite of an asset (marking it 360, relinking it)
    /// used to throw away its transcript, silence map and rhythm.
    pub fn insert_asset(&self, asset: &Asset) -> Result<()> {
        self.conn.execute(
            "INSERT INTO assets (id, path, name, duration, streams, imported_at, source_paths, voiceover, music_score, tags, sound_effect)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                 path = excluded.path,
                 name = excluded.name,
                 duration = excluded.duration,
                 streams = excluded.streams,
                 imported_at = excluded.imported_at,
                 source_paths = excluded.source_paths,
                 voiceover = excluded.voiceover,
                 music_score = excluded.music_score,
                 tags = excluded.tags,
                 sound_effect = excluded.sound_effect",
            params![
                asset.id.to_string(),
                asset.path,
                asset.name,
                asset.duration,
                serde_json::to_string(&asset.streams)?,
                asset.imported_at.to_rfc3339(),
                if asset.source_paths.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&asset.source_paths)?)
                },
                asset.voiceover.as_ref().map(serde_json::to_string).transpose()?,
                asset.music_score.as_ref().map(serde_json::to_string).transpose()?,
                tags_column(&asset.tags)?,
                asset.sound_effect.as_ref().map(serde_json::to_string).transpose()?,
            ],
        )?;
        Ok(())
    }

    /// Insert `asset`, unless one for the same file is already in the project —
    /// in which case that existing asset is returned untouched.
    ///
    /// Importing both halves of an Insta360 pair (or the same file twice) would
    /// otherwise land two asset rows for one piece of media: both imports stitch
    /// to the same cached file, so they arrive here with the same `path` and
    /// different fresh ids.
    pub fn insert_or_get_asset(&self, asset: &Asset) -> Result<Asset> {
        if let Some(existing) = self.asset_by_path(&asset.path)? {
            return Ok(existing);
        }
        self.insert_asset(asset)?;
        Ok(asset.clone())
    }

    /// The locked last step of a **tick backfill**: write the time bases `found` ([`engine::probe_time_bases`]) into the video
    /// streams of the assets that still have none, and answer how many assets it wrote. An asset stored before schema v21 has no
    /// tick, and importing its file again hands back this row without probing it; an export that could fold a run of its
    /// blended clips learns the tick from the file ([`engine::time_base_inputs`], the probe with the lock released) and the
    /// project keeps it here, so no export asks again. **Not an edit**: no revision, nothing staged, nothing for undo to
    /// bring back. A tick for a path the asset has since been relinked away from, for an asset removed, and for one that has
    /// a tick already is skipped. Database only.
    pub fn apply_time_bases(&self, found: &[engine::SourceTick]) -> Result<usize> {
        if found.is_empty() {
            return Ok(0);
        }
        let tx = self.tx()?;
        let mut written = 0;
        for tick in found {
            let Some(mut asset) = self.stored_asset(tick.asset)?.filter(|a| a.path == tick.path) else {
                continue;
            };
            let Some(stream) = asset
                .streams
                .iter_mut()
                .find(|s| s.kind == StreamKind::Video)
                .filter(|s| !s.image && s.time_base.is_none())
            else {
                continue;
            };
            stream.time_base = Some(tick.time_base);
            self.insert_asset(&asset)?;
            written += 1;
        }
        tx.commit()?;
        Ok(written)
    }

    /// The asset backed by `path`, if the project already has one.
    pub fn asset_by_path(&self, path: &str) -> Result<Option<Asset>> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT {ASSET_COLUMNS} FROM assets WHERE path = ?1 LIMIT 1"),
                params![path],
                read_asset_row,
            )
            .optional()?;
        row.map(row_to_asset).transpose()
    }

    /// Every asset, oldest import first. For the agent, with a proposal open, the library as that
    /// proposal would leave it ([`StagedAssetChange`]): it sees its own pending work, the user
    /// does not until it is accepted.
    pub fn list_assets(&self) -> Result<Vec<Asset>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {ASSET_COLUMNS} FROM assets ORDER BY imported_at"))?;
        let rows = stmt.query_map([], read_asset_row)?;
        let mut assets = Vec::new();
        for row in rows {
            assets.push(row_to_asset(row?)?);
        }
        let pending = self.pending_asset_changes()?;
        if !pending.is_empty() {
            for asset in &mut assets {
                apply_asset_changes(asset, &pending);
            }
        }
        Ok(assets)
    }

    /// One asset by id (as [`Project::list_assets`] shows it).
    pub fn get_asset(&self, id: Uuid) -> Result<Option<Asset>> {
        let mut asset = self.stored_asset(id)?;
        if let Some(asset) = &mut asset {
            let pending = self.pending_asset_changes()?;
            apply_asset_changes(asset, &pending);
        }
        Ok(asset)
    }

    /// The asset exactly as stored, without any change an open proposal holds back.
    pub(super) fn stored_asset(&self, id: Uuid) -> Result<Option<Asset>> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT {ASSET_COLUMNS} FROM assets WHERE id = ?1"),
                params![id.to_string()],
                read_asset_row,
            )
            .optional()?;
        row.map(row_to_asset).transpose()
    }

    pub fn require_asset(&self, id: Uuid) -> Result<Asset> {
        self.get_asset(id)?.ok_or(Error::AssetNotFound(id))
    }

    /// The assets whose source file is no longer on disk, each with how many clips of the live
    /// timeline use it — what a "relink media" prompt lists.
    ///
    /// **That is a stat per asset, and a stat on a share that went away can take a long time to
    /// fail**, so an adapter does not call this under the project lock: it takes
    /// [`Project::missing_assets_inputs`] under it and hands the result to the static
    /// [`Project::find_missing_assets`] with the lock released. This is the two put together, for
    /// a caller that has no lock to worry about.
    pub fn missing_assets(&self) -> Result<Vec<MissingAsset>> {
        Ok(Self::find_missing_assets(self.missing_assets_inputs()?))
    }

    /// The locked half of [`Project::missing_assets`]: every asset's file and the clips of the
    /// live timeline that use it, read from the database only — no file is touched.
    pub fn missing_assets_inputs(&self) -> Result<Vec<AssetFile>> {
        let timeline = self.timeline()?;
        let mut used: HashMap<Uuid, usize> = HashMap::new();
        for clip in timeline.tracks.iter().flat_map(|t| t.clips.iter()) {
            *used.entry(clip.asset_id).or_default() += 1;
        }
        Ok(self
            .list_assets()?
            .into_iter()
            .map(|a| AssetFile {
                clips: used.get(&a.id).copied().unwrap_or(0),
                asset_id: a.id,
                name: a.name,
                path: a.path,
            })
            .collect())
    }

    /// The lock-free half of [`Project::missing_assets`]: the `files` whose path is no longer on
    /// disk. Stats each one, so call it with the project lock released.
    pub fn find_missing_assets(files: Vec<AssetFile>) -> Vec<MissingAsset> {
        files
            .into_iter()
            .filter(|f| !Path::new(&f.path).exists())
            .map(|f| MissingAsset {
                asset_id: f.asset_id,
                name: f.name,
                path: f.path,
                clips: f.clips,
            })
            .collect()
    }

    /// What the file system says about every asset's file — size and times — for sorting the library by
    /// them. Stats each file, so (as [`Project::missing_assets`]) not under the project lock: take
    /// [`Project::asset_files_inputs`] under it and hand the result to [`Project::stat_asset_files`].
    /// This is the two put together, for a caller that has no lock to worry about.
    pub fn asset_files(&self) -> Result<Vec<AssetFileInfo>> {
        Ok(Self::stat_asset_files(self.asset_files_inputs()?))
    }

    /// The locked half of [`Project::asset_files`]: every asset and the file it points at, read from
    /// the database only. `clips` is not counted here (0).
    pub fn asset_files_inputs(&self) -> Result<Vec<AssetFile>> {
        Ok(self
            .list_assets()?
            .into_iter()
            .map(|a| AssetFile {
                clips: 0,
                asset_id: a.id,
                name: a.name,
                path: a.path,
            })
            .collect())
    }

    /// The lock-free half of [`Project::asset_files`]: one [`AssetFileInfo`] per file, in order.
    pub fn stat_asset_files(files: Vec<AssetFile>) -> Vec<AssetFileInfo> {
        files
            .iter()
            .map(|f| AssetFileInfo::stat(f.asset_id, Path::new(&f.path)))
            .collect()
    }

    /// Point an asset at another file — the media was moved, renamed or restored from a backup.
    ///
    /// This is [`Project::relink_inputs`], [`Project::probe_relink`], [`Project::apply_relink_probed`]
    /// and [`Project::forget_media_caches`] in a row, all behind `&self`. **The middle and the last
    /// are media work** — an ffprobe, a read of both files' ends, a stat — so an adapter does the
    /// steps itself: the first and the third under the project lock, the others with it released.
    pub fn relink_asset(&self, asset_id: Uuid, new_path: impl AsRef<Path>) -> Result<Relinked> {
        let old = self.relink_inputs(asset_id)?;
        let probe = Self::probe_relink(&old, new_path.as_ref(), &|| false)?;
        let done = self.apply_relink_probed(asset_id, &probe)?;
        Self::forget_relinked_caches(&done);
        Ok(done)
    }

    /// The locked first step of a relink: the asset as stored (its path, length and streams are
    /// what the new file is compared against). Refused for an agent, as the relink itself is, so
    /// the refusal comes before any media work. Database only.
    pub fn relink_inputs(&self, asset_id: Uuid) -> Result<Asset> {
        self.refuse_agent_library_edit()?;
        self.stored_asset(asset_id)?.ok_or(Error::AssetNotFound(asset_id))
    }

    /// The lock-free middle of a relink: probe `new_path` ([`Project::probe_asset`], so it must be
    /// an absolute path to a file), check it can stand in for `old`
    /// ([`Project::apply_relink_probed`] holds it to the same rules) and compare it with the old
    /// file's bytes when that is still there — its size, then a hash of its first and last
    /// [`crate::paths::FINGERPRINT_BYTES`]. Reads both files: **call it with the lock released**.
    /// `cancel` stops the probe.
    pub fn probe_relink(old: &Asset, new_path: &Path, cancel: &dyn Fn() -> bool) -> Result<RelinkProbe> {
        let probed = Self::probe_asset_with(new_path, cancel)?;
        check_relink_compatible(old, &probed)?;
        let same_file = crate::paths::same_content(Path::new(&old.path), Path::new(&probed.path));
        Ok(RelinkProbe {
            probed,
            old_path: old.path.clone(),
            same_file,
        })
    }

    /// A relink asked about and not made: whether `new_path` could stand in for the asset `old`
    /// ([`Project::relink_inputs`]) and what it would cost — the same probe as
    /// [`Project::probe_relink`], so the same refusals, and [`RelinkProbe::keeps_analysis`] decides
    /// as the real relink will. `has_analysis` is whether the asset has any
    /// ([`Project::has_analysis`], read under the lock). Reads both files: **lock released**.
    pub fn check_relink(old: &Asset, has_analysis: bool, new_path: &Path, cancel: &dyn Fn() -> bool) -> Result<RelinkCheck> {
        let probe = Self::probe_relink(old, new_path, cancel)?;
        Ok(RelinkCheck {
            analysis_kept: probe.keeps_analysis(old),
            has_analysis,
        })
    }

    /// Whether the asset has analysis stored. Database only.
    pub fn has_analysis(&self, asset_id: Uuid) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM analysis WHERE asset_id = ?1",
                params![asset_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// The locked last step of a relink: `probe` is what [`Project::probe_relink`] made from the
    /// asset [`Project::relink_inputs`] returned, and nothing here touches a file.
    ///
    /// The file has to be the same kind of media: the same kinds of stream (a picture with sound
    /// stays a picture with sound) and a length within [`RELINK_DURATION_TOLERANCE`] seconds (or
    /// 1 %) of the old, since the clips cut from the old file point into it by time. The asset
    /// keeps its id, name, place in the library and every clip that uses it. **Analysis is kept
    /// only when the file is the same media moved** — same length to the millisecond, same
    /// streams, and, when the old file was still there to compare with, the same size and the same
    /// first and last bytes; any other compatible file is a different recording and its transcript,
    /// silences and beats would be wrong, so they are dropped ([`Relinked::analysis_kept`]). With
    /// the old file gone there is nothing to compare, and length and streams have to do. A proxy
    /// declined for the old file is forgiven for the new one. Caches keyed by the old file are not
    /// touched here (that needs the disk — [`Project::forget_media_caches`], lock released).
    ///
    /// The library entry may have changed while the file was being read (another relink, a
    /// removal); that is an error to retry, not a comparison against the wrong file.
    pub fn apply_relink_probed(&self, asset_id: Uuid, probe: &RelinkProbe) -> Result<Relinked> {
        self.refuse_agent_library_edit()?;
        let old = self.stored_asset(asset_id)?.ok_or(Error::AssetNotFound(asset_id))?;
        if old.path != probe.old_path {
            return Err(Error::InvalidArgument(format!(
                "\"{}\" was relinked or changed while the new file was being checked — try again",
                old.name
            )));
        }
        let probed = &probe.probed;
        if let Some(other) = self.asset_by_path(&probed.path)? {
            if other.id != old.id {
                return Err(Error::InvalidArgument(format!(
                    "{} is already in the library as \"{}\"",
                    probed.path, other.name
                )));
            }
        }
        check_relink_compatible(&old, probed)?;
        let keep = probe.keeps_analysis(&old);

        let mut asset = old.clone();
        asset.path = probed.path.clone();
        asset.duration = probed.duration;
        asset.streams = probed.streams.clone();
        // A projection the user set by hand is a property of the footage, which is the same footage.
        if let Some(projection) = old.projection().filter(|_| asset.projection().is_none()) {
            for stream in asset.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
                stream.projection = Some(projection);
            }
        }
        asset.source_paths.clear();
        let tx = self.tx()?;
        self.insert_asset(&asset)?;
        if !keep {
            self.conn
                .execute("DELETE FROM analysis WHERE asset_id = ?1", params![asset_id.to_string()])?;
        }
        self.set_proxy_declined(asset_id, false)?;
        tx.commit()?;
        Ok(Relinked {
            asset,
            analysis_kept: keep,
            previous_path: old.path,
        })
    }

    /// [`Project::apply_relink_probed`] for a caller that probed the file itself with
    /// [`Project::probe_asset`]: kept for it, as the shape before the relink was split. It stats
    /// the two files to compare their size — a stat each, under whatever lock the caller holds —
    /// but reads no content, so it cannot tell the same length of a different take; prefer
    /// `relink_inputs` → `probe_relink` → `apply_relink_probed`.
    pub fn apply_relink(&self, asset_id: Uuid, probed: &Asset) -> Result<Relinked> {
        self.refuse_agent_library_edit()?;
        let old = self.stored_asset(asset_id)?.ok_or(Error::AssetNotFound(asset_id))?;
        let same_file = crate::paths::same_size(Path::new(&old.path), Path::new(&probed.path));
        self.apply_relink_probed(
            asset_id,
            &RelinkProbe {
                probed: probed.clone(),
                old_path: old.path,
                same_file,
            },
        )
    }

    /// Take an asset out of the library. Refused while a clip of the live timeline or of an open
    /// proposal uses it ([`Error::AssetInUse`]) — unless `force`, which first removes those clips
    /// as **one** `Remove media` revision (never rippling, never touching the clips' partners;
    /// a clip on a locked track refuses it, unlock the track first) and drops the asset's clips
    /// from the open proposal, its held-back changes to the asset and a kept discarded proposal
    /// that mentions it. Its analysis goes with it. Never available to an agent.
    ///
    /// **History does not bring it back**: the edit log holds snapshots of the cut, and a snapshot
    /// that has clips of the removed asset can no longer be restored (undo, redo and revert answer
    /// with the name of the missing media); steps from before it was used
    /// still can.
    ///
    /// Caches keyed by the file (proxy, waveform) are left for [`Project::forget_media_caches`],
    /// which needs the disk and so the lock released.
    pub fn remove_asset(&self, asset_id: Uuid, force: bool) -> Result<RemovedAsset> {
        self.refuse_agent_library_edit()?;
        let asset = self.stored_asset(asset_id)?.ok_or(Error::AssetNotFound(asset_id))?;
        let timeline = self.timeline()?;
        let uses = |t: &Timeline| -> Vec<Uuid> {
            t.tracks
                .iter()
                .flat_map(|track| track.clips.iter())
                .filter(|c| c.asset_id == asset_id)
                .map(|c| c.id)
                .collect()
        };
        let live = uses(&timeline);
        let staged = self.staged_row()?;
        let proposed: Timeline = match &staged {
            Some(row) => serde_json::from_str(&row.timeline)?,
            None => Timeline::new(),
        };
        let in_proposal = uses(&proposed);
        let mut all: HashSet<Uuid> = live.iter().copied().collect();
        all.extend(in_proposal.iter().copied());
        if !all.is_empty() && !force {
            return Err(Error::AssetInUse {
                asset: asset_id,
                clips: all.len(),
            });
        }
        if !live.is_empty() {
            let label = format!("Remove media {}", asset.name);
            self.with_links(Some(false), |p| {
                p.with_ripple(Some(false), |p| {
                    p.edit_named_exact(&live, |_| label.clone(), |tl| tl.remove_clips(&live))
                })
            })?;
        }
        let tx = self.tx()?;
        if let Some(row) = &staged {
            let touched = !in_proposal.is_empty() || row.assets.iter().any(|c| c.asset_id == asset_id);
            if touched {
                let mut proposal = proposed;
                for track in &mut proposal.tracks {
                    track.clips.retain(|c| c.asset_id != asset_id);
                }
                if proposal.has_links() {
                    proposal.dissolve_all_orphans();
                }
                let assets: Vec<StagedAssetChange> = row.assets.iter().filter(|c| c.asset_id != asset_id).cloned().collect();
                self.conn.execute(
                    "UPDATE staged SET timeline = ?1, assets = ?2, updated_at = ?3 WHERE id = 1",
                    params![
                        timeline_to_json(&proposal)?,
                        serde_json::to_string(&assets)?,
                        Utc::now().to_rfc3339()
                    ],
                )?;
                // Its remembered steps still show the asset.
                self.conn.execute("DELETE FROM staged_undo WHERE 1", [])?;
            }
        }
        if self
            .meta(DISCARDED_STAGED)?
            .is_some_and(|saved| saved.contains(&asset_id.to_string()))
        {
            self.forget_discarded()?;
        }
        self.conn
            .execute("DELETE FROM assets WHERE id = ?1", params![asset_id.to_string()])?;
        self.note_removed_asset(&asset)?;
        self.set_proxy_declined(asset_id, false)?;
        tx.commit()?;
        Ok(RemovedAsset {
            asset,
            clips_removed: live.len(),
        })
    }

    /// Delete what was cached for a removed asset: its stabilization analyses and what
    /// [`Project::forget_media_caches`] deletes for its file. **Not under the project lock.**
    pub fn forget_removed_caches(removed: &RemovedAsset) {
        crate::stabilize::forget(removed.asset.id);
        Self::forget_media_caches(Path::new(&removed.asset.path));
    }

    /// Delete what was cached for a relinked asset's old file: its stabilization analyses (measured on
    /// a file the asset no longer is) and, when the path moved, the old path's proxies and waveform.
    /// **Not under the project lock.**
    pub fn forget_relinked_caches(relinked: &Relinked) {
        crate::stabilize::forget(relinked.asset.id);
        if relinked.previous_path != relinked.asset.path {
            Self::forget_media_caches(Path::new(&relinked.previous_path));
        }
    }

    /// Delete what was cached for a media file — its preview proxies and its waveform — now that
    /// the asset that used them is gone or points elsewhere. Caches are keyed by the file's path,
    /// size and modification time, so this can only find them while the file is still there; for a
    /// missing one they are simply never read again. Probes the file: **not under the project lock**.
    pub fn forget_media_caches(path: &Path) {
        if !path.exists() {
            return;
        }
        for size in crate::proxy::ProxySize::ALL {
            if size.px() > 0 {
                engine::remove_proxy_files(path, size.px());
            }
        }
        if let Some(cache) = engine::waveform_cache_path(path) {
            let _ = std::fs::remove_file(cache);
        }
    }

    /// Removing or relinking media is the user's, always. A proposal cannot carry it (the library
    /// is not in the timeline), and an agent that did it directly would change what the user is
    /// looking at: with review on that is the review it was told to wait for, and with review off
    /// a session may still be open, whose edits would go to the proposal while the asset row went
    /// from under the live cut. So it is refused outright, with or without review.
    fn refuse_agent_library_edit(&self) -> Result<()> {
        if self.actor != EditSource::Agent {
            return Ok(());
        }
        if self.agent_review.get() {
            return Err(Error::AgentReviewRequired);
        }
        Err(Error::InvalidArgument(
            "removing or relinking media is done by the user, in Kerf's library — ask them".to_string(),
        ))
    }

    // ---- analysis ---------------------------------------------------------

    pub fn set_analysis(&self, analysis: &AssetAnalysis) -> Result<()> {
        // A result that comes back for an asset removed (or a project replaced) while it was being
        // computed must not land: say so, rather than trip the foreign key.
        if self.stored_asset(analysis.asset_id)?.is_none() {
            return Err(Error::AssetNotFound(analysis.asset_id));
        }
        self.conn.execute(
            "INSERT OR REPLACE INTO analysis (asset_id, data) VALUES (?1, ?2)",
            params![analysis.asset_id.to_string(), serde_json::to_string(analysis)?],
        )?;
        Ok(())
    }

    pub fn get_analysis(&self, asset_id: Uuid) -> Result<Option<AssetAnalysis>> {
        let data = self
            .conn
            .query_row(
                "SELECT data FROM analysis WHERE asset_id = ?1",
                params![asset_id.to_string()],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        match data {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    /// Fold the results of some analysis steps — an [`crate::analysis::StepsRun::patch`]
    /// — into what is cached for the asset, leaving every other kind as it was, and
    /// return the merged analysis. A patch that ran nothing changes nothing. Errors with
    /// [`Error::AssetNotFound`] when the asset is gone — it was removed, or the project was
    /// replaced, while the analysis ran — instead of writing results for nothing.
    pub fn merge_analysis(&self, patch: &AssetAnalysis) -> Result<AssetAnalysis> {
        if self.stored_asset(patch.asset_id)?.is_none() {
            return Err(Error::AssetNotFound(patch.asset_id));
        }
        let mut analysis = self.get_analysis(patch.asset_id)?.unwrap_or_else(|| AssetAnalysis {
            asset_id: patch.asset_id,
            ..AssetAnalysis::default()
        });
        if !patch.ran.is_empty() {
            analysis.merge(patch);
            self.set_analysis(&analysis)?;
        }
        Ok(analysis)
    }

    /// Which analyses have run for an asset, which are running, failed or are switched off.
    pub fn analysis_status(&self, asset_id: Uuid) -> Result<crate::analysis::AnalysisStatus> {
        Ok(crate::analysis::analysis_status(
            asset_id,
            self.get_analysis(asset_id)?.as_ref(),
        ))
    }

    /// [`Project::analysis_status`] of every asset.
    pub fn analysis_statuses(&self) -> Result<Vec<crate::analysis::AnalysisStatus>> {
        self.list_assets()?.iter().map(|a| self.analysis_status(a.id)).collect()
    }

    /// Run the default analysis steps against an asset's media file, cache the result
    /// (merged into what is already there) and return it.
    pub fn analyze_asset(&self, asset_id: Uuid) -> Result<AssetAnalysis> {
        let asset = self.require_asset(asset_id)?;
        // The heavy ffmpeg work lives in `analysis::analyze_asset_steps`, a free
        // function — so the GUI/MCP adapters can run it without holding the
        // shared Project lock and then re-lock only for the quick `merge_analysis`.
        let run = crate::analysis::analyze_asset_steps(&asset, None, &mut |_| {}, &mut |_| {}, crate::analysis::NEVER_CANCEL)?;
        let cancelled = run.cancelled;
        self.merge_analysis(&run.patch)?;
        if cancelled {
            return Err(Error::Cancelled);
        }
        let failure = run.failure_summary();
        let analysis = self.get_analysis(asset_id)?.unwrap_or_default();
        match failure {
            Some(why) if run.patch.ran.is_empty() => Err(Error::Engine(why)),
            _ => Ok(analysis),
        }
    }
}

/// The most a relinked file's length may differ from the asset it replaces (seconds), or 1 % of it
/// if that is more.
pub const RELINK_DURATION_TOLERANCE: f64 = 0.5;

/// What [`Project::relink_asset`] did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Relinked {
    pub asset: Asset,
    /// The new file is the old media moved, so its cached analysis still holds; otherwise it was
    /// dropped and the asset wants analysing again.
    pub analysis_kept: bool,
    /// The file the asset pointed at before ([`Project::forget_media_caches`] cleans up after it).
    pub previous_path: String,
}

/// What [`Project::check_relink`] found: how a relink to a file would turn out, before it is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct RelinkCheck {
    /// The file is the old media moved, so the analysis would be kept ([`Relinked::analysis_kept`]).
    pub analysis_kept: bool,
    /// The asset has analysis to lose: when it has none, a different recording costs nothing.
    pub has_analysis: bool,
}

/// What [`Project::referenced_files`] found: the files FFmpeg opens as media (each asset's file and
/// capture files) and the LUTs Kerf reads itself. Each sorted, without repeats.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReferencedFiles {
    pub media: Vec<String>,
    pub luts: Vec<String>,
}

/// What [`Project::remove_asset`] did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemovedAsset {
    /// The asset as it was (its `path` is what [`Project::forget_media_caches`] wants).
    pub asset: Asset,
    /// Clips of the live timeline removed with it (`force` only).
    pub clips_removed: usize,
}

/// What [`Project::probe_relink`] found out about the file an asset is to be pointed at — handed
/// to [`Project::apply_relink_probed`] under the lock, so nothing there reads a file.
#[derive(Debug, Clone)]
pub struct RelinkProbe {
    /// The new file, probed.
    pub probed: Asset,
    /// The path the asset pointed at when the probe was made: the apply step refuses if that is no
    /// longer so (it was relinked or removed meanwhile).
    pub old_path: String,
    /// Whether the new file has the same bytes as the old, as far as could be told cheaply (size,
    /// then a hash of both ends): `None` when the old file is gone or either cannot be read.
    pub same_file: Option<bool>,
}

impl RelinkProbe {
    /// Whether relinking `old` to this file keeps its analysis: the same media moved — same length
    /// to the millisecond and streams, and, when the old file was there to compare, the same bytes.
    pub fn keeps_analysis(&self, old: &Asset) -> bool {
        self.same_file != Some(false) && same_media(old, &self.probed)
    }
}

/// An asset and the file it points at: the input of [`Project::find_missing_assets`], which stats
/// the files with the project lock released ([`Project::missing_assets_inputs`] lists them under
/// it).
#[derive(Debug, Clone)]
pub struct AssetFile {
    pub asset_id: Uuid,
    pub name: String,
    pub path: String,
    /// Clips of the live timeline that use the asset.
    pub clips: usize,
}

/// What the file system says about an asset's file: for sorting the library by size or date. Nothing
/// of it is stored. A field is absent when the file cannot be read or the platform keeps no such time
/// (a creation time is not recorded everywhere).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AssetFileInfo {
    pub asset_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// When the file was last written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<DateTime<Utc>>,
    /// When the file was created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<DateTime<Utc>>,
}

impl AssetFileInfo {
    /// Stat `path`. One system call that can take long on a share that went away: not under the
    /// project lock.
    pub fn stat(asset_id: Uuid, path: &Path) -> Self {
        let meta = std::fs::metadata(path).ok().filter(|m| m.is_file());
        Self {
            asset_id,
            size_bytes: meta.as_ref().map(|m| m.len()),
            modified: meta.as_ref().and_then(|m| m.modified().ok()).map(DateTime::<Utc>::from),
            created: meta.as_ref().and_then(|m| m.created().ok()).map(DateTime::<Utc>::from),
        }
    }
}

/// An asset whose source file is gone (see [`Project::missing_assets`]).
#[derive(Debug, Clone, serde::Serialize)]
pub struct MissingAsset {
    pub asset_id: Uuid,
    pub name: String,
    pub path: String,
    /// Clips of the live timeline that use it.
    pub clips: usize,
}

/// The stream kinds of an asset, ignoring how many of each: what has to match for a relink.
fn media_kinds(asset: &Asset) -> (bool, bool, bool) {
    let has = |k: StreamKind| asset.streams.iter().any(|s| s.kind == k);
    (has(StreamKind::Video), has(StreamKind::Audio), asset.is_image())
}

/// Whether `new` can stand in for `old`: the same kinds of stream, and a length close enough that
/// the clips cut from the old file still point at the same footage.
pub(super) fn check_relink_compatible(old: &Asset, new: &Asset) -> Result<()> {
    let describe = |(video, audio, image): (bool, bool, bool)| match (image, video, audio) {
        (true, _, _) => "a still image",
        (_, true, true) => "picture and sound",
        (_, true, false) => "picture only",
        (_, false, true) => "sound only",
        _ => "no audio or video",
    };
    let (was, is) = (media_kinds(old), media_kinds(new));
    if was != is {
        return Err(Error::InvalidArgument(format!(
            "that file is {} but \"{}\" is {} — pick the same media",
            describe(is),
            old.name,
            describe(was)
        )));
    }
    if !old.is_image() {
        let allowed = RELINK_DURATION_TOLERANCE.max(old.duration * 0.01);
        if (old.duration - new.duration).abs() > allowed {
            return Err(Error::InvalidArgument(format!(
                "that file is {:.1} s long but \"{}\" is {:.1} s — a relinked file may differ by at most {allowed:.1} s",
                new.duration, old.name, old.duration
            )));
        }
    }
    Ok(())
}

/// Whether `new` is the very recording `old` described: the same length to the millisecond and the
/// same streams, so what was analysed in it is still true of it.
fn same_media(old: &Asset, new: &Asset) -> bool {
    let layout = |a: &Asset| {
        a.streams
            .iter()
            .map(|s| (s.kind, s.codec.clone(), s.width, s.height, s.sample_rate, s.channels))
            .collect::<Vec<_>>()
    };
    (old.duration - new.duration).abs() <= 0.001 && layout(old) == layout(new)
}

/// How a projection reads in a review line.
pub(super) fn projection_label(p: Projection) -> &'static str {
    match p {
        Projection::Equirect => "equirect 360",
        Projection::DualFisheye => "dual-fisheye 360",
        Projection::Fisheye => "fisheye",
        Projection::Flat => "flat",
    }
}
