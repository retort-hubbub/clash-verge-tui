//! The profile index: load, edit, save, and resolve the pipeline chain.
//!
//! The index is a single YAML document. It is written atomically, so a crash
//! during a save cannot leave a half-written index that loses every
//! subscription, and it is never rewritten unless something actually changed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::AppPaths;
use crate::profile::item::{PrfItem, ProfileType, SelectedNode};

/// The on-disk profile index.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Index {
    /// Uid of the profile that supplies the base document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// Explicit ordered list of patch profiles to apply after the base.
    ///
    /// When empty, ordering falls back to the `clash-verge-rev` convention:
    /// the current profile's `option` references first, then every remaining
    /// patch in type order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain: Vec<String>,
    /// Every known profile.
    #[serde(default)]
    pub items: Vec<PrfItem>,
}

/// Whether a string is one plain path component.
///
/// A uid becomes a file name, which makes it the one field of an index entry
/// that can reach outside the profiles directory. Separators, the two
/// directory names, and anything empty are refused; the rest of a uid's shape
/// is not this function's business.
fn is_plain_component(uid: &str) -> bool {
    !uid.is_empty() && uid != "." && uid != ".." && !uid.contains(['/', '\\', '\0'])
}

/// Where a profile's document is stored.
#[must_use]
pub fn document_path(paths: &AppPaths, item: &PrfItem) -> PathBuf {
    paths.profiles_dir().join(item.file_name())
}

/// Read and write access to the profile index and its documents.
#[derive(Debug, Clone)]
pub struct ProfileStore {
    paths: AppPaths,
    index: Index,
}

impl ProfileStore {
    /// Load the index, creating an empty one when the file does not exist.
    ///
    /// A malformed index is an error rather than being silently replaced: it
    /// is the user's only record of their subscriptions.
    ///
    /// # Errors
    /// [`Error::Parse`] for malformed YAML, [`Error::Io`] for an unreadable
    /// file.
    pub fn load(paths: &AppPaths) -> Result<Self> {
        let path = paths.profiles_index();
        let index = if path.is_file() {
            let text = paths.read(&path)?;
            if text.trim().is_empty() {
                Index::default()
            } else {
                serde_norway::from_str(&text)
                    .map_err(|e| Error::parse("profile index", &path, e))?
            }
        } else {
            Index::default()
        };
        Ok(Self {
            paths: paths.clone(),
            index,
        })
    }

    /// The paths this store writes to.
    #[must_use]
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// The in-memory index.
    #[must_use]
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Persist the index atomically.
    ///
    /// # Errors
    /// [`Error::Serialize`] or [`Error::Io`].
    pub fn save(&self) -> Result<()> {
        let yaml = serde_norway::to_string(&self.index)
            .map_err(|e| Error::serialize("profile index", e))?;
        self.paths.write_atomic(&self.paths.profiles_index(), &yaml)
    }

    /// All profiles, in index order.
    #[must_use]
    pub fn items(&self) -> &[PrfItem] {
        &self.index.items
    }

    /// Look a profile up by uid.
    #[must_use]
    pub fn get(&self, uid: &str) -> Option<&PrfItem> {
        self.index.items.iter().find(|i| i.uid == uid)
    }

    /// Look a profile up by uid for editing.
    pub fn get_mut(&mut self, uid: &str) -> Option<&mut PrfItem> {
        self.index.items.iter_mut().find(|i| i.uid == uid)
    }

    /// Profiles of one type.
    #[must_use]
    pub fn of_type(&self, kind: ProfileType) -> Vec<&PrfItem> {
        self.index.items.iter().filter(|i| i.kind == kind).collect()
    }

    /// The profile supplying the base document.
    #[must_use]
    pub fn current(&self) -> Option<&PrfItem> {
        self.index.current.as_deref().and_then(|uid| self.get(uid))
    }

    /// Uid of the current profile.
    #[must_use]
    pub fn current_uid(&self) -> Option<&str> {
        self.index.current.as_deref()
    }

