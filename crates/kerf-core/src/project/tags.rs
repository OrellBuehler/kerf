//! Asset tags on a project: editing them (staged for an agent like a projection change), the vocabulary, and
//! `assemble_by_tag`, which lays the library out as a cut grouped by the value of one key, a title per group.
//!
//! The tag model itself (spelling, edits, filters, grouping) is pure and lives in `model/tags.rs`.

use uuid::Uuid;

use super::title_clips::{check_title_room, place};
use super::{default_track, Project};
use crate::error::{Error, Result};
use crate::model::{
    group_by_key, tags_match, Asset, Clip, EditSource, StagedAssetChange, StreamKind, SvgPresetKind, SvgPresetParams, Tag,
    TagEdit, TagFilter, TagSummary, TitleClip, Track,
};
use crate::svg::{check_params, render_preset, SvgDoc};

/// The longest a group's title (or card) may run (seconds).
pub const MAX_TAG_TITLE_SECONDS: f64 = 60.0;

/// How [`AssembleByTag`] titles each group with its value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupTitle {
    /// A lower-third title clip over the first seconds of the group's first clip (on a video track above it).
    #[default]
    Overlay,
    /// A title card: `title_seconds` of the cut, with the title, before the group.
    Card,
    /// No title.
    None,
}

/// Which way the assets of one group are ordered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortWithin {
    /// Oldest import first.
    #[default]
    Imported,
    /// By name, A to Z.
    Name,
}

fn default_title_seconds() -> f64 {
    3.0
}

/// A cut built from the library by the value of one tag key.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssembleByTag {
    /// The key to group by (`location`): one group per value.
    pub key: String,
    /// The order of the groups, by value (ignoring case). Values it does not list follow A to Z; the default is A to Z.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    /// Only assets matching every term are used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filter: Vec<TagFilter>,
    /// Assets with no value for `key` come last, in a group of their own with no title (default: left out).
    #[serde(default)]
    pub include_untagged: bool,
    /// How a group is titled with its value (default: `overlay`).
    #[serde(default)]
    pub title: GroupTitle,
    /// How long a title (a card) runs, 0 to 60 s (default 3).
    #[serde(default = "default_title_seconds")]
    pub title_seconds: f64,
    /// How a group's assets are ordered (default: `imported`).
    #[serde(default)]
    pub sort_within: SortWithin,
}

impl AssembleByTag {
    /// Group by `key` with everything else at its default.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            order: None,
            filter: Vec::new(),
            include_untagged: false,
            title: GroupTitle::default(),
            title_seconds: default_title_seconds(),
            sort_within: SortWithin::default(),
        }
    }
}

/// One group of the cut [`Project::assemble_by_tag`] laid out.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AssembledGroup {
    /// The value that names the group; `None` for the assets with no value for the key.
    pub value: Option<String>,
    /// Its assets, in the order they play.
    pub asset_ids: Vec<Uuid>,
    /// The group's title clip, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_clip_id: Option<Uuid>,
    /// Where the group begins on the timeline, seconds (a card's start, else its first clip's).
    pub start: f64,
    /// Where it ends.
    pub end: f64,
}

/// What [`Project::assemble_by_tag`] added.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AssembleSummary {
    pub groups: Vec<AssembledGroup>,
    /// The clips added from the library, in play order: one for each of the groups' `asset_ids` in turn.
    pub clip_ids: Vec<Uuid>,
    /// Values of `order` that no asset carries.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unmatched: Vec<String>,
}

#[derive(Debug, Clone)]
struct PlannedGroup {
    value: Option<String>,
    assets: Vec<Asset>,
}

/// What [`Project::assemble_by_tag_inputs`] resolved under the project lock: the groups, in order, and what their
/// titles are drawn for. [`AssemblePlan::prepare`] reads the titles' artwork with the lock released.
#[derive(Debug, Clone)]
pub struct AssemblePlan {
    key: String,
    title: GroupTitle,
    title_seconds: f64,
    frame: (u32, u32),
    groups: Vec<PlannedGroup>,
    unmatched: Vec<String>,
}

#[derive(Debug, Clone)]
struct PreparedTitle {
    doc: SvgDoc,
    name: String,
    seconds: f64,
}

