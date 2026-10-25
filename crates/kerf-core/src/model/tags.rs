//! Asset tags: a keyed (`location: Cumbuco`) or plain (`jump`) label on a whole asset, so a library can be
//! searched, and an agent can build a cut grouped by a key. Pure: spelling, edits, filters, the vocabulary and
//! the grouping live here; storing them, staging an agent's change and assembling the cut are `project/tags.rs`.
//!
//! A tag is compared case-insensitively and stored in the spelling it was first given. Keys are lowercased.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The longest key (characters).
pub const MAX_TAG_KEY: usize = 32;
/// The longest value (characters).
pub const MAX_TAG_VALUE: usize = 64;
/// The most tags one asset holds.
pub const MAX_ASSET_TAGS: usize = 64;

fn invalid(what: impl Into<String>) -> Error {
    Error::InvalidArgument(what.into())
}

/// `s` trimmed, its whitespace (tabs and line breaks too) collapsed to single spaces and any other control
/// character dropped.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A key as stored: cleaned, lowercased, 1..=[`MAX_TAG_KEY`] characters.
pub fn normalize_key(key: &str) -> Result<String> {
    let key = clean(key).to_lowercase();
    if key.is_empty() {
        return Err(invalid("a tag key cannot be empty"));
    }
    if key.chars().count() > MAX_TAG_KEY {
        return Err(invalid(format!("a tag key is at most {MAX_TAG_KEY} characters: \"{key}\"")));
    }
    Ok(key)
}

fn normalize_value(value: &str) -> Result<String> {
    let value = clean(value);
    if value.is_empty() {
        return Err(invalid("a tag value cannot be empty"));
    }
    if value.chars().count() > MAX_TAG_VALUE {
        return Err(invalid(format!(
            "a tag value is at most {MAX_TAG_VALUE} characters: \"{value}\""
        )));
    }
    Ok(value)
}

/// A label on an asset: an optional key and a value. `{"key": "location", "value": "Cumbuco"}` is
/// `location: Cumbuco`; `{"value": "jump"}` is the plain tag `jump`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Tag {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub value: String,
}

impl Tag {
    /// A tag from its parts, normalized; an empty or over-long key or value is refused.
    pub fn new(key: Option<&str>, value: &str) -> Result<Tag> {
        Ok(Tag {
            key: key.map(normalize_key).transpose()?,
            value: normalize_value(value)?,
        })
    }

    /// `location: Cumbuco` splits on the first `:` into a key and a value; with no `:` it is a plain tag.
    pub fn parse(text: &str) -> Result<Tag> {
        match text.split_once(':') {
            Some((key, value)) => Tag::new(Some(key), value),
            None => Tag::new(None, text),
        }
    }

    /// This tag as [`Tag::new`] would have made it.
    pub fn normalized(&self) -> Result<Tag> {
        Tag::new(self.key.as_deref(), &self.value)
    }

    /// What two spellings of one tag share: the key and value, lowercased.
    fn identity(&self) -> (Option<String>, String) {
        (self.key.as_ref().map(|k| k.to_lowercase()), self.value.to_lowercase())
    }

    /// Whether `other` is the same tag, ignoring case.
    pub fn same_as(&self, other: &Tag) -> bool {
        self.identity() == other.identity()
    }
}

impl std::fmt::Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.key {
            Some(key) => write!(f, "{key}: {}", self.value),
            None => f.write_str(&self.value),
        }
    }
}

/// `tags` without a repeat (ignoring case), each in the spelling it was first given.
pub fn dedupe_tags(tags: Vec<Tag>) -> Vec<Tag> {
    let mut seen = HashSet::new();
    tags.into_iter().filter(|t| seen.insert(t.identity())).collect()
}

/// Whether two tag lists hold the same tags, whatever their order or spelling.
pub fn same_tags(a: &[Tag], b: &[Tag]) -> bool {
    let set = |tags: &[Tag]| tags.iter().map(Tag::identity).collect::<HashSet<_>>();
    set(a) == set(b)
}