    /// Make a profile current.
    ///
    /// # Errors
    /// [`Error::ProfileNotFound`] for an unknown uid, or
    /// [`Error::InvalidValue`] when the profile cannot supply a base document.
    pub fn set_current(&mut self, uid: &str) -> Result<()> {
        let item = self.get(uid).ok_or_else(|| Error::ProfileNotFound {
            uid: uid.to_owned(),
        })?;
        if item.kind.is_patch() {
            return Err(Error::invalid(
                "profile",
                format!(
                    "`{}` is a {} profile and cannot be the base; choose a remote or local profile",
                    item.label(),
                    item.kind.as_str()
                ),
            ));
        }
        self.index.current = Some(uid.to_owned());
        Ok(())
    }

    /// Add a profile, generating a uid when it has none.
    ///
    /// The filename is normalised to match the uid. A mismatch would be a
    /// silent data-loss bug: the index would name one file and the next load
    /// would read another.
    ///
    /// Returns the uid that was assigned.
    pub fn add(&mut self, mut item: PrfItem) -> String {
        // The funnel every caller goes through, so the file-name guard belongs
        // here as well as at the import: a uid that is not one plain path
        // component would put the document outside the profiles directory.
        if !is_plain_component(&item.uid) {
            item.uid = self.generate_uid(item.kind);
        }
        // A uid the index already holds is reassigned rather than honoured.
        // Keeping it would put two entries in the index with the same uid and
        // therefore the same document, so the second would overwrite the
        // first's file and the first would start reporting the second's
        // contents — a collision the caller cannot see coming, because the uid
        // is what the caller just supplied.
        if item.uid.is_empty() || self.get(&item.uid).is_some() {
            item.uid = self.generate_uid(item.kind);
        }
        let expected = item.default_file_name();
        if item.file.as_deref() != Some(expected.as_str()) {
            item.file = Some(expected);
        }
        let uid = item.uid.clone();
        self.index.items.push(item);
        uid
    }