/// An [`AssemblePlan`] with its titles drawn and read; [`Project::apply_assemble_by_tag`] only places them.
#[derive(Debug, Clone)]
pub struct PreparedAssemble {
    plan: AssemblePlan,
    titles: Vec<Option<PreparedTitle>>,
}

impl AssemblePlan {
    /// Draw and read each group's title: the sanitiser runs here, not under the project lock.
    pub fn prepare(self) -> Result<PreparedAssemble> {
        let kind = match self.title {
            GroupTitle::Card => SvgPresetKind::FadeUp,
            _ => SvgPresetKind::LowerThird,
        };
        let titles = self
            .groups
            .iter()
            .map(|group| {
                let (Some(value), true) = (&group.value, self.title != GroupTitle::None) else {
                    return Ok(None);
                };
                let seconds = match self.title {
                    GroupTitle::Overlay => self.title_seconds.min(group.assets[0].duration),
                    _ => self.title_seconds,
                };
                let params = SvgPresetParams::new(value.clone());
                check_params(&params)?;
                let doc = SvgDoc::parse(&render_preset(kind, &params, seconds, self.frame))?;
                Ok(Some(PreparedTitle {
                    doc,
                    name: value.clone(),
                    seconds,
                }))
            })
            .collect::<Result<_>>()?;
        Ok(PreparedAssemble { plan: self, titles })
    }
}

/// How a tag change reads in a review line: `Tag clip.mp4: +location: Cumbuco, -jump`.
fn tag_change_summary(name: &str, from: &[Tag], to: &[Tag]) -> String {
    let parts: Vec<String> = to
        .iter()
        .filter(|t| !from.contains(t))
        .map(|t| format!("+{t}"))
        .chain(from.iter().filter(|t| !to.contains(t)).map(|t| format!("-{t}")))
        .collect();
    match parts.len() {
        0 => format!("Tag {name}"),
        1..=4 => format!("Tag {name}: {}", parts.join(", ")),
        n => format!("Tag {name}: {}, +{} more", parts[..4].join(", "), n - 4),
    }
}

impl Project {
    /// Add, take off or replace tags on assets, all in one transaction: an unknown asset, or an edit that does not
    /// apply to one of them (an invalid tag, past 64 tags), refuses the lot and writes nothing. Returns the assets
    /// as the caller sees them. Not an edit of the cut, so no revision.
    ///
    /// As the agent with review on (or a proposal open) the new tag lists are held back with the proposal as
    /// [`StagedAssetChange`]s — `asset_tags:<asset id>`, each the asset's whole tag list — and land when it is
    /// accepted, replacing the asset's tags as they are then; the agent's own reads see them meanwhile.
    pub fn edit_asset_tags(&self, asset_ids: &[Uuid], edit: &TagEdit) -> Result<Vec<Asset>> {
        if asset_ids.is_empty() {
            return Err(Error::InvalidArgument("name at least one asset to tag".to_string()));
        }
        if edit.is_empty() {
            return Err(Error::InvalidArgument(
                "nothing to change: give tags to add or remove".to_string(),
            ));
        }
        let mut ids: Vec<Uuid> = Vec::with_capacity(asset_ids.len());
        for id in asset_ids {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }
        let planned = ids
            .into_iter()
            .map(|id| {
                let asset = self.require_asset(id)?;
                let tags = edit.apply(&asset.tags)?;
                Ok((asset, tags))
            })
            .collect::<Result<Vec<_>>>()?;
        if self.actor == EditSource::Agent && (self.staged_exists()? || self.agent_review.get()) {
            return self.stage_asset_tags(planned);
        }
        let tx = self.tx()?;
        let mut out = Vec::with_capacity(planned.len());
        for (mut asset, tags) in planned {
            if asset.tags != tags {
                self.conn.execute(
                    "UPDATE assets SET tags = ?1 WHERE id = ?2",
                    rusqlite::params![super::tags_column(&tags)?, asset.id.to_string()],
                )?;
                asset.tags = tags;
            }
            out.push(asset);
        }
        tx.commit()?;
        Ok(out)
    }

