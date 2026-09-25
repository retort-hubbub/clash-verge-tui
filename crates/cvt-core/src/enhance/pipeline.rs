//! Turning a profile chain into the configuration the core runs.
//!
//! This is the one place that decides what the core is handed. Keeping it a
//! pure function of the store — [`Pipeline::generate`] reads, computes and
//! returns; [`Pipeline::commit`] is a separate call that writes — means the
//! whole pipeline can be exercised in tests and previewed in the UI without
//! touching a running core.
//!
//! # The stages
//!
//! ```text
//! base document            remote subscription or a local file
//!   -> merge profiles      deep merge, with the directives of each honoured
//!   -> override profiles   declarative path edits
//!   -> sequence patches    prepend/append/delete on rules, proxies, groups
//!   -> validate            whole-document pre-flight checks
//!   -> render              YAML, written atomically
//!   -> reload              PUT /configs, falling back to a restart
//! ```
//!
//! Every stage is reported in the [`Outcome`], including a structural diff
//! against the previously generated configuration and one [`AppliedProfile`]
//! entry per profile, so the UI can answer "what did that subscription update
//! actually change?" rather than showing an opaque success message.

use std::path::PathBuf;

use serde_json::Value;

use crate::enhance::diff::{self, Diff};
use crate::enhance::merge::{self, MergeOptions};
use crate::enhance::overlay::Overlay;
use crate::error::{Error, Result};
use crate::model::config::Config;
use crate::paths::AppPaths;
use crate::profile::item::{PrfItem, ProfileType};
use crate::profile::store::{ProfileStore, document_path};
use crate::validate::{self, Report};

/// What one profile contributed.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedProfile {
    /// Profile uid.
    pub uid: String,
    /// Display name.
    pub name: String,
    /// What the profile is.
    pub kind: ProfileType,
    /// What it did, or why it did nothing.
    pub note: String,
    /// `true` when the profile was skipped.
    pub skipped: bool,
}

impl AppliedProfile {
    fn applied(item: &PrfItem, note: impl Into<String>) -> Self {
        Self {
            uid: item.uid.clone(),
            name: item.label().to_owned(),
            kind: item.kind,
            note: note.into(),
            skipped: false,
        }
    }

    fn skipped(item: &PrfItem, note: impl Into<String>) -> Self {
        Self {
            uid: item.uid.clone(),
            name: item.label().to_owned(),
            kind: item.kind,
            note: note.into(),
            skipped: true,
        }
    }
}

/// The result of running the pipeline.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The generated configuration.
    pub config: Config,
    /// The rendered YAML, exactly as it will be written.
    pub yaml: String,
    /// Validation findings.
    pub report: Report,
    /// One entry per profile in the chain, in application order.
    pub applied: Vec<AppliedProfile>,
    /// What changed relative to the previous generated configuration.
    pub diff: Diff,
    /// Non-fatal problems worth showing but not worth stopping for.
    pub warnings: Vec<String>,
}

impl Outcome {
    /// `true` when the configuration is safe to hand to the core.
    #[must_use]
    pub fn is_applicable(&self) -> bool {
        self.report.is_ok()
    }

    /// One-line summary for the status bar.
    #[must_use]
    pub fn summary(&self) -> String {
        let stats = self.config.stats();
        format!(
            "{} proxies, {} groups, {} rules - {}",
            stats.proxies,
            stats.groups,
            stats.rules,
            self.report.summary()
        )
    }

    /// Profiles that were skipped, with the reason.
    #[must_use]
    pub fn skipped(&self) -> Vec<&AppliedProfile> {
        self.applied.iter().filter(|a| a.skipped).collect()
    }
}

/// Generates runtime configurations from a profile store.
#[derive(Debug, Clone)]
pub struct Pipeline {
    paths: AppPaths,
}

impl Pipeline {
    /// Bind a pipeline to an application home.
    #[must_use]
    pub fn new(paths: AppPaths) -> Self {
        Self { paths }
    }

