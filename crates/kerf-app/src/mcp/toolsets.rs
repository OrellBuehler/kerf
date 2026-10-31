//! Tool sets: a client that does not want every tool's schema in its context connects with
//! `?tools=core,titles` on the MCP URL and is listed only those sets (and [`ESSENTIALS`]). No query,
//! an empty one or `all` lists everything, so a client set up before tool sets is unchanged.
//!
//! Each tool's set is the last column of its row in `hints::TOOLS`. A selection trims what
//! `tools/list` shows, not what may run: a call by name to a tool of another set still runs (the
//! bearer token is the permission), so an agent that follows the guide or a prompt into another
//! set is not stopped in a client that lets it call by name.

use serde::Serialize;

use super::hints::TOOLS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolSet {
    Core,
    Analysis,
    Clips,
    Keyframes,
    Titles,
    Audio,
    Music,
    Delivery,
    Tasks,
    Project,
}

/// Listed whatever the selection: the guide, the state of the cut, the proposal and a job.
pub(super) const ESSENTIALS: [&str; 4] = ["get_editing_guide", "timeline_summary", "staged_diff", "job_status"];

impl ToolSet {
    pub(super) const ALL: [ToolSet; 10] = [
        ToolSet::Core,
        ToolSet::Analysis,
        ToolSet::Clips,
        ToolSet::Keyframes,
        ToolSet::Titles,
        ToolSet::Audio,
        ToolSet::Music,
        ToolSet::Delivery,
        ToolSet::Tasks,
        ToolSet::Project,
    ];

    pub(super) fn name(self) -> &'static str {
        match self {
            ToolSet::Core => "core",
            ToolSet::Analysis => "analysis",
            ToolSet::Clips => "clips",
            ToolSet::Keyframes => "keyframes",
            ToolSet::Titles => "titles",
            ToolSet::Audio => "audio",
            ToolSet::Music => "music",
            ToolSet::Delivery => "delivery",
            ToolSet::Tasks => "tasks",
            ToolSet::Project => "project",
        }
    }

    fn about(self) -> &'static str {
        match self {
            ToolSet::Core => {
                "Look at the media and the cut, cut and arrange, volume / fades / transitions, review and history, jobs"
            }
            ToolSet::Analysis => "Transcripts, speakers, filler words, waveforms, scopes, speech model, proxies, audio sync",
            ToolSet::Clips => "Speed, transform, colour and grade, effects, masks, linked picture and sound",
            ToolSet::Keyframes => "Keyframes, per-property animation and 360 reframing",
            ToolSet::Titles => "Text overlays, SVG titles and templates, captions, markers, fonts",
            ToolSet::Audio => "Track and master mix, ducking, loudness and band balance, voiceover, sound effects",
            ToolSet::Music => "Music structure, fitting and composing music, stems, snapping cuts to beats",
            ToolSet::Delivery => "Delivery format, smart crop, export, variants, cover, captions and interchange files",
            ToolSet::Tasks => "The user's task queue",
            ToolSet::Project => "Open, create and save projects as, missing media, settings",
        }
    }

    fn parse(name: &str) -> Option<ToolSet> {
        ToolSet::ALL.into_iter().find(|set| set.name() == name)
    }

    pub(super) fn tools(self) -> impl Iterator<Item = &'static str> {
        TOOLS.iter().filter(move |row| row.3 == self).map(|row| row.0)
    }
}

/// The set that lists `tool`, `None` for a name that is no tool.
pub(super) fn set_of(tool: &str) -> Option<ToolSet> {
    TOOLS.iter().find(|row| row.0 == tool).map(|row| row.3)
}

/// The sets a connection asked for; `None` is every tool.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Selection(Option<Vec<ToolSet>>);

