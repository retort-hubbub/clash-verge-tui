//! Profile index entries.
//!
//! A profile is one document in the pipeline that produces the configuration
//! the core runs. The index lives in `profiles.yaml` and is deliberately
//! readable and writable by `clash-verge-rev`: an existing installation can be
//! imported by copying its directory, and a user can move between the two
//! front-ends without losing their subscriptions.
//!
//! Three additions go beyond the reference format, all optional so that the
//! file stays compatible:
//!
//! * [`ProfileType::Override`] — declarative edits, described in
//!   `enhance::overlay`, standing in for JavaScript.
//! * [`ProfileType::Script`] is **recognised but not executed**; see
//!   [`PrfItem::unsupported_reason`]. Silently ignoring a script would produce
//!   a config the user did not write, so it is surfaced as an error instead.
//! * [`SeqPatch`] captures the `prepend`/`append`/`delete` sequence format so
//!   `rules`/`proxies`/`groups` profiles from a modern `clash-verge-rev` import
//!   as editable rather than as opaque YAML.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a profile contributes to the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileType {
    /// A full configuration downloaded from a subscription URL.
    Remote,
    /// A full configuration authored locally.
    Local,
    /// A YAML patch deep-merged into the accumulated configuration.
    Merge,
    /// A declarative set of edits applied by path.
    Override,
    /// A sequence patch for the `rules` list.
    Rules,
    /// A sequence patch for the `proxies` list.
    Proxies,
    /// A sequence patch for the `proxy-groups` list.
    Groups,
    /// A `clash-verge-rev` JavaScript profile. Recognised, never executed.
    Script,
}

impl ProfileType {
    /// Position in the application order.
    ///
    /// A base document must exist before anything is merged into it, and
    /// sequence patches run after merges so that a patch sees the final list.
    #[must_use]
    pub fn order(self) -> u8 {
        match self {
            Self::Remote | Self::Local => 0,
            Self::Merge => 1,
            Self::Override => 2,
            Self::Rules | Self::Proxies | Self::Groups => 3,
            Self::Script => 4,
        }
    }

    /// `true` when the profile supplies the document everything else modifies.
    #[must_use]
    pub fn is_base(self) -> bool {
        matches!(self, Self::Remote | Self::Local)
    }

    /// `true` when the profile is only meaningful alongside a base.
    #[must_use]
    pub fn is_patch(self) -> bool {
        !self.is_base()
    }

    /// `true` when the type is a sequence patch rather than a document.
    #[must_use]
    pub fn is_sequence(self) -> bool {
        matches!(self, Self::Rules | Self::Proxies | Self::Groups)
    }

    /// The configuration key a sequence patch targets.
    #[must_use]
    pub fn sequence_key(self) -> Option<&'static str> {
        match self {
            Self::Rules => Some("rules"),
            Self::Proxies => Some("proxies"),
            Self::Groups => Some("proxy-groups"),
            _ => None,
        }
    }

    /// Lower-case name used in the index and on the command line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Remote => "remote",
            Self::Local => "local",
            Self::Merge => "merge",
            Self::Override => "override",
            Self::Rules => "rules",
            Self::Proxies => "proxies",
            Self::Groups => "groups",
            Self::Script => "script",
        }
    }

    /// Single-letter prefix used in generated filenames, matching the
    /// convention `clash-verge-rev` uses so imports look familiar.
    #[must_use]
    pub fn file_prefix(self) -> char {
        match self {
            Self::Remote => 'R',
            Self::Local => 'L',
            Self::Merge => 'm',
            Self::Override => 'o',
            Self::Rules => 'r',
            Self::Proxies => 'p',
            Self::Groups => 'g',
            Self::Script => 's',
        }
    }

    /// Every type, in pipeline order.
    #[must_use]
    pub fn all() -> [Self; 8] {
        [
            Self::Remote,
            Self::Local,
            Self::Merge,
            Self::Override,
            Self::Rules,
            Self::Proxies,
            Self::Groups,
            Self::Script,
        ]
    }
}