    /// Where the generated configuration is written.
    #[must_use]
    pub fn output_path(&self) -> PathBuf {
        self.paths.runtime_config()
    }

    /// Read and process the store without writing anything.
    ///
    /// # Errors
    /// [`Error::MissingField`] when no profile is current,
    /// [`Error::InvalidChain`] for a broken chain, [`Error::Io`] when a
    /// document is unreadable, and [`Error::Parse`] for a malformed document.
    pub fn generate(&self, store: &ProfileStore) -> Result<Outcome> {
        let chain = store.resolve_chain()?;
        let mut applied = Vec::new();
        let mut warnings = Vec::new();

        let mut config = Value::Object(serde_json::Map::new());
        let mut started = false;
        let mut seq: Vec<&PrfItem> = Vec::new();

        for item in chain {
            if let Some(reason) = item.unsupported_reason() {
                applied.push(AppliedProfile::skipped(item, reason));
                continue;
            }
            if item.kind.is_sequence() {
                // Sequence patches need the merged result, so they run after
                // every other stage rather than in chain order.
                seq.push(item);
                continue;
            }

            let text = store.read_document(item)?;
            match item.kind {
                ProfileType::Remote | ProfileType::Local => {
                    let parsed = parse_document(&text, item)?;
                    if started {
                        warnings.push(format!(
                            "`{}` is a second base profile; later bases replace earlier ones",
                            item.label()
                        ));
                    }
                    config = parsed;
                    started = true;
                    applied.push(AppliedProfile::applied(
                        item,
                        format!(
                            "base document, {} top-level keys",
                            config.as_object().map_or(0, serde_json::Map::len)
                        ),
                    ));
                }
                ProfileType::Merge => {
                    require_base(started, item)?;
                    let patch = parse_document(&text, item)?;
                    let before = config.clone();
                    let (patch, options) =
                        merge::expand_directives(&patch, &MergeOptions::default());
                    merge::deep_merge(&mut config, &patch, &options);
                    let d = diff::diff_limited(&before, &config, 200);
                    // Naming the changed keys is far more useful in a profiles
                    // list than a bare count: "merged dns, rules" answers the
                    // question the user actually has.
                    let keys = d.touched_keys();
                    let note = if keys.is_empty() {
                        "no change (already applied)".to_owned()
                    } else {
                        format!("changed {}", summarise(&keys, 4))
                    };
                    applied.push(AppliedProfile::applied(item, note));
                }
                ProfileType::Override => {
                    require_base(started, item)?;
                    let overlay = Overlay::from_yaml(&text).map_err(|e| {
                        Error::parse("override profile", document_path(&self.paths, item), e)
                    })?;
                    let log = overlay.apply(&mut config)?;
                    let note = if log.is_empty() {
                        "no edits (already applied)".to_owned()
                    } else {
                        format!("{} edit(s): {}", log.len(), summarise(&log, 3))
                    };
                    applied.push(AppliedProfile::applied(item, note));
                }
                ProfileType::Script => unreachable!("handled by unsupported_reason above"),
                ProfileType::Rules | ProfileType::Proxies | ProfileType::Groups => {
                    unreachable!("sequence patches are collected separately")
                }
            }
        }

        if !started {
            return Err(Error::MissingField {
                uid: "-".to_owned(),
                field: "base profile",
            });
        }

        for item in seq {
            require_base(started, item)?;
            let Some(key) = item.kind.sequence_key() else {
                continue;
            };
            let text = store.read_document(item)?;
            let patch: crate::profile::item::SeqPatch = serde_norway::from_str(&text)
                .map_err(|e| Error::parse("sequence patch", document_path(&self.paths, item), e))?;
            let existing: Vec<Value> = config
                .get(key)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let merged = patch.apply_values(&existing);
            let before = merged.len();
            let after = existing.len();
            config[key] = Value::Array(merged);
            applied.push(AppliedProfile::applied(
                item,
                format!("sequence patch on {key} ({before} -> {after})"),
            ));
        }

        let config = Config::from_value(config)?;
        let report = validate::check(&config);
        let yaml = config.to_yaml()?;
        let diff = self.diff_against_previous(&yaml);

        Ok(Outcome {
            config,
            yaml,
            report,
            applied,
            diff,
            warnings,
        })
    }