/// A change to an asset's tags, applied in the order `remove_keys`, `remove`, `add`: replacing a location is
/// `remove_keys: ["location"]` with `add: [location: X]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TagEdit {
    /// Tags to put on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<Tag>,
    /// Tags to take off (the same key and value, ignoring case; a plain tag only takes off a plain tag).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<Tag>,
    /// Keys to take every tag of off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove_keys: Vec<String>,
}

impl TagEdit {
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty() && self.remove_keys.is_empty()
    }

    /// `tags` after the edit: repeats gone (the first spelling stays), at most [`MAX_ASSET_TAGS`]. Everything
    /// the edit names is normalized first, and an invalid tag or key refuses the whole edit.
    pub fn apply(&self, tags: &[Tag]) -> Result<Vec<Tag>> {
        let keys = self
            .remove_keys
            .iter()
            .map(|k| normalize_key(k))
            .collect::<Result<HashSet<_>>>()?;
        let remove = self
            .remove
            .iter()
            .map(|t| t.normalized().map(|t| t.identity()))
            .collect::<Result<HashSet<_>>>()?;
        let add = self.add.iter().map(Tag::normalized).collect::<Result<Vec<_>>>()?;
        let mut out: Vec<Tag> = tags
            .iter()
            .filter(|t| !t.key.as_ref().is_some_and(|k| keys.contains(&k.to_lowercase())))
            .filter(|t| !remove.contains(&t.identity()))
            .cloned()
            .collect();
        out.extend(add);
        let out = dedupe_tags(out);
        if out.len() > MAX_ASSET_TAGS {
            return Err(invalid(format!("an asset holds at most {MAX_ASSET_TAGS} tags")));
        }
        Ok(out)
    }
}

/// One condition on an asset's tags. `{key, value}` is that tag; `{key}` any tag with that key; `{value}` alone a
/// plain tag with that value. Several together ([`tags_match`]) must all hold.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TagFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

impl TagFilter {
    /// `location: Cumbuco` is that tag, `location:*` any tag with the key, `jump` the plain tag.
    pub fn parse(text: &str) -> Result<TagFilter> {
        let term = match text.split_once(':') {
            Some((key, value)) if clean(value) == "*" => TagFilter {
                key: Some(key.to_string()),
                value: None,
            },
            Some((key, value)) => TagFilter {
                key: Some(key.to_string()),
                value: Some(value.to_string()),
            },
            None => TagFilter {
                key: None,
                value: Some(text.to_string()),
            },
        };
        term.checked()
    }

    /// This term with its key and value normalized the way tags are (a value is not length-capped: it only
    /// has to be non-empty); a term naming neither is refused.
    pub fn checked(&self) -> Result<TagFilter> {
        if self.key.is_none() && self.value.is_none() {
            return Err(invalid("a tag filter names a key, a value or both"));
        }
        let value = match &self.value {
            Some(v) if clean(v).is_empty() => return Err(invalid("a tag filter value cannot be empty")),
            Some(v) => Some(clean(v)),
            None => None,
        };
        Ok(TagFilter {
            key: self.key.as_deref().map(normalize_key).transpose()?,
            value,
        })
    }

    /// Whether some tag of `tags` satisfies this term (ignoring case).
    pub fn matches(&self, tags: &[Tag]) -> bool {
        let key = self.key.as_deref().map(|k| clean(k).to_lowercase());
        let value = self.value.as_deref().map(|v| clean(v).to_lowercase());
        tags.iter().any(|t| {
            let key_ok = match &key {
                Some(k) => t.key.as_ref().is_some_and(|tk| tk.to_lowercase() == *k),
                None => t.key.is_none(),
            };
            key_ok && value.as_ref().is_none_or(|v| *v == t.value.to_lowercase())
        })
    }
}

/// Whether `tags` satisfy every term of `filter` (an empty filter holds for all).
pub fn tags_match(filter: &[TagFilter], tags: &[Tag]) -> bool {
    filter.iter().all(|term| term.matches(tags))
}