/// When and how a remote profile refreshes itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrfOption {
    /// Minutes between automatic refreshes; `0` or absent disables it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_interval: Option<u64>,
    /// Whether `cvt update` may refresh this profile unattended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_auto_update: Option<bool>,
    /// Uid of a merge profile to apply, overriding the chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<String>,
    /// Uid of a script profile to apply. Never executed; reported instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// Uid of a rules sequence profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,
    /// Uid of a proxies sequence profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxies: Option<String>,
    /// Uid of a groups sequence profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<String>,
    /// Any other option, preserved for round-tripping.
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

impl PrfOption {
    /// The refresh interval, if auto-update is enabled.
    #[must_use]
    pub fn effective_interval(&self) -> Option<u64> {
        match (self.update_interval, self.allow_auto_update) {
            (Some(m), Some(true)) if m > 0 => Some(m),
            _ => None,
        }
    }

    /// Uids this profile pulls in through its options.
    #[must_use]
    pub fn referenced_uids(&self) -> Vec<&str> {
        [
            &self.merge,
            &self.script,
            &self.rules,
            &self.proxies,
            &self.groups,
        ]
        .into_iter()
        .filter_map(Option::as_deref)
        .filter(|s| !s.is_empty())
        .collect()
    }
}

/// Subscription quota, parsed from the `subscription-userinfo` header.
///
/// Field names are capitalised because that is how the core and every panel
/// spell them; they are parsed case-insensitively anyway.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInfo {
    /// Bytes uploaded.
    #[serde(default)]
    pub upload: u64,
    /// Bytes downloaded.
    #[serde(default)]
    pub download: u64,
    /// Quota in bytes; `0` means unlimited.
    #[serde(default)]
    pub total: u64,
    /// Expiry as a unix timestamp; `0` means no expiry.
    #[serde(default)]
    pub expire: u64,
}

impl UserInfo {
    /// Bytes consumed.
    #[must_use]
    pub fn used(&self) -> u64 {
        self.upload.saturating_add(self.download)
    }

    /// Fraction of the quota consumed, `None` when unlimited and `0.0` when
    /// the quota is already exhausted exactly.
    #[must_use]
    pub fn used_fraction(&self) -> Option<f64> {
        (self.total > 0).then(|| (self.used() as f64 / self.total as f64).clamp(0.0, 1.0))
    }

    /// `true` when nothing useful is known.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total == 0 && self.expire == 0 && self.used() == 0
    }
}

/// The one plain path component a file name has to be, if it is one.
///
/// Separators, the two directory names, an empty string and a NUL are refused.
/// A name that fails is not repaired by keeping its last segment:
/// `../../etc/passwd` would become `passwd`, which is a different document from
/// the one the index meant, and silently pointing at that is worse than saying
/// the name is unusable.
pub(crate) fn single_component(name: &str) -> Option<String> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        return None;
    }
    Some(name.to_owned())
}

/// A group's remembered selection.
///
/// `clash-verge-rev` records the node a user picked, per profile, and replays
/// it when that profile is applied again. It is the difference between "my
/// choice survived the subscription update" and "I pick again every morning":
/// a configuration reload rebuilds every group, and the core's own memory of
/// the choice is keyed to the group it was made in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedNode {
    /// The group the choice was made in.
    pub name: String,
    /// The member that was chosen.
    pub now: String,
}

impl SelectedNode {
    /// Record a choice.
    #[must_use]
    pub fn new(name: impl Into<String>, now: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            now: now.into(),
        }
    }
}

/// One entry in the profile index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrfItem {
    /// Stable identifier; also the filename stem.
    pub uid: String,
    /// What the profile contributes.
    #[serde(rename = "type")]
    pub kind: ProfileType,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Free-form description.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub desc: String,
    /// The node choices made in this profile, newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected: Vec<SelectedNode>,
    /// The subscription's own page, as the panel advertised it.
    ///
    /// Recorded rather than guessed: a panel that sends
    /// `profile-web-page-url` is telling the user where to manage their
    /// account, and the only moment anyone knows that is the moment the
    /// subscription was fetched. Only http(s) is kept — see the parser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    /// Subscription URL, for remote profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Filename inside the profiles directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Last successful refresh, as a unix timestamp in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<i64>,
    /// Refresh and chain options.
    #[serde(default, skip_serializing_if = "PrfOption::is_none")]
    pub option: PrfOption,
    /// Subscription quota, when the provider reported it.
    #[serde(default, skip_serializing_if = "UserInfo::is_empty")]
    pub extra: UserInfo,
    /// Anything else the index carried, preserved verbatim.
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