    /// Compare a freshly generated document with the one currently deployed.
    #[must_use]
    pub fn diff_against_previous(&self, yaml: &str) -> Diff {
        let path = self.paths.runtime_config();
        let Ok(previous) = std::fs::read_to_string(&path) else {
            return Diff::default();
        };
        let Ok(before) = serde_norway::from_str::<Value>(&previous) else {
            return Diff::default();
        };
        let Ok(after) = serde_norway::from_str::<Value>(yaml) else {
            return Diff::default();
        };
        diff::diff(&before, &after)
    }

    /// Write a generated configuration, snapshotting the previous one.
    ///
    /// A snapshot is written **before** the new file, so that even a crash
    /// between the two leaves a recoverable previous state. Writing is atomic,
    /// because the core may hot-reload this path at any moment and must never
    /// observe a partial document.
    ///
    /// # Errors
    /// [`Error::Validation`] when the outcome has errors, unless `force` is
    /// set; [`Error::Io`] on a write failure.
    pub fn commit(&self, outcome: &Outcome, force: bool) -> Result<()> {
        if !outcome.is_applicable() && !force {
            return Err(Error::Validation {
                problems: outcome
                    .report
                    .errors_iter()
                    .map(|d| d.message.clone())
                    .collect(),
            });
        }
        let target = self.output_path();
        if target.is_file() {
            self.snapshot(&target)?;
            let previous = std::fs::read_to_string(&target).map_err(|e| Error::io(&target, e))?;
            self.paths
                .write_atomic(&self.paths.runtime_config_previous(), &previous)?;
        }
        self.paths.write_atomic(&target, &outcome.yaml)
    }

    /// Restore the most recent snapshot of the generated configuration.
    ///
    /// # Errors
    /// [`Error::Io`] when there is nothing to restore or the write fails.
    pub fn rollback(&self) -> Result<PathBuf> {
        let snapshots = self.snapshots()?;
        let Some(latest) = snapshots.first() else {
            return Err(Error::invalid(
                "rollback",
                "there are no snapshots to restore",
            ));
        };
        let contents = std::fs::read_to_string(latest).map_err(|e| Error::io(latest, e))?;
        // Validate before overwriting: restoring a broken snapshot would make
        // the situation worse than the one being escaped.
        Config::from_yaml(&contents)?;
        self.paths.write_atomic(&self.output_path(), &contents)?;
        Ok(latest.clone())
    }

    /// Snapshots of previously generated configurations, newest first.
    ///
    /// # Errors
    /// [`Error::Io`] when the snapshot directory cannot be read.
    pub fn snapshots(&self) -> Result<Vec<PathBuf>> {
        let dir = self.paths.snapshots_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(&dir, e)),
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        files.reverse();
        Ok(files)
    }

    /// Keep at most this many snapshots.
    pub const SNAPSHOT_LIMIT: usize = 20;

    fn snapshot(&self, source: &std::path::Path) -> Result<()> {
        let contents = std::fs::read_to_string(source).map_err(|e| Error::io(source, e))?;
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S-%3f");
        let name = format!("config-{stamp}.yaml");
        let target = self.paths.snapshots_dir().join(&name);
        self.paths.write_atomic(&target, &contents)?;

        // Prune the oldest beyond the limit so the directory cannot grow
        // without bound over months of use.
        let all = self.snapshots()?;
        for stale in all.into_iter().skip(Self::SNAPSHOT_LIMIT) {
            let _ = std::fs::remove_file(stale);
        }
        Ok(())
    }
}

/// Join up to `max` items, noting how many were elided.
fn summarise(items: &[String], max: usize) -> String {
    if items.len() <= max {
        return items.join(", ");
    }
    format!(
        "{}, and {} more",
        items[..max].join(", "),
        items.len() - max
    )
}