    /// Remove a profile and its document.
    ///
    /// # Errors
    /// [`Error::Io`] when the document exists but cannot be deleted.
    pub fn remove(&mut self, uid: &str) -> Result<Option<PrfItem>> {
        let Some(position) = self.index.items.iter().position(|i| i.uid == uid) else {
            return Ok(None);
        };
        let item = self.index.items.remove(position);
        self.index.chain.retain(|u| u != uid);
        if self.index.current.as_deref() == Some(uid) {
            self.index.current = None;
        }
        let path = document_path(&self.paths, &item);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| Error::io(&path, e))?;
        }
        Ok(Some(item))
    }

    /// Rename a profile. Does not rename its document, which keeps the uid and
    /// the filename stable across renames.
    ///
    /// # Errors
    /// [`Error::ProfileNotFound`] for an unknown uid.
    pub fn rename(&mut self, uid: &str, name: &str) -> Result<()> {
        let item = self.get_mut(uid).ok_or_else(|| Error::ProfileNotFound {
            uid: uid.to_owned(),
        })?;
        name.clone_into(&mut item.name);
        Ok(())
    }

    /// Point a remote profile at a different subscription URL.
    ///
    /// Only the URL changes: the uid, the name and the document on disk stay
    /// where they are, so a chain that names this profile keeps working and a
    /// `config generate` between this and the next fetch still sees a document.
    /// The document is deliberately *not* touched — it is the old provider's
    /// until a fetch replaces it, and pretending otherwise would mean a
    /// half-updated profile whose index and content disagree.
    ///
    /// # Errors
    /// [`Error::ProfileNotFound`] for an unknown uid, and
    /// [`Error::InvalidValue`] for a profile that has no URL to change.
    pub fn set_url(&mut self, uid: &str, url: &str) -> Result<()> {
        let item = self.get_mut(uid).ok_or_else(|| Error::ProfileNotFound {
            uid: uid.to_owned(),
        })?;
        if item.url.is_none() {
            return Err(Error::invalid(
                "url",
                format!(
                    "`{}` is a {} profile, which has no subscription URL to change",
                    item.label(),
                    item.kind.as_str()
                ),
            ));
        }
        url.clone_into(item.url.as_mut().unwrap_or(&mut String::new()));
        Ok(())
    }

    /// Replace the explicit chain.
    ///
    /// # Errors
    /// [`Error::ProfileNotFound`] for an unknown uid, and
    /// [`Error::InvalidValue`] when a base profile is listed.
    pub fn set_chain(&mut self, uids: &[String]) -> Result<()> {
        for uid in uids {
            let item = self
                .get(uid)
                .ok_or_else(|| Error::ProfileNotFound { uid: uid.clone() })?;
            if item.kind.is_base() {
                return Err(Error::invalid(
                    "chain",
                    format!(
                        "`{}` is a {} profile; the chain may only contain patches",
                        item.label(),
                        item.kind.as_str()
                    ),
                ));
            }
        }
        self.index.chain = uids.to_vec();
        Ok(())
    }

    /// Remember which member of a group the user picked.
    ///
    /// Recorded on the *current* profile, because that is the one a reload
    /// rebuilds and therefore the one whose choices have to be replayed. A
    /// store with no current profile records nothing: there is no
    /// configuration for the choice to belong to.
    ///
    /// # Errors
    /// [`Error::ProfileNotFound`] when the current uid is not in the index.
    pub fn remember_selection(&mut self, group: &str, member: &str) -> Result<()> {
        self.amend_selection(group, Some(member))
    }

    /// Forget what was remembered for a group.
    ///
    /// # Errors
    /// As [`ProfileStore::remember_selection`].
    pub fn forget_selection(&mut self, group: &str) -> Result<()> {
        self.amend_selection(group, None)
    }

    fn amend_selection(&mut self, group: &str, member: Option<&str>) -> Result<()> {
        let Some(uid) = self.current_uid().map(str::to_owned) else {
            return Ok(());
        };
        // Nothing to record for a blank group or member: storing one would put
        // an entry in the index that the reader has to filter out again.
        if group.trim().is_empty() {
            return Ok(());
        }
        if member.is_some_and(|member| member.trim().is_empty()) {
            return Ok(());
        }
        let item = self
            .get_mut(&uid)
            .ok_or_else(|| Error::ProfileNotFound { uid: uid.clone() })?;
        match member {
            Some(member) => match item.selected.iter_mut().find(|s| s.name == group) {
                Some(recorded) => member.clone_into(&mut recorded.now),
                None => item.selected.push(SelectedNode::new(group, member)),
            },
            None => item.selected.retain(|s| s.name != group),
        }
        Ok(())
    }

    /// What the current profile remembers.
    #[must_use]
    pub fn selections(&self) -> Vec<SelectedNode> {
        self.current()
            .map(|item| item.selected.clone())
            .unwrap_or_default()
            .into_iter()
            // A placeholder for a group nobody has chosen in is not a choice,
            // and a blank group name would send the replay after `/group/`.
            .filter(SelectedNode::is_usable)
            .collect()
    }

    /// The ordered profiles that produce the runtime configuration.
    ///
    /// Resolution order:
    ///
    /// 1. the base, from `current`;
    /// 2. the explicit `chain`, when set;
    /// 3. otherwise the current profile's `option` references, followed by
    ///    every other patch profile in type order — which is how an imported
    ///    `clash-verge-rev` index behaves.
    ///
    /// # Errors
    /// [`Error::MissingField`] when nothing is current, and
    /// [`Error::InvalidChain`] for a dangling or cyclic reference.
    pub fn resolve_chain(&self) -> Result<Vec<&PrfItem>> {
        let base = self.current().ok_or_else(|| Error::MissingField {
            uid: "-".to_owned(),
            field: "current",
        })?;
        let mut chain: Vec<&PrfItem> = vec![base];

        if self.index.chain.is_empty() {
            self.append_automatic(&mut chain)?;
        } else {
            self.append_explicit(&mut chain)?;
        }

        // A profile that appears twice would be applied twice, and a patch
        // applied twice is not idempotent once it prepends.
        let mut seen = HashSet::new();
        for item in &chain {
            if !seen.insert(item.uid.as_str()) {
                return Err(Error::InvalidChain {
                    reason: format!("`{}` appears more than once in the chain", item.label()),
                });
            }
        }
        Ok(chain)
    }

    /// Append the patches named by the explicit `chain` field, in that order.
    fn append_explicit<'a>(&'a self, chain: &mut Vec<&'a PrfItem>) -> Result<()> {
        for uid in &self.index.chain {
            let item = self.get(uid).ok_or_else(|| Error::InvalidChain {
                reason: format!("chain references `{uid}`, which does not exist"),
            })?;
            chain.push(item);
        }
        Ok(())
    }

    /// Append patches in the order a `clash-verge-rev` index implies.
    ///
    /// The base profile's `option` references come first, because they are an
    /// explicit instruction; every remaining patch follows in pipeline order,
    /// so an index that leaves `option` empty still applies its merges.
    fn append_automatic<'a>(&'a self, chain: &mut Vec<&'a PrfItem>) -> Result<()> {
        let Some(base) = chain.first().copied() else {
            return Ok(());
        };
        for uid in base.option.referenced_uids() {
            let item = self.get(uid).ok_or_else(|| Error::InvalidChain {
                reason: format!(
                    "`{}` references `{uid}` through its options, but it does not exist",
                    base.label()
                ),
            })?;
            if !chain.contains(&item) {
                chain.push(item);
            }
        }
        // Snapshot what is already present so the filter does not hold a
        // borrow of `chain` across the mutation below.
        let taken: HashSet<&str> = chain.iter().map(|i| i.uid.as_str()).collect();
        let mut rest: Vec<&'a PrfItem> = self
            .index
            .items
            .iter()
            .filter(|i| i.kind.is_patch() && !taken.contains(i.uid.as_str()))
            .collect();
        rest.sort_by_key(|i| (i.kind.order(), i.uid.clone()));
        chain.extend(rest);
        Ok(())
    }

    /// Read a profile's document.
    ///
    /// # Errors
    /// [`Error::Io`] when the file is missing or unreadable.
    pub fn read_document(&self, item: &PrfItem) -> Result<String> {
        let path = document_path(&self.paths, item);
        self.paths.read(&path)
    }

    /// Write a profile's document atomically.
    ///
    /// # Errors
    /// [`Error::Io`] on failure.
    pub fn write_document(&self, item: &PrfItem, contents: &str) -> Result<()> {
        let path = document_path(&self.paths, item);
        self.paths.write_atomic(&path, contents)
    }

    /// Generate a uid that is not already taken.
    #[must_use]
    pub fn generate_uid(&self, kind: ProfileType) -> String {
        let prefix = kind.file_prefix();
        for attempt in 0..1000u32 {
            let candidate = format!("{prefix}{}", random_suffix(attempt));
            if !self.index.items.iter().any(|i| i.uid == candidate) {
                return candidate;
            }
        }
        // Astronomically unlikely; fall back to something certainly unique.
        format!("{prefix}{}", self.index.items.len() + 1)
    }

    /// Copy profiles from an existing `clash-verge-rev` installation.
    ///
    /// Documents are copied verbatim, so an imported subscription keeps working
    /// without a network fetch. An existing uid is never overwritten: a
    /// collision gets a fresh uid and the document is copied under the new
    /// name, which makes importing the same directory twice safe.
    ///
    /// # Errors
    /// [`Error::Io`] when the source index or a document cannot be read, and
    /// [`Error::Parse`] when the source index is malformed.
    pub fn import_from(&mut self, source: &Path) -> Result<ImportReport> {
        let index_path = source.join("profiles.yaml");
        let text = std::fs::read_to_string(&index_path).map_err(|e| Error::io(&index_path, e))?;
        let foreign: Index = serde_norway::from_str(&text)
            .map_err(|e| Error::parse("profile index", &index_path, e))?;

        let mut report = ImportReport::default();
        let source_profiles = source.join("profiles");
        let mut imported_current: Option<String> = None;

        for mut item in foreign.items {
            let was_current = foreign.current.as_deref() == Some(item.uid.as_str());
            // The document lives under the *source* filename, which is not
            // necessarily `{uid}.yaml` for hand-edited installations.
            let source_file = item.file_name();
            let ext = if item.kind == ProfileType::Script {
                "js"
            } else {
                "yaml"
            };

            // A foreign index is untrusted input, and its uid becomes a file
            // name here: `uid: "../profiles"` would put the document at
            // `profiles/../profiles.yaml` — the index itself. That was a path
            // traversal, not a naming quirk.
            let mut uid = item.uid.clone();
            let mut reassigned = !is_plain_component(&uid);

            // The uid may be free while the *document* is not, and the
            // document that matters is the one about to be written — named
            // after the uid. Checking the *source* file name instead meant the
            // protection did nothing whenever the two differed, which is
            // exactly the hand-edited installation it exists for.
            let destination_for =
                |uid: &str| self.paths.profiles_dir().join(format!("{uid}.{ext}"));
            while self.get(&uid).is_some() || destination_for(&uid).exists() {
                uid = self.generate_uid(item.kind);
                reassigned = true;
            }
            if reassigned {
                report.renamed += 1;
            }
            item.uid = uid;
            item.file = Some(format!("{}.{ext}", item.uid));

            let uid = self.add(item);
            let destination = document_path(
                &self.paths,
                self.get(&uid).expect("the item was just added"),
            );
            let origin = source_profiles.join(&source_file);
            if origin.is_file() {
                std::fs::copy(&origin, &destination).map_err(|e| Error::io(&destination, e))?;
                report.documents_copied += 1;
            }
            if was_current {
                imported_current = Some(uid);
            }
            report.imported += 1;
        }

        // Only adopt the imported selection when this store had none, so an
        // import never silently repoints an existing setup.
        if self.index.current.is_none()
            && let Some(uid) = imported_current
        {
            self.index.current = Some(uid);
        }
        Ok(report)
    }
}