    /// Hold the tag lists back with the agent's proposal, one step for all of them.
    fn stage_asset_tags(&self, planned: Vec<(Asset, Vec<Tag>)>) -> Result<Vec<Asset>> {
        let mut changes = Vec::with_capacity(planned.len());
        let mut out = Vec::with_capacity(planned.len());
        let mut label = None;
        for (mut asset, tags) in planned {
            let stored = self.stored_asset(asset.id)?.ok_or(Error::AssetNotFound(asset.id))?;
            let id = StagedAssetChange::tags_id(asset.id);
            let summary = tag_change_summary(&asset.name, &stored.tags, &tags);
            let change = (stored.tags != tags).then(|| StagedAssetChange {
                id: id.clone(),
                asset_id: asset.id,
                projection: None,
                tags: Some(tags.clone()),
                summary: summary.clone(),
            });
            label = Some(summary);
            changes.push((id, change));
            asset.tags = tags;
            out.push(asset);
        }
        let label = match (label, out.len()) {
            (Some(one), 1) => one,
            _ => format!("Tag {} assets", out.len()),
        };
        self.stage_asset_changes(label, changes)?;
        Ok(out)
    }

    /// Every tag in use across the library, for autocomplete and for choosing a key to group by. The agent's
    /// view includes the tags its proposal holds back.
    pub fn list_tags(&self) -> Result<TagSummary> {
        let assets = self.list_assets()?;
        Ok(TagSummary::of(assets.iter().map(|a| a.tags.as_slice())))
    }

    /// The assets matching every term of `filter` (as [`Project::list_assets`] shows them); an invalid term is refused.
    pub fn list_assets_tagged(&self, filter: &[TagFilter]) -> Result<Vec<Asset>> {
        let filter = filter.iter().map(TagFilter::checked).collect::<Result<Vec<_>>>()?;
        Ok(self
            .list_assets()?
            .into_iter()
            .filter(|a| tags_match(&filter, &a.tags))
            .collect())
    }

    /// The locked first step of [`Project::assemble_by_tag`]: which assets go in which group, in order, as the
    /// caller sees the library (an agent's pending tags included), and the frame the titles are drawn for. Refuses
    /// a request no asset answers. Reads the database only.
    pub fn assemble_by_tag_inputs(&self, req: &AssembleByTag) -> Result<AssemblePlan> {
        let seconds = req.title_seconds;
        if !(seconds.is_finite() && seconds > 0.0 && seconds <= MAX_TAG_TITLE_SECONDS) {
            return Err(Error::InvalidArgument(format!(
                "title_seconds is a number of seconds above 0 and at most {MAX_TAG_TITLE_SECONDS}"
            )));
        }
        let filter = req.filter.iter().map(TagFilter::checked).collect::<Result<Vec<_>>>()?;
        let mut assets: Vec<Asset> = self
            .list_assets()?
            .into_iter()
            .filter(|a| a.primary_kind() != StreamKind::Data && a.duration > 0.0 && tags_match(&filter, &a.tags))
            .collect();
        match req.sort_within {
            SortWithin::Imported => assets.sort_by_key(|a| (a.imported_at, a.name.to_lowercase(), a.id)),
            SortWithin::Name => assets.sort_by_key(|a| (a.name.to_lowercase(), a.imported_at, a.id)),
        }
        let tags: Vec<&[Tag]> = assets.iter().map(|a| a.tags.as_slice()).collect();
        let grouping = group_by_key(
            &tags,
            &req.key,
            req.order.as_deref().unwrap_or_default(),
            req.include_untagged,
        )?;
        if grouping.groups.is_empty() {
            return Err(Error::InvalidArgument(format!(
                "no asset has a \"{}\" tag{}",
                req.key.trim(),
                if filter.is_empty() { "" } else { " and matches the filter" }
            )));
        }
        let groups = grouping
            .groups
            .into_iter()
            .map(|g| PlannedGroup {
                value: g.value,
                assets: g.members.into_iter().map(|i| assets[i].clone()).collect(),
            })
            .collect();
        Ok(AssemblePlan {
            key: req.key.trim().to_lowercase(),
            title: req.title,
            title_seconds: seconds,
            frame: self.working_timeline()?.preset_frame(),
            groups,
            unmatched: grouping.unmatched,
        })
    }