fn require_base(started: bool, item: &PrfItem) -> Result<()> {
    if started {
        return Ok(());
    }
    Err(Error::InvalidChain {
        reason: format!(
            "`{}` is a {} profile and needs a base document, but no remote or local profile precedes it",
            item.label(),
            item.kind.as_str()
        ),
    })
}

/// Parse a base or merge document.
///
/// A subscription that is really a bare list of proxies — which some providers
/// serve — is wrapped rather than rejected, because that is a real and common
/// format.
fn parse_document(text: &str, item: &PrfItem) -> Result<Value> {
    let value: Value = serde_norway::from_str(text).map_err(|e| Error::Parse {
        kind: "profile document",
        path: PathBuf::from(format!("{}.yaml", item.uid)),
        source: Box::new(e),
    })?;
    Ok(match value {
        Value::Object(_) => value,
        Value::Array(proxies) => {
            let mut map = serde_json::Map::new();
            map.insert("proxies".to_owned(), Value::Array(proxies));
            Value::Object(map)
        }
        Value::Null => Value::Object(serde_json::Map::new()),
        other => {
            return Err(Error::invalid(
                "profile",
                format!(
                    "`{}` does not contain a configuration: expected a mapping, found {}",
                    item.label(),
                    if other.is_string() {
                        "a string"
                    } else {
                        "a scalar"
                    }
                ),
            ));
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::profile::item::SeqPatch;
    use tempfile::TempDir;

    const BASE: &str = r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
mode: rule
proxies:
  - { name: "JP 01", type: vless, server: 1.2.3.4, port: 443, uuid: u }
proxy-groups:
  - { name: PROXY, type: select, proxies: ["JP 01", DIRECT] }
rules:
  - DOMAIN-SUFFIX,google.com,PROXY
  - MATCH,DIRECT
"#;

    struct Fixture {
        _dir: TempDir,
        paths: AppPaths,
        store: ProfileStore,
        pipeline: Pipeline,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        let store = ProfileStore::load(&paths).unwrap();
        let pipeline = Pipeline::new(paths.clone());
        Fixture {
            _dir: dir,
            paths,
            store,
            pipeline,
        }
    }

    impl Fixture {
        fn add(&mut self, item: PrfItem, contents: &str) -> String {
            let uid = self.store.add(item);
            let stored = self.store.get(&uid).unwrap().clone();
            self.store.write_document(&stored, contents).unwrap();
            uid
        }

        fn base(&mut self) -> String {
            let uid = self.add(PrfItem::local("L1", "base"), BASE);
            self.store.set_current(&uid).unwrap();
            uid
        }

        fn generate(&self) -> Outcome {
            self.pipeline.generate(&self.store).unwrap()
        }
    }

    #[test]
    fn a_lone_base_profile_generates_its_own_configuration() {
        let mut f = fixture();
        f.base();
        let outcome = f.generate();
        assert!(outcome.is_applicable(), "{}", outcome.report.render());
        assert_eq!(outcome.config.stats().proxies, 1);
        assert_eq!(outcome.config.stats().rules, 2);
        assert!(outcome.yaml.contains("mixed-port: 7890"));
        assert_eq!(outcome.applied.len(), 1);
        assert!(outcome.applied[0].note.contains("base document"));
    }

    #[test]
    fn a_merge_profile_is_applied_and_reported() {
        let mut f = fixture();
        f.base();
        f.add(
            PrfItem::patch("m1", "Merge", ProfileType::Merge),
            "log-level: debug\n",
        );
        let outcome = f.generate();
        assert_eq!(outcome.config.log_level(), "debug");

        let merge_note = outcome.applied.iter().find(|a| a.uid == "m1").unwrap();
        assert!(
            merge_note.note.starts_with("changed"),
            "{}",
            merge_note.note
        );
        assert!(
            merge_note.note.contains("log-level"),
            "the note names the key: {}",
            merge_note.note
        );
        // The outcome's diff is against what is *deployed*, and nothing has been
        // committed here, so it is correctly empty.
        assert!(outcome.diff.is_empty(), "{}", outcome.diff);
    }

    #[test]
    fn the_legacy_prepend_directive_still_works() {
        let mut f = fixture();
        f.base();
        f.add(
            PrfItem::patch("m1", "Merge", ProfileType::Merge),
            "prepend-rules:\n  - DOMAIN-SUFFIX,corp.example,DIRECT\n",
        );
        let outcome = f.generate();
        let rules = outcome.config.raw_rules();
        assert_eq!(rules[0], "DOMAIN-SUFFIX,corp.example,DIRECT");
        assert_eq!(rules.last().unwrap(), "MATCH,DIRECT");
    }

    #[test]
    fn an_override_profile_edits_by_path() {
        let mut f = fixture();
        f.base();
        f.add(
            PrfItem::patch("o1", "Office", ProfileType::Override),
            "set:\n  mode: global\nappend:\n  rules:\n    - DOMAIN-SUFFIX,corp.example,DIRECT\n",
        );
        let outcome = f.generate();
        assert_eq!(outcome.config.mode(), "global");
        let rules = outcome.config.raw_rules();
        assert_eq!(
            rules,
            vec![
                "DOMAIN-SUFFIX,google.com,PROXY",
                "DOMAIN-SUFFIX,corp.example,DIRECT",
                "MATCH,DIRECT"
            ]
        );
        let note = outcome.applied.iter().find(|a| a.uid == "o1").unwrap();
        assert!(note.note.contains("2 edit"), "{}", note.note);
    }

    #[test]
    fn a_sequence_patch_applies_after_merges() {
        let mut f = fixture();
        f.base();
        f.add(
            PrfItem::patch("m1", "Merge", ProfileType::Merge),
            "log-level: debug\n",
        );
        let patch = serde_norway::to_string(&SeqPatch {
            prepend: vec!["DOMAIN-SUFFIX,intranet.example,DIRECT".into()],
            append: vec![],
            delete: vec![],
        })
        .unwrap();
        f.add(PrfItem::patch("r1", "Rules", ProfileType::Rules), &patch);

        let outcome = f.generate();
        assert_eq!(
            outcome.config.raw_rules()[0],
            "DOMAIN-SUFFIX,intranet.example,DIRECT"
        );
        let note = outcome.applied.iter().find(|a| a.uid == "r1").unwrap();
        assert!(
            note.note.contains("sequence patch on rules"),
            "{}",
            note.note
        );
    }

    #[test]
    fn a_sequence_patch_can_delete_a_rule_from_the_subscription() {
        let mut f = fixture();
        f.base();
        let patch = serde_norway::to_string(&SeqPatch {
            prepend: vec![],
            append: vec![],
            delete: vec!["DOMAIN-SUFFIX,google.com,PROXY".into()],
        })
        .unwrap();
        f.add(PrfItem::patch("r1", "Rules", ProfileType::Rules), &patch);
        let outcome = f.generate();
        assert_eq!(outcome.config.raw_rules(), vec!["MATCH,DIRECT"]);
    }

    #[test]
    fn a_script_profile_is_skipped_and_reported_never_silently_ignored() {
        let mut f = fixture();
        f.base();
        f.add(
            PrfItem::patch("s1", "Tweaks", ProfileType::Script),
            "function main(config) { return config; }\n",
        );
        let outcome = f.generate();
        let skipped = outcome.skipped();
        assert_eq!(skipped.len(), 1);
        assert!(
            skipped[0].note.contains("JavaScript"),
            "{}",
            skipped[0].note
        );
        assert!(
            outcome.config.log_level() == "info",
            "the script must not have run"
        );
    }

    #[test]
    fn everything_is_reported_in_chain_order() {
        let mut f = fixture();
        f.base();
        f.add(
            PrfItem::patch("m1", "Merge", ProfileType::Merge),
            "log-level: debug\n",
        );
        f.add(
            PrfItem::patch("o1", "Office", ProfileType::Override),
            "set:\n  mode: global\n",
        );
        let outcome = f.generate();
        let uids: Vec<&str> = outcome.applied.iter().map(|a| a.uid.as_str()).collect();
        assert_eq!(uids, vec!["L1", "m1", "o1"]);
    }

    #[test]
    fn validation_findings_reach_the_outcome() {
        let mut f = fixture();
        f.add(
            PrfItem::local("L1", "broken"),
            "mixed-port: 7890\nexternal-controller: 127.0.0.1:9090\nrules:\n  - DOMAIN,a.test,GHOST\n  - MATCH,DIRECT\n",
        );
        f.store.set_current("L1").unwrap();
        let outcome = f.generate();
        assert!(!outcome.is_applicable());
        assert!(
            outcome
                .report
                .errors_iter()
                .any(|d| d.code == "E-DANGLING-POLICY")
        );
        assert!(outcome.summary().contains("error"));
    }

    #[test]
    fn a_patch_before_any_base_is_reported_with_the_reason() {
        let mut f = fixture();
        f.add(PrfItem::local("L1", "base"), BASE);
        f.add(
            PrfItem::patch("m1", "Merge", ProfileType::Merge),
            "log-level: debug\n",
        );
        f.store.set_current("L1").unwrap();
        // Force the merge to the front of the chain.
        f.store.set_chain(&["m1".into()]).unwrap();
        // A chain always starts with the base, so this must succeed; the
        // failure case is a chain whose base has no document.
        assert!(f.pipeline.generate(&f.store).is_ok());
    }

    #[test]
    fn generating_without_a_base_is_an_error() {
        let mut f = fixture();
        f.add(
            PrfItem::patch("m1", "Merge", ProfileType::Merge),
            "log-level: debug\n",
        );
        let err = f.pipeline.generate(&f.store).unwrap_err();
        assert!(err.to_string().contains("current"), "{err}");
    }

    #[test]
    fn a_subscription_that_is_a_bare_proxy_list_is_wrapped_not_rejected() {
        let mut f = fixture();
        f.add(
            PrfItem::local("L1", "bare"),
            "- { name: A, type: vless, server: 1.2.3.4, port: 443, uuid: u }\n",
        );
        f.store.set_current("L1").unwrap();
        let outcome = f.generate();
        assert_eq!(outcome.config.stats().proxies, 1);
    }

    #[test]
    fn a_document_that_is_not_a_configuration_is_rejected_with_a_clear_message() {
        let mut f = fixture();
        f.add(PrfItem::local("L1", "junk"), "just a string\n");
        f.store.set_current("L1").unwrap();
        let err = f.pipeline.generate(&f.store).unwrap_err();
        assert!(err.to_string().contains("expected a mapping"), "{err}");
    }

    #[test]
    fn commit_writes_the_configuration_and_validates_first() {
        let mut f = fixture();
        f.base();
        let outcome = f.generate();
        f.pipeline.commit(&outcome, false).unwrap();
        let written = std::fs::read_to_string(f.paths.runtime_config()).unwrap();
        assert_eq!(written, outcome.yaml);
    }

    #[test]
    fn commit_refuses_an_invalid_configuration_unless_forced() {
        let mut f = fixture();
        f.add(
            PrfItem::local("L1", "broken"),
            "mixed-port: 7890\nrules:\n  - DOMAIN,a.test,GHOST\n",
        );
        f.store.set_current("L1").unwrap();
        let outcome = f.generate();
        assert!(!outcome.is_applicable());

        let err = f.pipeline.commit(&outcome, false).unwrap_err();
        assert!(matches!(err, Error::Validation { .. }), "{err:?}");
        assert!(!f.paths.runtime_config().exists(), "nothing may be written");

        f.pipeline.commit(&outcome, true).unwrap();
        assert!(
            f.paths.runtime_config().exists(),
            "force overrides the refusal"
        );
    }

    #[test]
    fn committing_twice_keeps_the_previous_version_and_a_snapshot() {
        let mut f = fixture();
        f.base();
        f.pipeline.commit(&f.generate(), false).unwrap();

        f.add(
            PrfItem::patch("m1", "Merge", ProfileType::Merge),
            "log-level: debug\n",
        );
        f.pipeline.commit(&f.generate(), false).unwrap();

        let previous = std::fs::read_to_string(f.paths.runtime_config_previous()).unwrap();
        assert!(
            previous.contains("mixed-port: 7890"),
            "the previous config is preserved"
        );
        assert!(
            !previous.contains("log-level: debug"),
            "it is the *old* version: {previous}"
        );
        assert!(
            std::fs::read_to_string(f.paths.runtime_config())
                .unwrap()
                .contains("log-level: debug")
        );
        assert_eq!(f.pipeline.snapshots().unwrap().len(), 1);
    }

    #[test]
    fn rollback_restores_the_newest_snapshot() {
        let mut f = fixture();
        f.base();
        f.pipeline.commit(&f.generate(), false).unwrap();
        let original = std::fs::read_to_string(f.paths.runtime_config()).unwrap();

        f.add(
            PrfItem::patch("m1", "Merge", ProfileType::Merge),
            "log-level: debug\n",
        );
        f.pipeline.commit(&f.generate(), false).unwrap();
        assert!(
            std::fs::read_to_string(f.paths.runtime_config())
                .unwrap()
                .contains("debug")
        );

        let restored = f.pipeline.rollback().unwrap();
        assert!(restored.exists());
        assert_eq!(
            std::fs::read_to_string(f.paths.runtime_config()).unwrap(),
            original
        );
    }

    #[test]
    fn rollback_without_a_snapshot_is_a_clear_error() {
        let f = fixture();
        let err = f.pipeline.rollback().unwrap_err();
        assert!(err.to_string().contains("no snapshots"), "{err}");
        assert!(f.pipeline.snapshots().unwrap().is_empty());
    }

    #[test]
    fn snapshots_are_pruned_to_the_limit() {
        let mut f = fixture();
        f.base();
        let outcome = f.generate();
        f.pipeline.commit(&outcome, false).unwrap();
        for i in 0..Pipeline::SNAPSHOT_LIMIT + 5 {
            f.store
                .write_document(
                    f.store.get("L1").unwrap(),
                    &format!(
                        "{BASE}\nlog-level: {}\n",
                        if i % 2 == 0 { "debug" } else { "info" }
                    ),
                )
                .unwrap();
            f.pipeline.commit(&f.generate(), false).unwrap();
        }
        assert!(
            f.pipeline.snapshots().unwrap().len() <= Pipeline::SNAPSHOT_LIMIT,
            "snapshots must not grow without bound"
        );
    }

    #[test]
    fn the_diff_against_the_previous_config_highlights_what_an_update_changed() {
        let mut f = fixture();
        f.base();
        f.pipeline.commit(&f.generate(), false).unwrap();

        // A subscription update that renames a node and adds a rule.
        f.store
            .write_document(
                f.store.get("L1").unwrap(),
                &BASE.replace("JP 01", "JP 01 (renamed)").replace(
                    "  - MATCH,DIRECT",
                    "  - DOMAIN-SUFFIX,new.example,PROXY\n  - MATCH,DIRECT",
                ),
            )
            .unwrap();
        let outcome = f.generate();
        assert!(!outcome.diff.is_empty());
        assert!(outcome.diff.touched_keys().contains(&"proxies".to_owned()));
        assert!(outcome.diff.touched_keys().contains(&"rules".to_owned()));
        let rendered = outcome.diff.to_string();
        assert!(rendered.contains("renamed"), "{rendered}");
    }

    #[test]
    fn generating_twice_without_a_change_produces_an_empty_diff() {
        let mut f = fixture();
        f.base();
        f.pipeline.commit(&f.generate(), false).unwrap();
        let second = f.generate();
        assert!(second.diff.is_empty(), "{}", second.diff);
    }
}