impl PrfOption {
    /// `true` when every field is at its default.
    #[must_use]
    pub fn is_none(&self) -> bool {
        self == &Self::default()
    }
}

impl PrfItem {
    /// A new remote profile.
    ///
    /// The filename is left unset: an empty uid must not produce `".yaml"`.
    /// [`PrfItem::file_name`] derives it, and [`ProfileStore::add`] records it.
    ///
    /// [`ProfileStore::add`]: crate::profile::store::ProfileStore::add
    #[must_use]
    pub fn remote(uid: impl Into<String>, name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            kind: ProfileType::Remote,
            name: name.into(),
            url: Some(url.into()),
            ..Default::default()
        }
    }

    /// A new local profile.
    #[must_use]
    pub fn local(uid: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            kind: ProfileType::Local,
            name: name.into(),
            ..Default::default()
        }
    }

    /// A new patch profile of any non-base type.
    #[must_use]
    pub fn patch(uid: impl Into<String>, name: impl Into<String>, kind: ProfileType) -> Self {
        Self {
            uid: uid.into(),
            kind,
            name: name.into(),
            ..Default::default()
        }
    }

    /// `true` when the profile is fetched from a URL.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        self.kind == ProfileType::Remote
    }

    /// The filename, derived from the uid when the index omitted it.
    #[must_use]
    pub fn file_name(&self) -> String {
        // The document lives *inside* the profiles directory, so this is one
        // path component and nothing else.
        //
        // Every arm goes through the guard, and that is the point: the last
        // two attempts at this validated the field somebody had named — first
        // `uid`, then `file` — and each left the *other* arm of this one
        // expression open. A loaded index entry that omits `file` falls back
        // to `{uid}.yaml`, so a hostile uid walked into a path through the
        // fallback; and the fallback is derived from the uid, which is as
        // untrusted as the name it replaces.
        //
        // A name that is not one component is replaced rather than refused: an
        // entry with an unusable name is still an entry, and the alternative is
        // an index that cannot be opened at all.
        self.file
            .as_deref()
            .and_then(single_component)
            .unwrap_or_else(|| self.fallback_file_name())
    }

    /// A name that is certainly one plain path component.
    ///
    /// Built from the uid with everything a path can act on removed, because
    /// the uid is untrusted too. Deliberately *not* [`Self::default_file_name`]:
    /// that one keeps the uid verbatim, which is right for a value this program
    /// wrote and wrong for a value it read.
    fn fallback_file_name(&self) -> String {
        let ext = if self.kind == ProfileType::Script {
            "js"
        } else {
            "yaml"
        };
        let stem: String = self
            .uid
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        // `..` and `.` cannot survive the filter above, but an empty one can.
        let stem = if stem.is_empty() {
            "profile".to_owned()
        } else {
            stem
        };
        format!("{stem}.{ext}")
    }

    /// The filename this profile would have if named after its uid.
    #[must_use]
    pub fn default_file_name(&self) -> String {
        let ext = if self.kind == ProfileType::Script {
            "js"
        } else {
            "yaml"
        };
        format!("{}.{ext}", self.uid)
    }

    /// Why this profile cannot participate in the pipeline, if it cannot.
    ///
    /// Only one type is ever unsupported: `clash-verge-rev` scripts are
    /// JavaScript, and this application has no JavaScript engine. Reporting
    /// that plainly is better than skipping the profile and handing the core a
    /// configuration the user never wrote.
    #[must_use]
    pub fn unsupported_reason(&self) -> Option<String> {
        (self.kind == ProfileType::Script).then(|| {
            format!(
                "`{}` is a JavaScript profile; clash-verge-tui does not run JavaScript. \
                 Re-express it as an `override` profile (see docs/OVERRIDE-FORMAT.md), \
                 or disable it in the chain.",
                self.name
            )
        })
    }

    /// Display label, falling back to the uid when unnamed.
    #[must_use]
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.uid
        } else {
            &self.name
        }
    }
}

impl Default for PrfItem {
    fn default() -> Self {
        Self {
            uid: String::new(),
            kind: ProfileType::Local,
            name: String::new(),
            desc: String::new(),
            selected: Vec::new(),
            home: None,
            url: None,
            file: None,
            updated: None,
            option: PrfOption::default(),
            extra: UserInfo::default(),
            other: BTreeMap::new(),
        }
    }
}