/// What an import did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Profiles added.
    pub imported: usize,
    /// Profiles whose uid collided and had to change.
    pub renamed: usize,
    /// Documents copied into the profiles directory.
    pub documents_copied: usize,
}

impl ImportReport {
    /// One-line summary.
    #[must_use]
    pub fn summary(&self) -> String {
        use std::fmt::Write as _;
        let mut s = format!("imported {} profile(s)", self.imported);
        if self.renamed > 0 {
            let _ = write!(s, ", {} renamed to avoid a collision", self.renamed);
        }
        if self.documents_copied > 0 {
            let _ = write!(s, ", {} document(s) copied", self.documents_copied);
        }
        s
    }
}

/// Generate an 11-character base-62 suffix.
///
/// A tiny xorshift seeded from the clock and the process id is enough here:
/// the goal is avoiding accidental collisions within one directory, not
/// unpredictability. Uniqueness is enforced by the caller against the index.
fn random_suffix(salt: u32) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mut state = nanos
        ^ (u64::from(std::process::id()) << 32)
        ^ (u64::from(salt) << 7)
        ^ 0x9E37_79B9_7F4A_7C15;
    let mut out = String::with_capacity(11);
    for _ in 0..11 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push(char::from(
            ALPHABET[(state % ALPHABET.len() as u64) as usize],
        ));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, ProfileStore) {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        let store = ProfileStore::load(&paths).unwrap();
        (dir, store)
    }

    #[test]
    fn a_subscription_url_can_be_changed_without_touching_anything_else() {
        let (_dir, mut store) = store();
        let uid = store.add(PrfItem::remote("", "panel", "https://old.example/sub"));
        let before = store.get(&uid).unwrap().clone();
        store.write_document(&before, "mode: rule\n").unwrap();

        store
            .set_url(&uid, "https://new.example/sub")
            .expect("a remote profile has a URL to change");

        let after = store.get(&uid).unwrap();
        assert_eq!(after.url.as_deref(), Some("https://new.example/sub"));
        // Everything else is where it was, so a chain naming this profile keeps
        // working and a generate before the next fetch still finds a document.
        assert_eq!(after.uid, before.uid);
        assert_eq!(after.name, before.name);
        assert_eq!(after.file, before.file);
        assert_eq!(
            store.read_document(after).unwrap(),
            "mode: rule\n",
            "the document is the old provider's until a fetch replaces it"
        );
    }

    #[test]
    fn a_profile_with_no_url_says_so_rather_than_being_given_one() {
        let (_dir, mut store) = store();
        let local = store.add(PrfItem::local("L1", "base"));
        let error = store.set_url(&local, "https://x.example/").unwrap_err();
        assert!(
            error.to_string().contains("no subscription URL"),
            "a local profile cannot be given a subscription: {error}"
        );
        // And an unknown uid is the error it always was.
        let error = store.set_url("nope", "https://x.example/").unwrap_err();
        assert!(error.to_string().contains("nope"), "{error}");
    }

    #[test]
    fn a_choice_is_remembered_once_per_group_and_can_be_forgotten() {
        let (dir, mut store) = store();
        let uid = store.add(PrfItem::local("L1", "base"));
        store.set_current(&uid).unwrap();

        assert!(store.selections().is_empty(), "nothing chosen yet");
        store.remember_selection("PROXY", "JP 01").unwrap();
        store.remember_selection("auto", "US 01").unwrap();
        // A second choice in the same group replaces the first rather than
        // accumulating: a list of everything ever chosen would replay the
        // oldest one first and the newest last, which is the same answer by a
        // longer route.
        store.remember_selection("PROXY", "JP 02").unwrap();

        let selections = store.selections();
        assert_eq!(selections.len(), 2);
        assert_eq!(selections[0].name, "PROXY");
        assert_eq!(selections[0].now, "JP 02");
        assert_eq!(selections[1].now, "US 01");

        store.forget_selection("PROXY").unwrap();
        assert_eq!(store.selections().len(), 1);
        assert_eq!(store.selections()[0].name, "auto");
        // Forgetting something that was never remembered is not an error.
        store.forget_selection("never").unwrap();
        drop(dir);
    }

    #[test]
    fn a_choice_survives_a_round_trip_through_the_index() {
        let (dir, mut store) = store();
        let uid = store.add(PrfItem::local("L1", "base"));
        store.set_current(&uid).unwrap();
        store.remember_selection("PROXY", "JP 01").unwrap();
        store.save().unwrap();

        let reloaded = ProfileStore::load(store.paths()).unwrap();
        assert_eq!(
            reloaded.selections(),
            vec![SelectedNode::new("PROXY", "JP 01")],
            "the choice is part of the index, not of this process"
        );
        drop(dir);
    }

    #[test]
    fn a_store_with_nothing_current_remembers_nothing() {
        let (dir, mut store) = store();
        assert!(store.selections().is_empty());
        // Not an error: there is no configuration for the choice to belong to.
        store.remember_selection("PROXY", "JP 01").unwrap();
        assert!(store.selections().is_empty());
        drop(dir);
    }

    #[test]
    fn a_missing_index_loads_as_empty() {
        let (_d, store) = store();
        assert!(store.items().is_empty());
        assert!(store.current().is_none());
        assert!(
            !store.paths().profiles_index().exists(),
            "loading must not create the file"
        );
    }

    #[test]
    fn save_and_reload_round_trips() {
        let (_d, mut store) = store();
        let uid = store.add(PrfItem::remote("R1", "Airport", "https://example.com/sub"));
        store.add(PrfItem::patch("m1", "Merge", ProfileType::Merge));
        store.set_current(&uid).unwrap();
        store.save().unwrap();

        let reloaded = ProfileStore::load(store.paths()).unwrap();
        assert_eq!(reloaded.index(), store.index());
        assert_eq!(reloaded.current_uid(), Some("R1"));
        assert_eq!(reloaded.items().len(), 2);
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temporary_files() {
        let (_d, mut store) = store();
        store.add(PrfItem::local("L1", "mine"));
        store.save().unwrap();
        let stray: Vec<_> = std::fs::read_dir(store.paths().profiles_dir().parent().unwrap())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(stray.is_empty(), "temporary files left behind: {stray:?}");
    }

    #[test]
    fn a_malformed_index_is_an_error_not_a_reset() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.profiles_index(), "current: [unclosed\n").unwrap();
        let err = ProfileStore::load(&paths).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }), "{err:?}");
        assert!(
            paths.profiles_index().exists(),
            "the file must not be destroyed"
        );
    }

    #[test]
    fn adding_without_a_uid_generates_a_unique_one_with_the_right_prefix() {
        let (_d, mut store) = store();
        let a = store.add(PrfItem::remote("", "a", "https://x"));
        let b = store.add(PrfItem::remote("", "b", "https://y"));
        assert!(a.starts_with('R'), "{a}");
        assert!(b.starts_with('R'), "{b}");
        assert_ne!(a, b);
        assert_eq!(store.get(&a).unwrap().file_name(), format!("{a}.yaml"));

        let m = store.add(PrfItem::patch("", "merge", ProfileType::Merge));
        assert!(m.starts_with('m'), "{m}");
    }

    #[test]
    fn the_base_cannot_be_a_patch() {
        let (_d, mut store) = store();
        store.add(PrfItem::patch("m1", "Merge", ProfileType::Merge));
        let err = store.set_current("m1").unwrap_err();
        assert!(err.to_string().contains("cannot be the base"), "{err}");
    }

    #[test]
    fn removing_a_profile_also_removes_its_document_and_chain_entries() {
        let (_d, mut store) = store();
        let base = store.add(PrfItem::remote("R1", "a", "https://x"));
        let patch = store.add(PrfItem::patch("m1", "m", ProfileType::Merge));
        store.set_current(&base).unwrap();
        store.set_chain(std::slice::from_ref(&patch)).unwrap();
        store
            .write_document(store.get(&patch).unwrap(), "log-level: debug\n")
            .unwrap();

        store.remove(&patch).unwrap();
        assert!(store.get(&patch).is_none());
        assert!(
            !document_path(
                store.paths(),
                &PrfItem::patch(&patch, "", ProfileType::Merge)
            )
            .exists()
        );
        assert!(store.index().chain.is_empty());
    }

    #[test]
    fn removing_the_current_profile_clears_current() {
        let (_d, mut store) = store();
        store.add(PrfItem::local("L1", "x"));
        store.set_current("L1").unwrap();
        store.remove("L1").unwrap();
        assert!(store.current_uid().is_none());
    }

    #[test]
    fn removing_something_absent_is_not_an_error() {
        let (_d, mut store) = store();
        assert!(store.remove("nope").unwrap().is_none());
    }

    #[test]
    fn resolving_without_a_current_profile_fails_clearly() {
        let (_d, store) = store();
        let err = store.resolve_chain().unwrap_err();
        assert!(err.to_string().contains("current"), "{err}");
    }

    #[test]
    fn an_explicit_chain_is_honoured_in_order() {
        let (_d, mut store) = store();
        store.add(PrfItem::local("L1", "base"));
        store.add(PrfItem::patch("m1", "m", ProfileType::Merge));
        store.add(PrfItem::patch("o1", "o", ProfileType::Override));
        store.set_current("L1").unwrap();
        store.set_chain(&["o1".into(), "m1".into()]).unwrap();

        let chain = store.resolve_chain().unwrap();
        let uids: Vec<&str> = chain.iter().map(|i| i.uid.as_str()).collect();
        assert_eq!(
            uids,
            vec!["L1", "o1", "m1"],
            "explicit order wins over type order"
        );
    }

    #[test]
    fn a_dangling_chain_reference_is_reported() {
        let (_d, mut store) = store();
        store.add(PrfItem::local("L1", "base"));
        store.set_current("L1").unwrap();
        store.index.chain = vec!["ghost".into()];
        let err = store.resolve_chain().unwrap_err();
        assert!(matches!(err, Error::InvalidChain { .. }), "{err:?}");
        assert!(err.to_string().contains("ghost"), "{err}");
    }

    #[test]
    fn a_base_profile_cannot_appear_in_the_chain() {
        let (_d, mut store) = store();
        store.add(PrfItem::local("L1", "base"));
        store.add(PrfItem::local("L2", "other base"));
        store.set_current("L1").unwrap();
        let err = store.set_chain(&["L2".into()]).unwrap_err();
        assert!(
            err.to_string().contains("may only contain patches"),
            "{err}"
        );
    }

    #[test]
    fn without_an_explicit_chain_every_patch_is_applied_in_type_order() {
        let (_d, mut store) = store();
        store.add(PrfItem::local("L1", "base"));
        store.add(PrfItem::patch("r1", "r", ProfileType::Rules));
        store.add(PrfItem::patch("m1", "m", ProfileType::Merge));
        store.add(PrfItem::patch("o1", "o", ProfileType::Override));
        store.set_current("L1").unwrap();

        let chain = store.resolve_chain().unwrap();
        let uids: Vec<&str> = chain.iter().map(|i| i.uid.as_str()).collect();
        assert_eq!(
            uids,
            vec!["L1", "m1", "o1", "r1"],
            "sorted by pipeline order"
        );
    }

    #[test]
    fn option_references_are_honoured_before_the_automatic_order() {
        let (_d, mut store) = store();
        let mut base = PrfItem::local("L1", "base");
        base.option.merge = Some("m2".into());
        store.add(base);
        store.add(PrfItem::patch("m1", "auto", ProfileType::Merge));
        store.add(PrfItem::patch("m2", "named", ProfileType::Merge));
        store.set_current("L1").unwrap();

        let chain = store.resolve_chain().unwrap();
        let uids: Vec<&str> = chain.iter().map(|i| i.uid.as_str()).collect();
        assert_eq!(uids, vec!["L1", "m2", "m1"], "the named merge comes first");
    }

    #[test]
    fn a_reference_to_a_missing_patch_is_reported() {
        let (_d, mut store) = store();
        let mut base = PrfItem::local("L1", "base");
        base.option.merge = Some("ghost".into());
        store.add(base);
        store.set_current("L1").unwrap();
        let err = store.resolve_chain().unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");
    }

    #[test]
    fn documents_round_trip_through_the_store() {
        let (_d, mut store) = store();
        let uid = store.add(PrfItem::patch("m1", "m", ProfileType::Merge));
        let item = store.get(&uid).unwrap().clone();
        store.write_document(&item, "log-level: debug\n").unwrap();
        assert_eq!(store.read_document(&item).unwrap(), "log-level: debug\n");
        assert!(document_path(store.paths(), &item).starts_with(store.paths().profiles_dir()));
    }

    #[test]
    fn importing_a_verge_home_copies_items_and_documents() {
        let (dir, mut store) = store();
        let foreign = dir.path().join("io.github.clash-verge-rev.clash-verge-rev");
        std::fs::create_dir_all(foreign.join("profiles")).unwrap();
        std::fs::write(
            foreign.join("profiles.yaml"),
            r#"
current: Rabcdefghij
items:
  - { uid: Rabcdefghij, type: remote, name: Airport, url: "https://x/sub", file: Rabcdefghij.yaml }
  - { uid: mZzzzzzzzzz, type: merge, name: Merge, file: mZzzzzzzzzz.yaml }
"#,
        )
        .unwrap();
        std::fs::write(
            foreign.join("profiles/Rabcdefghij.yaml"),
            "mixed-port: 7890\n",
        )
        .unwrap();
        std::fs::write(
            foreign.join("profiles/mZzzzzzzzzz.yaml"),
            "log-level: debug\n",
        )
        .unwrap();

        let report = store.import_from(&foreign).unwrap();
        assert_eq!(report.imported, 2, "{}", report.summary());
        assert_eq!(report.renamed, 0);
        assert_eq!(store.current_uid(), Some("Rabcdefghij"));
        assert_eq!(
            store
                .read_document(store.get("Rabcdefghij").unwrap())
                .unwrap(),
            "mixed-port: 7890\n"
        );
        assert_eq!(
            store
                .read_document(store.get("mZzzzzzzzzz").unwrap())
                .unwrap(),
            "log-level: debug\n"
        );
        assert!(report.summary().contains("2 profile"));
    }

    #[test]
    fn importing_twice_renames_instead_of_overwriting() {
        let (dir, mut store) = store();
        let foreign = dir.path().join("foreign");
        std::fs::create_dir_all(foreign.join("profiles")).unwrap();
        std::fs::write(
            foreign.join("profiles.yaml"),
            "items:\n  - { uid: Rabc, type: remote, name: A, url: \"https://x\", file: Rabc.yaml }\n",
        )
        .unwrap();
        std::fs::write(foreign.join("profiles/Rabc.yaml"), "mode: rule\n").unwrap();

        store.import_from(&foreign).unwrap();
        let second = store.import_from(&foreign).unwrap();
        assert_eq!(second.imported, 1);
        assert_eq!(
            second.renamed, 1,
            "the second import must not clobber the first"
        );
        assert_eq!(store.items().len(), 2);
    }

    #[test]
    fn a_missing_foreign_index_is_an_error() {
        let (dir, mut store) = store();
        let err = store.import_from(&dir.path().join("nowhere")).unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
    }

    #[test]
    fn generated_uids_look_like_the_reference_convention() {
        let suffix = random_suffix(0);
        assert_eq!(suffix.len(), 11);
        assert!(suffix.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(random_suffix(0), random_suffix(1));
    }
}
