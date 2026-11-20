//! Importing media and describing it: import, projection, tags (and assembling by tag) and preview proxies.

use super::*;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ImportParams {
    #[schemars(description = "Absolute path to the media file to import")]
    path: String,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result (or subscribe to kerf://jobs/{job_id}), stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct RebuildProxyParams {
    #[schemars(description = "UUID of the asset")]
    pub(super) asset_id: String,
    #[schemars(
        description = "true: put the asset's proxy first in the queue instead of rebuilding it, and keep what there is \
                       (a proxy that is ready is left alone, a build under way keeps going). Default false: build \
                       it again from the original"
    )]
    pub(super) next: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AssetProjectionParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(
        description = "The source's spherical projection: \"equirect\", \"dual_fisheye\" or \"fisheye\"; null clears \
                       the mark and treats the asset as ordinary flat footage"
    )]
    projection: Option<Projection>,
}

/// A tag as `edit_asset_tags` takes it. [`Tag`] itself reads what it knows and drops the rest, so a misspelt
/// `{"keys":"location","value":"Cumbuco"}` would have become the plain tag `Cumbuco`; this refuses the field.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct TagArg {
    pub(super) key: Option<String>,
    pub(super) value: String,
}

impl From<TagArg> for Tag {
    fn from(arg: TagArg) -> Self {
        Tag {
            key: arg.key,
            value: arg.value,
        }
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct EditAssetTagsParams {
    #[schemars(description = "UUIDs of the assets to change: one, or many to tag a whole batch in one call")]
    pub(super) asset_ids: Vec<String>,
    #[schemars(
        description = "Tags to put on, each {\"key\":\"location\",\"value\":\"Cumbuco\"} (shown `location: Cumbuco`) or, with no \
                       key, {\"value\":\"jump\"}. Whitespace is tidied, a key is lowercased, a tag already there (ignoring \
                       case) is left as it is. A key is at most 32 characters, a value 64, an asset holds 64 tags"
    )]
    pub(super) add: Option<Vec<TagArg>>,
    #[schemars(
        description = "Tags to take off, spelled as in `add` (matched ignoring case; a plain tag only takes off a plain tag, \
                       `jump` leaves `style: jump`). One that is not there is ignored"
    )]
    pub(super) remove: Option<Vec<TagArg>>,
    #[schemars(description = "Keys to take every tag of off, e.g. [\"location\"] clears all the locations of an asset")]
    pub(super) remove_keys: Option<Vec<String>>,
}