/// A `prepend`/`append`/`delete` sequence patch.
///
/// This is the format a current `clash-verge-rev` writes for its `rules`,
/// `proxies` and `groups` profile types. Modelling it explicitly means an
/// imported profile is editable in the TUI instead of being opaque YAML.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeqPatch {
    /// Items inserted at the front, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prepend: Vec<String>,
    /// Items appended at the back, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub append: Vec<String>,
    /// Items removed, matched by `name` for named lists or by exact value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delete: Vec<String>,
}

impl SeqPatch {
    /// `true` when the patch changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.prepend.is_empty() && self.append.is_empty() && self.delete.is_empty()
    }

    /// Apply to a list of strings.
    ///
    /// Deletion happens first so that a value may be deleted from a
    /// subscription and re-added by the patch without ending up duplicated.
    #[must_use]
    pub fn apply(&self, base: &[String]) -> Vec<String> {
        let mut out: Vec<String> = base
            .iter()
            .filter(|item| !self.delete.contains(item))
            .cloned()
            .collect();
        // Checked against `out` *and* against what has already been collected:
        // a patch that names the same value twice is one value, and comparing
        // only against the base let the second copy through. A patch is a
        // document a person wrote, and writing the same line twice is a slip,
        // not a request for two of them.
        let mut head: Vec<String> = Vec::new();
        for item in &self.prepend {
            if !out.contains(item) && !head.contains(item) {
                head.push(item.clone());
            }
        }
        head.append(&mut out);
        for item in &self.append {
            if !head.contains(item) {
                head.push(item.clone());
            }
        }
        head
    }

    /// Apply to a list of JSON values, deleting by `name` when present.
    #[must_use]
    pub fn apply_values(&self, base: &[Value]) -> Vec<Value> {
        let out: Vec<Value> = base
            .iter()
            .filter(|v| name_of(v).is_none_or(|n| !self.delete.contains(&n)))
            .cloned()
            .collect();
        // Deduplicated by the same rule the doc comment states, and by the
        // same rule `apply` uses for plain strings: an item the base already
        // holds is not added, and neither is a second copy of one this patch
        // already named. Comparing only the *names* for the base and only
        // identity for the patch is what let a repeated entry through.
        let parse = |s: &String| match serde_json::from_str::<Value>(s) {
            Ok(v) => v,
            Err(_) => Value::String(s.clone()),
        };
        let already_present =
            |list: &[Value], candidate: &Value| list.iter().any(|e| same_item(e, candidate));
        let mut head: Vec<Value> = Vec::new();
        for item in &self.prepend {
            let value = parse(item);
            if !already_present(&out, &value) && !already_present(&head, &value) {
                head.push(value);
            }
        }
        head.extend(out);
        for item in &self.append {
            let value = parse(item);
            if !already_present(&head, &value) {
                head.push(value);
            }
        }
        head
    }
}

/// How a list item is identified for deletion and deduplication.
///
/// A named mapping is identified by its `name`, so a proxy is the same proxy
/// however much else about it changed; a plain string is its own identity.
fn name_of(v: &Value) -> Option<String> {
    v.get("name")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| v.as_str().map(str::to_owned))
}