    /// The locked last step of [`Project::assemble_by_tag`]: append the groups to the end of the cut as **one**
    /// `Assemble by <key>` edit (staged for an agent, rippled and sync-locked like any), whole assets with their
    /// own sound as [`Project::concatenate`] lays them. The first clip starts at the end of the tracks the assets
    /// land on; a group's clips follow one another across tracks. A title is a title clip: a card on the first
    /// video track free over its span, an overlay on the first one free over the group's first clip (a new track
    /// above when none is).
    pub fn apply_assemble_by_tag(&self, prepared: PreparedAssemble) -> Result<AssembleSummary> {
        let PreparedAssemble { plan, titles } = prepared;
        let mut groups = Vec::with_capacity(plan.groups.len());
        for group in &plan.groups {
            let assets = group
                .assets
                .iter()
                .map(|a| self.require_asset(a.id))
                .collect::<Result<Vec<_>>>()?;
            groups.push(PlannedGroup {
                value: group.value.clone(),
                assets,
            });
        }
        let label = |done: &AssembleSummary| {
            format!(
                "Assemble by {} ({} group{}, {} clip{})",
                plan.key,
                done.groups.len(),
                if done.groups.len() == 1 { "" } else { "s" },
                done.clip_ids.len(),
                if done.clip_ids.len() == 1 { "" } else { "s" }
            )
        };
        self.edit_named(&[], label, |timeline| {
            let mut targets = Vec::new();
            for asset in groups.iter().flat_map(|g| &g.assets) {
                targets.push(default_track(timeline, asset.primary_kind())?);
            }
            let mut cursor = targets
                .iter()
                .filter_map(|id| timeline.track(*id))
                .map(Track::end)
                .fold(0.0, f64::max);
            let mut summary = AssembleSummary {
                groups: Vec::with_capacity(groups.len()),
                clip_ids: Vec::new(),
                unmatched: plan.unmatched.clone(),
            };
            for (group, title) in groups.iter().zip(&titles) {
                let start = cursor;
                let mut title_clip_id = None;
                let make = |t: &PreparedTitle, at: f64| {
                    let clip = Clip::title(
                        TitleClip {
                            name: t.name.clone(),
                            svg: t.doc.sanitized().to_string(),
                            template: None,
                        },
                        at,
                        t.seconds,
                    );
                    (clip.id, clip)
                };
                if let (Some(t), GroupTitle::Card) = (title, plan.title) {
                    check_title_room(timeline)?;
                    let (id, clip) = make(t, cursor);
                    place(timeline, None, clip)?;
                    title_clip_id = Some(id);
                    cursor += t.seconds;
                }
                let first = cursor;
                for asset in &group.assets {
                    let tid = default_track(timeline, asset.primary_kind())?;
                    let clip = Clip::for_asset(asset, 0.0, asset.duration, cursor);
                    cursor = clip.timeline_end();
                    summary.clip_ids.push(clip.id);
                    timeline.track_mut(tid).ok_or(Error::TrackNotFound(tid))?.clips.push(clip);
                }
                if let (Some(t), GroupTitle::Overlay) = (title, plan.title) {
                    check_title_room(timeline)?;
                    let (id, clip) = make(t, first);
                    place(timeline, None, clip)?;
                    title_clip_id = Some(id);
                }
                summary.groups.push(AssembledGroup {
                    value: group.value.clone(),
                    asset_ids: group.assets.iter().map(|a| a.id).collect(),
                    title_clip_id,
                    start,
                    end: cursor,
                });
            }
            Ok(summary)
        })
    }

    /// Lay the library out as a cut grouped by the value of one tag key, a title per group (the three steps
    /// above in a row — **a surface runs them separately, so the titles are read with the project lock
    /// released**). Appends at the end of the cut as one revision; an error if no asset matches.
    pub fn assemble_by_tag(&self, req: AssembleByTag) -> Result<AssembleSummary> {
        let prepared = self.assemble_by_tag_inputs(&req)?.prepare()?;
        self.apply_assemble_by_tag(prepared)
    }
}
