//! Review and history: staged agent edits, revisions, undo and redo.

use super::*;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct StageEditsParams {
    #[schemars(description = "One line describing what you are about to propose; it becomes the label of the applied revision")]
    note: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ApplyStagedParams {
    #[schemars(
        description = "Apply even though the user has edited the timeline since these edits were staged, replacing their newer cut. Default false."
    )]
    force: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ApplySubsetParams {
    #[schemars(
        description = "The `id`s of the staged_diff entries to apply (for example `clip_added:<clip id>`); the rest stay in the proposal"
    )]
    entry_ids: Vec<String>,
    #[schemars(description = "Apply even though the user has edited the timeline since these edits were staged. Default false.")]
    force: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RevisionDiffParams {
    #[schemars(description = "Revision seq to explain (see history); the diff is from the revision before it")]
    seq: i64,
    #[schemars(description = "Compare against this revision seq instead of `seq - 1`")]
    from: Option<i64>,
    #[schemars(description = "Skip this many changes (default 0), to page on; `entries.next_offset` is the next page")]
    offset: Option<usize>,
    #[schemars(description = "Most changes to return (default 100, at most 500)")]
    limit: Option<usize>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct DiffPageParams {
    #[schemars(description = "Skip this many changes (default 0), to page on; `diff.entries.next_offset` is the next page")]
    offset: Option<usize>,
    #[schemars(description = "Most changes to return (default 100, at most 500)")]
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RevertParams {
    #[schemars(description = "Revision seq to jump the timeline back to (see history)")]
    seq: i64,
}

#[tool_router(router = tool_router_review, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Begin a staged edit: from here on your edits are held back as a proposal the user reviews \
                       instead of changing the cut they are looking at. Your own reads (get_timeline_state, \
                       preview_timeline, timeline_summary, export) follow the proposal, so you can check your work \
                       before handing it over. While the user has review switched on (the default) claim_next_task \
                       and your first edit open the proposal by themselves, so you rarely need this. With review \
                       off your edits, task work included, land on the cut as you make them; call this to hold a \
                       change back for the user to look at first. The proposal belongs to your session: another \
                       agent connected at the same time is refused while it is open."
    )]
    fn stage_edits(&self, Parameters(p): Parameters<StageEditsParams>) -> Result<String, McpError> {
        let owner = self.owner();
        self.act(|project| {
            let staged = project
                .begin_staging(None, p.note.as_deref(), owner.as_deref())
                .map_err(core_err)?;
            json(&reads::staged_brief(&staged))
        })
    }

    #[tool(
        description = "What your staged edits would change about the user's cut: `diff.entries` — every add, cut, move, \
                       retrim and adjustment (each has an `id`), a page at a time (`limit` default 100, at most 500, from \
                       `offset`; `diff.entries.next_offset` pages on) — with the runtime and clip count before and after, \
                       and `staged` (the headline, your note, `undo_depth` — how many steps undo_staged_edit can take back \
                       — and whether it is `stale`) and any held-back library changes under `asset_changes`. Returns null \
                       when nothing is staged."
    )]
    fn staged_diff(&self, Parameters(p): Parameters<DiffPageParams>) -> Result<String, McpError> {
        let project = self.lock();
        match project.staged().map_err(core_err)? {
            None => json(&serde_json::json!(null)),
            Some(staged) => json(&serde_json::json!({
                "staged": reads::staged_brief(&staged),
                "asset_changes": staged.asset_changes,
                "diff": reads::diff_page(&staged.diff, p.offset, p.limit),
            })),
        }
    }

    #[tool(
        description = "Apply your staged edits to the live timeline, as one revision — ONLY while the user has agent \
                       review switched off (Settings › Agent). With review on (the default) this is refused: the user \
                       applies agent edits in Kerf, so finish with complete_task (or just tell them the proposal is \
                       ready) and ask them to review it; `force` does not get round that. With review off there is \
                       only a proposal to apply when you opened one with stage_edits (or one was open when the user \
                       switched review off); other edits have already landed. It fails if the user has edited the \
                       timeline since you staged, unless `force`. Returns a summary of the timeline afterwards \
                       (get_timeline_state has all of it)."
    )]
    fn apply_staged_edits(&self, Parameters(p): Parameters<ApplyStagedParams>) -> Result<String, McpError> {
        self.act(|project| {
            let timeline = project.apply_staged(p.force.unwrap_or(false)).map_err(core_err)?;
            json(&reads::timeline_brief(&timeline))
        })
    }

    #[tool(
        description = "Throw your staged edits away, leaving the user's timeline untouched. The proposal is kept aside, so \
                       restore_discarded_edits can bring it back if this was a mistake (the user can also restore it in \
                       Kerf). Returns a summary of the user's timeline."
    )]
    fn discard_staged_edits(&self) -> Result<String, McpError> {
        self.act(|project| {
            let timeline = project.discard_staged().map_err(core_err)?;
            json(&reads::timeline_brief(&timeline))
        })
    }

    #[tool(
        description = "Take back the last step of your staged edits (the proposal remembers its last 20), leaving the \
                       user's timeline alone. This is how to undo a mistake you made in the proposal; undo works on the \
                       live cut instead. Returns the label of the step taken back and how the proposal stands."
    )]
    fn undo_staged_edit(&self) -> Result<String, McpError> {
        self.act(|project| {
            let (label, staged) = project.undo_staged().map_err(core_err)?;
            json(&serde_json::json!({ "undone": label, "staged": reads::staged_brief(&staged) }))
        })
    }

    #[tool(
        description = "Bring back the proposal YOU last discarded (discard_staged_edits), once, and only while no proposal \
                       is open. It returns as it was, without its undo history, and may be `stale` if the user edited the \
                       timeline since. A proposal the user discarded is theirs to bring back, in Kerf: this refuses it."
    )]
    fn restore_discarded_edits(&self) -> Result<String, McpError> {
        self.act(|project| {
            let staged = project.restore_discarded().map_err(core_err)?;
            json(&reads::staged_brief(&staged))
        })
    }

    #[tool(
        description = "Apply only some of your staged changes (the `id`s of staged_diff's entries) to the live timeline, \
                       leaving the rest in the proposal — ONLY while agent review is switched off; with it on (the \
                       default) the user picks which changes to accept in Kerf and this is refused. A linked partner of a \
                       picked clip comes with it (the answer's `auto_included` says so); a pick that would collide with \
                       other clips or break a link is refused with where. Fails if the timeline moved on since you staged, \
                       unless `force`."
    )]
    fn apply_staged_subset(&self, Parameters(p): Parameters<ApplySubsetParams>) -> Result<String, McpError> {
        if p.entry_ids.is_empty() {
            return Err(McpError::invalid_params(
                "`entry_ids` must name at least one change (the `id`s in staged_diff)",
                None,
            ));
        }
        self.act(|project| {
            let applied = project
                .apply_staged_subset(&p.entry_ids, p.force.unwrap_or(false))
                .map_err(core_err)?;
            json(&serde_json::json!({
                "message": applied.message,
                "applied": applied.applied,
                "auto_included": applied.auto_included,
                "skipped": applied.skipped,
                "remaining": applied.remaining,
                "closed": applied.closed,
                "timeline": reads::timeline_brief(&applied.timeline),
            }))
        })
    }

    #[tool(
        description = "Explain what one revision changed (see history), as a list of edits to the cut: `diff.entries` a \
                       page at a time (`limit` default 100, at most 500, from `offset`; `diff.entries.next_offset` pages on), \
                       with the headline and the runtime and clip count before and after."
    )]
    fn revision_diff(&self, Parameters(p): Parameters<RevisionDiffParams>) -> Result<String, McpError> {
        let project = self.lock();
        let diff = match p.from {
            Some(from) => project.diff_revisions(from, p.seq).map_err(core_err)?,
            None => project.revision_diff(p.seq).map_err(core_err)?,
        };
        json(&serde_json::json!({ "diff": reads::diff_page(&diff, p.offset, p.limit) }))
    }

    #[tool(
        description = "List the timeline edit history, newest last: `revisions` (each with its `seq` — higher is newer — a \
                       label, who made it, and `current` on the one the timeline is at) and the `total` count. Returns \
                       the newest `limit` revisions (default 50, at most 500) after skipping `offset` of the newest; \
                       `next_offset` pages back through older ones."
    )]
    fn history(&self, Parameters(p): Parameters<PageParams>) -> Result<String, McpError> {
        let (offset, limit) = reads::page_of(p.offset, p.limit, 50, 500);
        let (revisions, total) = self
            .lock()
            .history_page(kerf_core::Page {
                limit: Some(limit),
                offset,
            })
            .map_err(core_err)?;
        let end = offset + revisions.len();
        json(&serde_json::json!({
            "total": total,
            "offset": offset,
            "returned": revisions.len(),
            "next_offset": (end < total).then_some(end),
            "revisions": revisions,
        }))
    }

    #[tool(
        description = "Undo the last edit to the user's LIVE timeline, returning a summary of the restored timeline. Only \
                       while agent review is off: with review on (the default) the live cut is the user's — take back your \
                       own staged work with undo_staged_edit instead."
    )]
    fn undo(&self) -> Result<String, McpError> {
        self.refuse_live_history()?;
        self.act(|project| {
            let out = project.undo().map_err(core_err)?;
            json(&reads::timeline_brief(&out))
        })
    }

    #[tool(
        description = "Redo the next edit on the user's LIVE timeline, returning a summary of the restored timeline. Only \
                       while agent review is off (see undo)."
    )]
    fn redo(&self) -> Result<String, McpError> {
        self.refuse_live_history()?;
        self.act(|project| {
            let out = project.redo().map_err(core_err)?;
            json(&reads::timeline_brief(&out))
        })
    }

    #[tool(
        description = "Revert the user's LIVE timeline to a specific revision seq (see history), returning a summary of the \
                       restored timeline. Only while agent review is off (see undo)."
    )]
    fn revert_to(&self, Parameters(p): Parameters<RevertParams>) -> Result<String, McpError> {
        self.refuse_live_history()?;
        self.act(|project| {
            let out = project.revert_to(p.seq).map_err(core_err)?;
            json(&reads::timeline_brief(&out))
        })
    }
}