/// A value in the vocabulary and how many assets carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagCount {
    pub value: String,
    pub assets: usize,
}

/// A key in the vocabulary: how many assets have it at all, and its values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagKeyCount {
    pub key: String,
    pub assets: usize,
    pub values: Vec<TagCount>,
}

/// Every tag in use, for autocomplete and for an agent choosing what to group by.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagSummary {
    /// Keyed tags by key, A to Z.
    pub keys: Vec<TagKeyCount>,
    /// Plain tags, A to Z.
    pub plain: Vec<TagCount>,
}

fn sort_counts(counts: &mut [TagCount]) {
    counts.sort_by(|a, b| {
        a.value
            .to_lowercase()
            .cmp(&b.value.to_lowercase())
            .then_with(|| a.value.cmp(&b.value))
    });
}

impl TagSummary {
    /// The vocabulary of a library, one tag list per asset. A value is counted once per asset and shown in
    /// the spelling met first.
    pub fn of<'a>(assets: impl IntoIterator<Item = &'a [Tag]>) -> TagSummary {
        let mut plain: HashMap<String, TagCount> = HashMap::new();
        let mut keyed: HashMap<String, (usize, HashMap<String, TagCount>)> = HashMap::new();
        for tags in assets {
            let mut keys_seen = HashSet::new();
            for tag in dedupe_tags(tags.to_vec()) {
                let folded = tag.value.to_lowercase();
                let counts = match &tag.key {
                    Some(key) => {
                        let (assets, values) = keyed.entry(key.to_lowercase()).or_default();
                        if keys_seen.insert(key.to_lowercase()) {
                            *assets += 1;
                        }
                        values
                    }
                    None => &mut plain,
                };
                counts
                    .entry(folded)
                    .or_insert_with(|| TagCount {
                        value: tag.value.clone(),
                        assets: 0,
                    })
                    .assets += 1;
            }
        }
        let sorted = |counts: HashMap<String, TagCount>| {
            let mut counts: Vec<TagCount> = counts.into_values().collect();
            sort_counts(&mut counts);
            counts
        };
        let mut keys: Vec<TagKeyCount> = keyed
            .into_iter()
            .map(|(key, (assets, values))| TagKeyCount {
                key,
                assets,
                values: sorted(values),
            })
            .collect();
        keys.sort_by(|a, b| a.key.cmp(&b.key));
        TagSummary {
            keys,
            plain: sorted(plain),
        }
    }
}

/// One group of [`group_by_key`]: the value that names it (`None` for the assets with no such key) and the
/// indices of its members, in the order they were given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyGroup {
    pub value: Option<String>,
    pub members: Vec<usize>,
}

/// The groups [`group_by_key`] made, and the `order` entries that matched no group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grouping {
    pub groups: Vec<KeyGroup>,
    pub unmatched: Vec<String>,
}