#[tool_router(router = tool_router_library, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "Import a media file into the project and return the Asset (probed streams, duration, \
                       resolution, fps, codecs). `path` is the absolute path of a media file on this computer, or one \
                       relative to the workspace when the server is confined to one (a URL, an ffmpeg protocol such as \
                       concat: or pipe:, a network share (UNC path) or a missing file is refused at once). Importing a file the project already holds resolves to that \
                       asset instead of duplicating it, so this is safe to re-run. One Insta360 lens file pulls \
                       in its sibling and stitches the pair into a single 360 asset — a full re-encode that can \
                       run for minutes (send a `progressToken` for progress, cancel the request to stop it, or pass \
                       `background: true` to run it as a background job). An import is NOT part of your staged proposal: media lands for the user \
                       immediately, because a file on disk is not an edit to their cut."
    )]
    async fn import_asset(
        &self,
        Parameters(p): Parameters<ImportParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        // Absolute, an existing regular file, no URL, ffmpeg protocol or network share: said now, on
        // the call, not by a background job a moment later. The network check goes first, before any
        // stat: opening a UNC path makes this machine authenticate to the host it names.
        let path = self.path_arg(&p.path)?;
        crate::require_local_input_path(&path).map_err(|e| McpError::invalid_params(e, None))?;
        kerf_core::paths::validate_import_path(std::path::Path::new(&path)).map_err(core_err)?;
        // Confined to a workspace: no playlist, manifest or lens pair that reads outside it. It probes,
        // so on the blocking pool.
        if let Some(ws) = self.workspace.clone() {
            let path = path.clone();
            blocking(move || ws.check_media(&path).map_err(|e| McpError::invalid_params(e, None))).await?;
        }
        let (me, owner, generation) = (self.clone(), self.owner(), self.lock().generation());
        self.long_tool("import_asset", p.background, &context, move |run| {
            me.import_run(path, run, owner, generation)
        })
        .await
    }

    #[tool(
        description = "The preview proxies and the queue they are built in. `proxies` has the proxy of one asset \
                       (asset_id) or of every asset: `state` is ready, building (with `fraction` 0..1 and \
                       `eta_secs`), queued, failed (with the `reason`), missing, off (proxies are off, previews use \
                       the original, or the user deleted this one) or not_needed (a still or an audio-only file). \
                       `queue` is the line the builds wait in: `jobs` in the order they run (`position` 1 is the one \
                       building; only the first 20 are listed, `remaining` counts all of them) each with `eta_secs`, \
                       the seconds until that one is done once Kerf has timed a build; the total `eta_secs`; `paused` \
                       (the user stopped it: the build under way finishes, nothing new starts); `waiting_on` (an \
                       export or analysis has the machine, so the next build waits for it); and `failed`. Previews \
                       decode the original until the proxy is ready; export always reads the original. A proxy is \
                       built before any analysis waiting for the machine; rebuild_proxy with `next: true` moves one \
                       asset to the front."
    )]
    pub(super) async fn proxy_status(&self, Parameters(p): Parameters<ProxyStatusParams>) -> Result<String, McpError> {
        let wanted = p.asset_id.as_deref().map(|s| parse_arg("asset_id", s)).transpose()?;
        let project = self.project.clone();
        let answer = blocking(move || {
            let mut inputs = lock_agent(&project).proxy_inputs().map_err(core_err)?;
            // The whole queue, read with the lock released and before the statuses (which touch the disk).
            let queue = kerf_core::proxy::queue(&inputs);
            if let Some(id) = wanted {
                inputs.retain(|i| i.asset.id == id);
                if inputs.is_empty() {
                    return Err(core_err(kerf_core::Error::AssetNotFound(id)));
                }
            }
            Ok(reads::ProxyStatusAnswer {
                proxies: kerf_core::proxy::statuses(&inputs),
                queue: reads::QueueBrief::of(queue, wanted),
            })
        })
        .await?;
        json(&answer)
    }

    #[tool(
        description = "Build an asset's preview proxy again from the original: the current one (and any build in \
                       progress) is dropped first, and a proxy the user had deleted is wanted again. Use it when \
                       previews of an asset look wrong or stale. With `next: true` it instead puts the asset's proxy \
                       first in the queue and keeps what there is (a ready proxy is left alone, a build under way \
                       keeps going): use it to get the media you are about to look at ready sooner. Returns where \
                       the build starts; poll proxy_status for its progress and the queue. Not part of your staged \
                       proposal — a proxy is a cache, not an edit."
    )]
    pub(super) async fn rebuild_proxy(&self, Parameters(p): Parameters<RebuildProxyParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let next = p.next.unwrap_or(false);
        let (project, app) = (self.project.clone(), self.app.clone());
        let status = blocking(move || {
            let lock = || lock_agent(&project);
            if next {
                crate::proxy_queue::prioritize_proxy_now(&app, &lock, id)
            } else {
                crate::rebuild_proxy_now(&app, &lock, id)
            }
            .map_err(|e| McpError::invalid_params(e, None))
        })
        .await?;
        json(&status)
    }

    #[tool(
        description = "Delete an asset's preview proxy files (at every size) and stop any build of them, freeing \
                       the disk. Previews decode the original until it is rebuilt (rebuild_proxy), and it is not \
                       built again by itself on a later open. Returns the bytes freed and the asset's new status."
    )]
    async fn delete_proxy(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let (project, app) = (self.project.clone(), self.app.clone());
        let (freed, status) = blocking(move || {
            crate::delete_proxy_now(&app, &|| lock_agent(&project), id).map_err(|e| McpError::invalid_params(e, None))
        })
        .await?;
        json(&serde_json::json!({ "bytes_freed": freed, "proxy": status }))
    }

    #[tool(
        description = "Mark an asset as 360 footage Kerf did not detect as such (or pass null to unmark it). \
                       Detection only trusts spherical metadata or a telltale capture layout, so a stitched \
                       equirect export that lost its metadata imports as flat video. Unlike set_reframe this is \
                       remembered on the asset, so every clip cut from it afterwards is reframed automatically."
    )]
    async fn set_asset_projection(&self, Parameters(p): Parameters<AssetProjectionParams>) -> Result<String, McpError> {
        let asset_id = parse_arg("asset_id", &p.asset_id)?;
        let (project, app, owner) = (self.project.clone(), self.app.clone(), self.owner());
        // On the blocking pool: queueing the proxy at the new size runs the cached ffprobe that
        // keys it, which must not stall an async worker. The change to the asset is held back
        // with the agent's proposal (the core does that, for the session named here).
        let asset = blocking(move || {
            let asset = lock_agent_as(&project, owner.as_deref())
                .set_asset_projection(asset_id, p.projection)
                .map_err(core_err)?;
            crate::spawn_proxy(&app, &asset);
            Ok(asset)
        })
        .await?;
        self.changed();
        json(&asset)
    }

    #[tool(
        description = "Tag whole assets so they can be found and cut together. A tag is a key and a value, \
                       {\"key\":\"location\",\"value\":\"Cumbuco\"} (`location: Cumbuco`), or just a value, \
                       {\"value\":\"jump\"} (a plain tag); a key is lowercased, tags compare ignoring case, and an asset \
                       may hold several values of one key (two riders). One call changes many assets the same way: \
                       `remove_keys` first, then `remove`, then `add`, so replacing a location is remove_keys \
                       [\"location\"] plus add [{\"key\":\"location\",\"value\":\"Jeri\"}]. list_tags shows what is in \
                       use (reuse its spelling) and list_assets shows and filters by tags; assemble_by_tag builds a cut \
                       grouped by a key. A tag is the user's and yours alike: while review is on your change is held \
                       back with your proposal for them to accept (list_assets and list_tags show it meanwhile; it is \
                       an `asset_tags:<id>` line in staged_diff's `asset_changes`), with review off it lands at once. An \
                       asset that does not exist, or a tag that is empty or too long, refuses the whole call and \
                       changes nothing. Returns each asset's id, name and tags."
    )]
    pub(super) fn edit_asset_tags(&self, Parameters(p): Parameters<EditAssetTagsParams>) -> Result<String, McpError> {
        let ids = p
            .asset_ids
            .iter()
            .map(|s| parse_arg("asset_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        let edit = TagEdit {
            add: p.add.unwrap_or_default().into_iter().map(Tag::from).collect(),
            remove: p.remove.unwrap_or_default().into_iter().map(Tag::from).collect(),
            remove_keys: p.remove_keys.unwrap_or_default(),
        };
        self.edit(|project| {
            let assets = project.edit_asset_tags(&ids, &edit).map_err(core_err)?;
            let assets: Vec<_> = assets
                .iter()
                .map(|a| serde_json::json!({ "id": a.id, "name": a.name, "tags": reads::tag_texts(&a.tags) }))
                .collect();
            json(&serde_json::json!({ "assets": assets }))
        })
    }

    #[tool(
        description = "Build a cut from the library grouped by a tag: one section per value of `key`, appended to the end \
                       of the cut as ONE edit. `key` \"location\" with assets tagged `location: Cumbuco` / `location: Jeri` \
                       makes a Cumbuco section then a Jeri section, each with its value as a title. Whole assets, picture \
                       and sound, laid down as concatenate does (video on the first unlocked video track, audio-only \
                       media on the first unlocked audio track); an asset with several values of the key goes in the first \
                       one in order. Sections run A to Z unless `order` lists values first (the rest follow A to Z; an \
                       entry no asset has comes back in `unmatched`). `filter` keeps only assets matching every term \
                       (the `tags` terms of list_assets, e.g. [{\"key\":\"rider\",\"value\":\"Orell\"}]); \
                       `include_untagged` puts assets without the key last, in a section with no title. `title`: \"overlay\" \
                       (default) a lower-third title clip over the first `title_seconds` (default 3, at most 60) of the section, on a video track \
                       above; \"card\" a title card of `title_seconds` before the section; \"none\". `sort_within` orders \
                       a section's assets \"imported\" (default) or by \"name\". The titles are ordinary title clips \
                       (restyle them with update_title_clip, set_transition, ...). Tag first (edit_asset_tags; list_tags \
                       shows the keys and values), and use it on an empty cut or to extend one: it always appends. \
                       Returns `groups` (value, asset_ids, title_clip_id, start and end in timeline seconds), `clip_ids` and \
                       `unmatched`; an error when no asset matches."
    )]
    pub(super) async fn assemble_by_tag(&self, Parameters(p): Parameters<kerf_core::AssembleByTag>) -> Result<String, McpError> {
        let (project, owner) = (self.project.clone(), self.owner());
        // The groups are decided under the lock as this session sees the library (its pending tags
        // included); the titles are drawn and read with it released.
        let prepared = blocking(move || {
            let plan = lock_agent_as(&project, owner.as_deref())
                .assemble_by_tag_inputs(&p)
                .map_err(core_err)?;
            plan.prepare().map_err(core_err)
        })
        .await?;
        self.edit(|project| json(&project.apply_assemble_by_tag(prepared).map_err(core_err)?))
    }
}