impl Selection {
    /// Read the `tools` parameter of a request URL's query (`tools=core,titles`; repeated
    /// parameters add up). A name that is no set is an error naming the sets there are.
    pub(super) fn from_query(query: Option<&str>) -> Result<Selection, String> {
        let mut sets = Vec::new();
        let values = query
            .unwrap_or("")
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .filter(|(key, _)| *key == "tools")
            .map(|(_, value)| value.replace("%2C", ",").replace("%2c", ","));
        for value in values {
            for name in value.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                if name == "all" {
                    return Ok(Selection(None));
                }
                let set = ToolSet::parse(name).ok_or_else(|| {
                    let known: Vec<&str> = ToolSet::ALL.iter().map(|s| s.name()).collect();
                    format!("`{name}` is no tool set; the sets are {} (or all)", known.join(", "))
                })?;
                if !sets.contains(&set) {
                    sets.push(set);
                }
            }
        }
        Ok(Selection((!sets.is_empty()).then_some(sets)))
    }

    /// Whether `tools/list` shows `tool` on this connection.
    pub(super) fn lists(&self, tool: &str) -> bool {
        match &self.0 {
            None => true,
            Some(sets) => ESSENTIALS.contains(&tool) || set_of(tool).is_some_and(|set| sets.contains(&set)),
        }
    }

    /// What the guide adds for a connection that lists only some sets; `None` for every tool.
    pub(super) fn note(&self) -> Option<String> {
        let sets = self.0.as_ref()?;
        let listed: Vec<&str> = sets.iter().map(|s| s.name()).collect();
        let others: Vec<&str> = ToolSet::ALL.iter().filter(|s| !sets.contains(s)).map(|s| s.name()).collect();
        Some(format!(
            "\n\nThis connection lists only the tool sets {}. A tool named above from another set ({}) is not \
             in your list; if you need it, ask the user to add its set to the `tools` parameter of Kerf's MCP URL.",
            listed.join(", "),
            others.join(", ")
        ))
    }
}

/// One tool set, for the Agent settings page and `mcp_connection_info`.
#[derive(Clone, Debug, Serialize)]
pub struct ToolSetInfo {
    pub name: &'static str,
    pub description: &'static str,
    /// How many tools it lists (the essentials come with any selection).
    pub tools: usize,
}

pub fn catalogue() -> Vec<ToolSetInfo> {
    ToolSet::ALL
        .into_iter()
        .map(|set| ToolSetInfo {
            name: set.name(),
            description: set.about(),
            tools: set.tools().count(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_query_or_all_lists_every_tool() {
        for query in [
            None,
            Some(""),
            Some("x=1"),
            Some("tools="),
            Some("tools=all"),
            Some("tools=core,all"),
        ] {
            let selection = Selection::from_query(query).unwrap();
            assert_eq!(selection, Selection(None), "{query:?}");
            assert!(TOOLS.iter().all(|row| selection.lists(row.0)));
            assert!(selection.note().is_none());
        }
    }

    #[test]
    fn a_selection_lists_its_sets_and_the_essentials() {
        let selection = Selection::from_query(Some("a=b&tools=core%2Ctitles&tools=music")).unwrap();
        assert_eq!(
            selection,
            Selection(Some(vec![ToolSet::Core, ToolSet::Titles, ToolSet::Music]))
        );
        assert!(selection.lists("trim") && selection.lists("add_svg_overlay") && selection.lists("fit_music"));
        assert!(!selection.lists("export") && !selection.lists("set_grade"));
        let only_titles = Selection::from_query(Some("tools=titles")).unwrap();
        for tool in ESSENTIALS {
            assert!(only_titles.lists(tool), "{tool}");
        }
        assert!(!only_titles.lists("trim"));
        let note = only_titles.note().unwrap();
        assert!(
            note.contains("tool sets titles.") && note.contains("core, analysis"),
            "{note}"
        );
    }

    #[test]
    fn an_unknown_set_is_refused_naming_the_sets() {
        let error = Selection::from_query(Some("tools=core,colour")).unwrap_err();
        assert!(
            error.contains("`colour`") && error.contains("core, analysis, clips"),
            "{error}"
        );
    }

    #[test]
    fn the_catalogue_counts_every_tool_once() {
        let sets = catalogue();
        assert_eq!(sets.len(), ToolSet::ALL.len());
        assert!(sets.iter().all(|s| s.tools > 0 && !s.description.is_empty()));
        assert_eq!(sets.iter().map(|s| s.tools).sum::<usize>(), TOOLS.len());
        for tool in ESSENTIALS {
            assert!(set_of(tool).is_some(), "{tool}");
        }
    }
}