/// Group assets (one tag list each) by the value of `key`. Groups follow `order` (case-insensitive); values it
/// does not list follow A to Z, and with `include_untagged` the assets lacking the key come last as a group of
/// no value. An asset with several values of the key joins the first of them in that order. Members keep the
/// order they were given in.
pub fn group_by_key(assets: &[&[Tag]], key: &str, order: &[String], include_untagged: bool) -> Result<Grouping> {
    let key = normalize_key(key)?;
    let mut ranks: HashMap<String, usize> = HashMap::new();
    let mut listed: Vec<(String, String)> = Vec::new();
    for entry in order {
        let shown = clean(entry);
        if shown.is_empty() {
            return Err(invalid("an empty value in the order"));
        }
        let folded = shown.to_lowercase();
        if !ranks.contains_key(&folded) {
            ranks.insert(folded.clone(), ranks.len());
            listed.push((folded, shown));
        }
    }
    let rank = |folded: &str| ranks.get(folded).copied().unwrap_or(ranks.len());
    let mut groups: HashMap<String, (String, Vec<usize>)> = HashMap::new();
    let mut untagged = Vec::new();
    for (i, tags) in assets.iter().enumerate() {
        let best = tags
            .iter()
            .filter(|t| t.key.as_ref().is_some_and(|k| k.to_lowercase() == key))
            .min_by_key(|t| (rank(&t.value.to_lowercase()), t.value.to_lowercase()));
        match best {
            Some(tag) => groups
                .entry(tag.value.to_lowercase())
                .or_insert_with(|| (tag.value.clone(), Vec::new()))
                .1
                .push(i),
            None => untagged.push(i),
        }
    }
    let unmatched = listed
        .into_iter()
        .filter(|(folded, _)| !groups.contains_key(folded))
        .map(|(_, shown)| shown)
        .collect();
    let mut named: Vec<(String, (String, Vec<usize>))> = groups.into_iter().collect();
    named.sort_by(|(a, _), (b, _)| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    let mut groups: Vec<KeyGroup> = named
        .into_iter()
        .map(|(_, (value, members))| KeyGroup {
            value: Some(value),
            members,
        })
        .collect();
    if include_untagged && !untagged.is_empty() {
        groups.push(KeyGroup {
            value: None,
            members: untagged,
        });
    }
    Ok(Grouping { groups, unmatched })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(text: &str) -> Tag {
        Tag::parse(text).unwrap()
    }

    fn list(texts: &[&str]) -> Vec<Tag> {
        texts.iter().map(|s| t(s)).collect()
    }

    fn shown(tags: &[Tag]) -> Vec<String> {
        tags.iter().map(Tag::to_string).collect()
    }

    #[test]
    fn a_tag_splits_on_the_first_colon_and_a_plain_one_has_none() {
        assert_eq!(t("location: Cumbuco"), Tag::new(Some("location"), "Cumbuco").unwrap());
        assert_eq!(t("jump").key, None);
        let url = t("link: https://x.example/a");
        assert_eq!(
            (url.key.as_deref(), url.value.as_str()),
            (Some("link"), "https://x.example/a")
        );
        assert_eq!(t("location: Cumbuco").to_string(), "location: Cumbuco");
        assert_eq!(t("jump").to_string(), "jump");
    }

    #[test]
    fn spelling_is_normalized() {
        let tag = t("  LoCation \t:\n  Jeri   coara\u{7} ");
        assert_eq!(tag.key.as_deref(), Some("location"), "keys are lowercased");
        assert_eq!(
            tag.value, "Jeri coara",
            "values keep their case; whitespace collapses, controls go"
        );
        assert_eq!(t("a\tb").value, "a b", "a tab separates words rather than joining them");
    }

    #[test]
    fn empty_and_over_long_tags_are_refused() {
        for bad in ["", "   ", "location:", "location:   ", ": Cumbuco", " : x", "\u{7}"] {
            assert!(Tag::parse(bad).is_err(), "{bad:?}");
        }
        assert!(Tag::new(Some(&"k".repeat(MAX_TAG_KEY)), "v").is_ok());
        assert!(Tag::new(Some(&"k".repeat(MAX_TAG_KEY + 1)), "v").is_err());
        assert!(Tag::new(None, &"é".repeat(MAX_TAG_VALUE)).is_ok(), "counted in characters");
        assert!(Tag::new(None, &"é".repeat(MAX_TAG_VALUE + 1)).is_err());
    }

    #[test]
    fn a_tag_is_stored_without_a_missing_key() {
        let json = serde_json::to_value(t("jump")).unwrap();
        assert_eq!(json, serde_json::json!({"value": "jump"}));
        let json = serde_json::to_value(t("rider: Orell")).unwrap();
        assert_eq!(json, serde_json::json!({"key": "rider", "value": "Orell"}));
        let back: Tag = serde_json::from_value(json).unwrap();
        assert_eq!(back, t("rider: Orell"));
    }

    #[test]
    fn repeats_ignore_case_and_the_first_spelling_stays() {
        let tags = dedupe_tags(list(&[
            "location: Cumbuco",
            "Location: CUMBUCO",
            "jump",
            "Jump",
            "rider: A",
            "rider: B",
        ]));
        assert_eq!(shown(&tags), ["location: Cumbuco", "jump", "rider: A", "rider: B"]);
        assert!(t("jump").same_as(&t("JUMP")));
        assert!(!t("jump").same_as(&t("x: jump")), "a plain tag is not a keyed one");
        assert!(same_tags(&list(&["a", "b: C"]), &list(&["B: c", "A"])));
        assert!(!same_tags(&list(&["a"]), &list(&["a", "b"])));
    }

    #[test]
    fn an_edit_removes_keys_then_tags_then_adds() {
        let tags = list(&["location: Cumbuco", "rider: Orell", "rider: Sam", "jump"]);
        let edit = TagEdit {
            add: list(&["location: Jeri", "wave"]),
            remove: list(&["JUMP"]),
            remove_keys: vec!["Location".into()],
        };
        assert_eq!(
            shown(&edit.apply(&tags).unwrap()),
            ["rider: Orell", "rider: Sam", "location: Jeri", "wave"]
        );
        let swap = TagEdit {
            add: list(&["jump"]),
            remove: list(&["jump"]),
            ..Default::default()
        };
        assert_eq!(
            shown(&swap.apply(&tags).unwrap()),
            ["location: Cumbuco", "rider: Orell", "rider: Sam", "jump"]
        );
        let by_key = TagEdit {
            remove_keys: vec!["rider".into()],
            ..Default::default()
        };
        assert_eq!(shown(&by_key.apply(&tags).unwrap()), ["location: Cumbuco", "jump"]);
    }

    #[test]
    fn an_edit_takes_off_exactly_the_tag_it_names() {
        let tags = list(&["location: Cumbuco", "Cumbuco"]);
        let plain = TagEdit {
            remove: list(&["cumbuco"]),
            ..Default::default()
        };
        assert_eq!(shown(&plain.apply(&tags).unwrap()), ["location: Cumbuco"]);
        let keyed = TagEdit {
            remove: list(&["location: cumbuco"]),
            ..Default::default()
        };
        assert_eq!(shown(&keyed.apply(&tags).unwrap()), ["Cumbuco"]);
        let missing = TagEdit {
            remove: list(&["nothing"]),
            ..Default::default()
        };
        assert_eq!(missing.apply(&tags).unwrap(), tags);
    }

    #[test]
    fn an_edit_normalizes_what_it_adds_and_refuses_what_is_invalid() {
        let sloppy = TagEdit {
            add: vec![Tag {
                key: Some(" Location ".into()),
                value: "  Cumbuco  ".into(),
            }],
            ..Default::default()
        };
        assert_eq!(shown(&sloppy.apply(&[]).unwrap()), ["location: Cumbuco"]);
        for bad in [
            TagEdit {
                add: vec![Tag {
                    key: None,
                    value: "  ".into(),
                }],
                ..Default::default()
            },
            TagEdit {
                remove: vec![Tag {
                    key: Some("".into()),
                    value: "x".into(),
                }],
                ..Default::default()
            },
            TagEdit {
                remove_keys: vec![" ".into()],
                ..Default::default()
            },
        ] {
            assert!(bad.apply(&list(&["a"])).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_asset_holds_at_most_so_many_tags() {
        let full: Vec<Tag> = (0..MAX_ASSET_TAGS).map(|i| t(&format!("tag{i}"))).collect();
        let more = TagEdit {
            add: list(&["one more"]),
            ..Default::default()
        };
        let err = more.apply(&full).unwrap_err().to_string();
        assert!(err.contains("at most 64 tags"), "{err}");
        let swap = TagEdit {
            add: list(&["one more"]),
            remove: list(&["tag0"]),
            ..Default::default()
        };
        assert_eq!(swap.apply(&full).unwrap().len(), MAX_ASSET_TAGS);
        let again = TagEdit {
            add: list(&["TAG1"]),
            ..Default::default()
        };
        assert_eq!(again.apply(&full).unwrap().len(), MAX_ASSET_TAGS, "a repeat adds nothing");
    }

    #[test]
    fn an_edit_refuses_an_unknown_field() {
        let err = serde_json::from_str::<TagEdit>(r#"{"add":[],"remove_key":["x"]}"#).unwrap_err();
        assert!(err.to_string().contains("remove_key"), "{err}");
        assert!(serde_json::from_str::<TagEdit>("{}").unwrap().is_empty());
    }

    #[test]
    fn a_filter_term_matches_a_key_a_pair_or_a_plain_tag() {
        let tags = list(&["location: Cumbuco", "rider: Orell", "jump"]);
        let term = |key: Option<&str>, value: Option<&str>| TagFilter {
            key: key.map(Into::into),
            value: value.map(Into::into),
        };
        assert!(term(Some("location"), None).matches(&tags), "any value");
        assert!(term(Some("LOCATION"), Some("cumbuco")).matches(&tags), "exact, ignoring case");
        assert!(!term(Some("location"), Some("Jeri")).matches(&tags));
        assert!(!term(Some("wind"), None).matches(&tags));
        assert!(term(None, Some("JUMP")).matches(&tags), "a value alone is a plain tag");
        assert!(!term(None, Some("Cumbuco")).matches(&tags), "not the keyed tag of that value");
        assert!(!term(None, Some("jump")).matches(&list(&["style: jump"])));
    }

    #[test]
    fn filters_all_have_to_hold() {
        let tags = list(&["location: Cumbuco", "rider: Orell"]);
        let all = [
            TagFilter::parse("location:*").unwrap(),
            TagFilter::parse("rider: orell").unwrap(),
        ];
        assert!(tags_match(&all, &tags));
        assert!(tags_match(&[], &tags), "no terms, no restriction");
        let one_off = [
            TagFilter::parse("location: Cumbuco").unwrap(),
            TagFilter::parse("jump").unwrap(),
        ];
        assert!(!tags_match(&one_off, &tags));
        assert!(!tags_match(&all, &list(&["location: Cumbuco"])));
    }

    #[test]
    fn a_filter_term_is_parsed_and_checked() {
        assert_eq!(
            TagFilter::parse(" Location : * ").unwrap(),
            TagFilter {
                key: Some("location".into()),
                value: None
            }
        );
        assert_eq!(
            TagFilter::parse("location: Cumbuco").unwrap(),
            TagFilter {
                key: Some("location".into()),
                value: Some("Cumbuco".into())
            }
        );
        assert_eq!(
            TagFilter::parse("jump").unwrap(),
            TagFilter {
                key: None,
                value: Some("jump".into())
            }
        );
        for bad in ["", "  ", "location:", ": x"] {
            assert!(TagFilter::parse(bad).is_err(), "{bad:?}");
        }
        assert!(TagFilter::default().checked().is_err(), "a term names something");
        let json = serde_json::to_value(TagFilter::parse("location:*").unwrap()).unwrap();
        assert_eq!(json, serde_json::json!({"key": "location"}));
    }

    #[test]
    fn the_vocabulary_counts_assets_and_sorts() {
        let a = list(&["location: Jeri", "rider: Orell", "jump"]);
        let b = list(&["location: Cumbuco", "Rider: orell", "wave"]);
        let c = list(&["location: cumbuco", "jump", "Jump"]);
        let summary = TagSummary::of([a.as_slice(), b.as_slice(), c.as_slice(), &[]]);
        let keys: Vec<_> = summary
            .keys
            .iter()
            .map(|k| {
                (
                    k.key.as_str(),
                    k.assets,
                    k.values.iter().map(|v| (v.value.as_str(), v.assets)).collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                ("location", 3, vec![("Cumbuco", 2), ("Jeri", 1)]),
                ("rider", 2, vec![("Orell", 2)]),
            ]
        );
        let plain: Vec<(&str, usize)> = summary.plain.iter().map(|v| (v.value.as_str(), v.assets)).collect();
        assert_eq!(plain, vec![("jump", 2), ("wave", 1)], "a repeat on one asset counts once");
        assert_eq!(TagSummary::of(std::iter::empty()), TagSummary::default());
    }

    fn groups(assets: &[Vec<Tag>], key: &str, order: &[&str], untagged: bool) -> Grouping {
        let refs: Vec<&[Tag]> = assets.iter().map(Vec::as_slice).collect();
        let order: Vec<String> = order.iter().map(|s| s.to_string()).collect();
        group_by_key(&refs, key, &order, untagged).unwrap()
    }

    fn outline(g: &Grouping) -> Vec<(Option<&str>, Vec<usize>)> {
        g.groups.iter().map(|g| (g.value.as_deref(), g.members.clone())).collect()
    }

    #[test]
    fn groups_run_a_to_z_by_default_and_members_keep_their_order() {
        let assets = [
            list(&["location: Jeri"]),
            list(&["location: Cumbuco"]),
            list(&["location: jeri"]),
            list(&["location: Atins"]),
        ];
        let g = groups(&assets, "Location", &[], false);
        assert_eq!(
            outline(&g),
            vec![
                (Some("Atins"), vec![3]),
                (Some("Cumbuco"), vec![1]),
                (Some("Jeri"), vec![0, 2])
            ]
        );
        assert!(g.unmatched.is_empty());
    }

    #[test]
    fn an_order_goes_first_and_the_rest_follow_a_to_z() {
        let assets = [
            list(&["location: Jeri"]),
            list(&["location: Cumbuco"]),
            list(&["location: Atins"]),
            list(&["location: Prea"]),
        ];
        let g = groups(&assets, "location", &["prea", "JERI", "Nowhere", "jeri"], false);
        assert_eq!(
            outline(&g),
            vec![
                (Some("Prea"), vec![3]),
                (Some("Jeri"), vec![0]),
                (Some("Atins"), vec![2]),
                (Some("Cumbuco"), vec![1])
            ]
        );
        assert_eq!(g.unmatched, ["Nowhere"], "an entry no asset carries is reported once");
    }

    #[test]
    fn assets_without_the_key_come_last_only_when_asked() {
        let assets = [list(&["jump"]), list(&["location: Jeri"]), list(&[]), list(&["rider: Orell"])];
        assert_eq!(
            outline(&groups(&assets, "location", &[], false)),
            vec![(Some("Jeri"), vec![1])]
        );
        assert_eq!(
            outline(&groups(&assets, "location", &[], true)),
            vec![(Some("Jeri"), vec![1]), (None, vec![0, 2, 3])]
        );
        let nobody = groups(&assets, "wind", &[], true);
        assert_eq!(outline(&nobody), vec![(None, vec![0, 1, 2, 3])]);
        assert!(groups(&assets, "wind", &[], false).groups.is_empty());
    }

    #[test]
    fn an_asset_with_several_values_joins_the_first_in_order() {
        let assets = [list(&["rider: Sam", "rider: Orell"]), list(&["rider: Sam"])];
        let alphabetical = groups(&assets, "rider", &[], false);
        assert_eq!(outline(&alphabetical), vec![(Some("Orell"), vec![0]), (Some("Sam"), vec![1])]);
        let sam_first = groups(&assets, "rider", &["sam"], false);
        assert_eq!(outline(&sam_first), vec![(Some("Sam"), vec![0, 1])]);
    }

    #[test]
    fn grouping_refuses_an_empty_key_or_order_entry() {
        let refs: Vec<&[Tag]> = Vec::new();
        assert!(group_by_key(&refs, " ", &[], false).is_err());
        assert!(group_by_key(&refs, "location", &["  ".to_string()], false).is_err());
    }
}