/// `true` when two items are the same item.
///
/// Two anonymous values are compared structurally, because that is the only
/// identity they have; comparing them by name would make every unnamed value
/// equal to every other one.
fn same_item(a: &Value, b: &Value) -> bool {
    match (name_of(a), name_of(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_clash_verge_rev_index_entry() {
        // Field names and shapes taken from a real profiles.yaml.
        let yaml = r#"
uid: Rabcdefghij
type: remote
name: My Airport
desc: ""
url: "https://example.com/sub?token=abc"
file: Rabcdefghij.yaml
updated: 1690000000
option:
  update_interval: 1440
  allow_auto_update: true
extra:
  upload: 100
  download: 200
  total: 1000000
  expire: 1800000000
"#;
        let item: PrfItem = serde_norway::from_str(yaml).unwrap();
        assert_eq!(item.uid, "Rabcdefghij");
        assert_eq!(item.kind, ProfileType::Remote);
        assert!(item.is_remote());
        assert_eq!(item.option.effective_interval(), Some(1440));
        assert_eq!(item.file_name(), "Rabcdefghij.yaml");
        assert_eq!(item.extra.used(), 300);
        assert!(item.unsupported_reason().is_none());
    }

    #[test]
    fn unrecognised_profile_types_fail_loudly_rather_than_defaulting() {
        // Silently treating an unknown type as `local` would corrupt the chain.
        let yaml = "uid: x\ntype: teleport\nname: n\n";
        assert!(serde_norway::from_str::<PrfItem>(yaml).is_err());
    }

    #[test]
    fn round_trips_unknown_fields_and_options() {
        let yaml = r#"
uid: L1
type: local
name: mine
file: L1.yaml
something-new: [1, 2]
option:
  update_interval: 60
  future_option: true
"#;
        let item: PrfItem = serde_norway::from_str(yaml).unwrap();
        assert_eq!(item.other.get("something-new"), Some(&json!([1, 2])));
        assert!(item.option.other.contains_key("future_option"));
        let back = serde_norway::to_string(&item).unwrap();
        let again: PrfItem = serde_norway::from_str(&back).unwrap();
        assert_eq!(item, again, "index entries must survive a round-trip");
    }

    #[test]
    fn orders_the_pipeline_base_first_and_script_last() {
        let mut types = ProfileType::all().to_vec();
        types.sort_by_key(|t| t.order());
        assert_eq!(types[0], ProfileType::Remote);
        assert_eq!(types[7], ProfileType::Script);
        assert!(ProfileType::Merge.order() < ProfileType::Override.order());
        assert!(ProfileType::Override.order() < ProfileType::Rules.order());
    }

    #[test]
    fn identifies_base_and_sequence_types() {
        assert!(ProfileType::Remote.is_base());
        assert!(ProfileType::Local.is_base());
        assert!(!ProfileType::Merge.is_base());
        assert!(ProfileType::Rules.is_sequence());
        assert_eq!(ProfileType::Rules.sequence_key(), Some("rules"));
        assert_eq!(ProfileType::Groups.sequence_key(), Some("proxy-groups"));
        assert_eq!(ProfileType::Merge.sequence_key(), None);
    }

    #[test]
    fn auto_update_requires_both_a_positive_interval_and_the_flag() {
        let mut o = PrfOption {
            update_interval: Some(60),
            ..PrfOption::default()
        };
        assert_eq!(o.effective_interval(), None, "the flag must be set too");
        o.allow_auto_update = Some(true);
        assert_eq!(o.effective_interval(), Some(60));
        o.update_interval = Some(0);
        assert_eq!(o.effective_interval(), None, "zero means disabled");
    }

    #[test]
    fn a_script_profile_is_reported_as_unsupported_not_skipped_silently() {
        let item = PrfItem::patch("s1", "tweaks", ProfileType::Script);
        let reason = item.unsupported_reason().unwrap();
        assert!(reason.contains("JavaScript"), "{reason}");
        assert!(
            reason.contains("override"),
            "the message must suggest the alternative"
        );
        assert_eq!(item.file_name(), "s1.js");
    }

    #[test]
    fn collects_the_uids_an_option_pulls_in() {
        let o = PrfOption {
            merge: Some("m1".into()),
            rules: Some("r1".into()),
            proxies: None,
            script: Some(String::new()),
            ..PrfOption::default()
        };
        assert_eq!(
            o.referenced_uids(),
            vec!["m1", "r1"],
            "empty uids are ignored"
        );
    }

    #[test]
    fn user_info_computes_usage() {
        let u = UserInfo {
            upload: 1_000,
            download: 3_000,
            total: 10_000,
            expire: 0,
        };
        assert_eq!(u.used(), 4_000);
        assert_eq!(u.used_fraction(), Some(0.4));
        assert!(!u.is_empty());

        let unlimited = UserInfo {
            upload: 5,
            total: 0,
            ..UserInfo::default()
        };
        assert_eq!(
            unlimited.used_fraction(),
            None,
            "an unlimited plan has no fraction"
        );
        assert!(UserInfo::default().is_empty());

        let over = UserInfo {
            upload: 50_000,
            download: 50_000,
            total: 10_000,
            ..UserInfo::default()
        };
        assert_eq!(over.used_fraction(), Some(1.0), "usage is clamped to 100%");
    }

    #[test]
    fn sequence_patch_prepends_appends_and_deletes() {
        let base = vec!["A".to_owned(), "B".to_owned(), "C".to_owned()];
        let patch = SeqPatch {
            prepend: vec!["X".into()],
            append: vec!["Y".into()],
            delete: vec!["B".into()],
        };
        assert_eq!(patch.apply(&base), vec!["X", "A", "C", "Y"]);
        assert!(SeqPatch::default().is_empty());
        assert_eq!(SeqPatch::default().apply(&base), base);
    }

    #[test]
    fn sequence_patch_does_not_duplicate_a_deleted_then_readded_item() {
        let base = vec!["A".to_owned(), "B".to_owned()];
        let patch = SeqPatch {
            prepend: vec!["B".into()],
            append: vec!["B".into()],
            delete: vec!["B".into()],
        };
        assert_eq!(
            patch.apply(&base),
            vec!["B", "A"],
            "exactly one copy survives"
        );
    }

    #[test]
    fn sequence_patch_handles_named_lists() {
        let base = vec![
            json!({"name": "keep", "port": 1}),
            json!({"name": "drop", "port": 2}),
        ];
        let patch = SeqPatch {
            prepend: vec![r#"{"name":"front","port":3}"#.into()],
            append: vec![],
            delete: vec!["drop".into()],
        };
        let out = patch.apply_values(&base);
        let names: Vec<&str> = out.iter().filter_map(|v| v["name"].as_str()).collect();
        assert_eq!(names, vec!["front", "keep"]);
    }

    #[test]
    fn a_plain_string_sequence_is_handled_too() {
        let base: Vec<Value> = vec![json!("A,DIRECT"), json!("B,DIRECT")];
        let patch = SeqPatch {
            prepend: vec!["X,DIRECT".into()],
            append: vec![],
            delete: vec!["B,DIRECT".into()],
        };
        let out = patch.apply_values(&base);
        assert_eq!(out, vec![json!("X,DIRECT"), json!("A,DIRECT")]);
    }

    #[test]
    fn filenames_are_derived_from_the_uid_when_unset() {
        let remote = PrfItem::remote("R1", "n", "https://x");
        assert!(
            remote.file.is_none(),
            "a constructor must not invent a filename"
        );
        assert_eq!(remote.file_name(), "R1.yaml");
        assert_eq!(PrfItem::local("L1", "n").file_name(), "L1.yaml");
        assert_eq!(
            PrfItem::patch("s1", "n", ProfileType::Script).file_name(),
            "s1.js"
        );
        assert_eq!(
            PrfItem::patch("o1", "n", ProfileType::Override).file_name(),
            "o1.yaml"
        );

        // An empty uid must not yield a bare extension — `.yaml` is a
        // dotfile, and a document nobody can find by name is worse than one
        // with a placeholder stem.
        let unassigned = PrfItem::remote("", "n", "https://x");
        assert_eq!(unassigned.file_name(), "profile.yaml");
    }

    /// The fallback is derived from the uid, which is untrusted input too.
    ///
    /// Two earlier attempts at this validated the field somebody had named —
    /// `uid`, then `file` — and each left the other arm of `file_name`'s one
    /// expression open. This covers the arm that does not read `file` at all.
    #[test]
    fn an_unusable_uid_cannot_reach_a_path_through_the_fallback() {
        for (uid, expected) in [
            ("../canary", "canary.yaml"),
            ("../profiles", "profiles.yaml"),
            ("../../etc/passwd", "etcpasswd.yaml"),
            ("/absolute/path", "absolutepath.yaml"),
            ("a/b", "ab.yaml"),
            ("..", "profile.yaml"),
            (".", "profile.yaml"),
            ("", "profile.yaml"),
            ("a\0b", "ab.yaml"),
        ] {
            let mut item = PrfItem::remote(uid, "n", "https://x");
            item.file = None;
            assert_eq!(item.file_name(), expected, "uid {uid:?}");
            assert!(
                !item.file_name().contains(['/', '\\']),
                "uid {uid:?} reached a path: {}",
                item.file_name()
            );
        }
    }

    #[test]
    fn labels_fall_back_to_the_uid() {
        let mut item = PrfItem::local("L1", "");
        assert_eq!(item.label(), "L1");
        item.name = "mine".into();
        assert_eq!(item.label(), "mine");
    }
}
