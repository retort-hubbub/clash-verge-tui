//! Backup creation, additive restore and retention for the service facade.
//!
//! Filesystem guards and retention ordering live here so both entry points use
//! the same copying policy. The public API remains on [`Service`].

use std::path::{Path, PathBuf};

use chrono::Utc;

use super::Service;
use crate::error::{Error, Result};

/// Relative paths copied in both directions and recognised as backup contents.
const STATE_FILES: &[&str] = &["cvt.yaml", "profiles.yaml"];
const STATE_DIRS: &[&str] = &["profiles", "overrides"];

/// Default retention target. Concurrent backups from the same second are kept
/// together, so automatic pruning may temporarily retain more than this limit.
pub const BACKUP_LIMIT: usize = 5;

/// One directory of saved state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    /// Where it is.
    pub path: PathBuf,
    /// Unix timestamp of when it was taken.
    pub created: i64,
    /// Which backup of that second it was, `1` for the first.
    ///
    /// Carried separately because two backups taken in the same second share a
    /// timestamp, and sorting on the timestamp alone leaves their order to
    /// whatever `read_dir` produced — so the same six backups pruned to
    /// different survivors depending on the order they were made in.
    pub sequence: u32,
    /// How many entries it holds, for a report.
    pub items: usize,
}

impl Backup {
    /// The timestamp with a sequence suffix when several backups share a second.
    #[must_use]
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Whether a directory is one of this program's backups.
///
/// Any saved state entry is sufficient: a home may not have saved settings or
/// a profile index yet. The supported paths are shared with [`copy_state`].
fn looks_like_a_backup(dir: &Path) -> bool {
    // `symlink_metadata`, not `exists`: that follows the link, so a directory
    // whose entries are all links was admitted as a backup and then every
    // copier skipped everything it held — a restore that did nothing at all and
    // reported success. The admission test and the copiers have to agree about
    // what a link means, and they now both say "not this program's".
    STATE_FILES.iter().chain(STATE_DIRS).any(|name| {
        std::fs::symlink_metadata(dir.join(name)).is_ok_and(|meta| !meta.file_type().is_symlink())
    })
}

/// Copy the state a user would have to recreate by hand.
///
/// Used in both directions: from the home into a backup, and from a backup
/// into the home. Both operations share the same file selection and guards.
fn copy_state(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).map_err(|e| Error::io(to, e))?;
    for name in STATE_FILES {
        let source = from.join(name);
        // A *link* where a scalar file goes is skipped, exactly as `copy_dir`
        // skips a directory reached through one: reading it copies the
        // contents of a file outside the home into the backup, which is the
        // opposite of the omission the skip was written for and worse.
        if std::fs::symlink_metadata(&source).is_ok_and(|meta| meta.file_type().is_symlink()) {
            tracing::warn!(
                path = %source.display(),
                "skipping a file reached through a symbolic link"
            );
            continue;
        }
        if source.is_file() {
            let destination = to.join(name);
            copy_file(&source, &destination)?;
        }
    }
    for name in STATE_DIRS {
        copy_dir(&from.join(name), &to.join(name))?;
    }
    Ok(())
}

/// Whether two paths name the same file, by the identity the filesystem gives
/// it rather than by the path.
///
/// A hard link is one file with two names, and `std::fs::copy(x, y)` where `x`
/// and `y` are those two names truncates the file before reading it — the copy
/// then reports success having written nothing. Comparing canonical paths
/// misses it, because the paths really are different.
#[cfg(unix)]
fn is_same_file(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (std::fs::metadata(left).ok(), std::fs::metadata(right).ok()) {
        (Some(a), Some(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// The portable fallback: the most a path alone can say is whether it is the
/// same path.
#[cfg(not(unix))]
fn is_same_file(left: &Path, right: &Path) -> bool {
    let same = std::fs::canonicalize(left).ok();
    same.is_some() && same == std::fs::canonicalize(right).ok()
}

/// Everything a copy would write, checked before any of it is written.
///
/// A restore that refuses halfway has already replaced part of the home, and
/// the state it leaves is two configurations at once with nothing saying so.
/// The refusal has to come first, which means walking the whole tree — the
/// sources *and* the destinations — before the first byte moves.
///
/// # Errors
/// [`Error::InvalidValue`] naming the first destination that cannot be written.
fn check_copy(from: &Path, to: &Path) -> Result<()> {
    for name in STATE_FILES {
        let source = from.join(name);
        // Skip source links, matching copy_state's file policy.
        if std::fs::symlink_metadata(&source).is_ok_and(|meta| meta.file_type().is_symlink()) {
            continue;
        }
        if source.is_file() {
            check_destination(&source, &to.join(name))?;
        }
    }
    for name in STATE_DIRS {
        let source = from.join(name);
        if !source.is_dir() {
            continue;
        }
        check_directory_destination(&to.join(name))?;
        let Ok(entries) = std::fs::read_dir(&source) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                check_destination(
                    &source.join(entry.file_name()),
                    &to.join(name).join(entry.file_name()),
                )?;
            }
        }
    }
    Ok(())
}

/// Whether a directory destination can be written.
///
/// A directory has its own two answers: a *symlink* would be followed, putting
/// everything outside the home, and something that is not a directory at all —
/// a file where `profiles/` belongs — cannot hold what is being copied into it.
fn check_directory_destination(path: &Path) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        return Err(Error::invalid(
            "backup",
            format!(
                "{} is a symbolic link; writing through it would put the files \
                 outside the home. Remove the link, or restore by hand.",
                path.display()
            ),
        ));
    }
    if !meta.is_dir() {
        return Err(Error::invalid(
            "backup",
            format!(
                "{} is not a directory, so the files that belong in it cannot be \
                 restored there",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Whether one file destination can be written, and why not when it cannot.
///
/// A destination already naming the source needs no copy. Otherwise accept
/// only an absent path or a regular file: symlinks may redirect writes outside
/// the home, and special files such as FIFOs can block a copy indefinitely.
/// [`copy_file`] handles replacing Unix hard links before writing.
fn check_destination(source: &Path, path: &Path) -> Result<()> {
    // A destination that already *is* the source's file needs nothing, and a
    // link pointing at the source is that: refusing it would refuse a restore
    // that has nothing to do. The order matters and got it wrong once.
    if is_same_file(source, path) {
        return Ok(());
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        return Err(Error::invalid(
            "backup",
            format!(
                "{} is a symbolic link; writing through it would put the files \
                 outside the home. Remove the link, or restore by hand.",
                path.display()
            ),
        ));
    }
    if !meta.is_file() {
        return Err(Error::invalid(
            "backup",
            format!(
                "{} is not a regular file, so a copy into it would block or fail; \
                 remove it, or restore by hand",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Copy one file, refusing to copy it onto itself.
///
/// The guard is here and not only at the entry points, because this is the
/// function that would do the truncating and a caller added later would not know
/// to check. `std::fs::copy` opens the destination for writing before it reads
/// the source, so a file copied onto itself comes back empty and the call
/// reports success.
fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    // The same *file*, not merely the same path. Canonicalising compares names,
    // and two names for one inode — a hard link, which is what `cp -al` leaves
    // behind — are not the same name. `std::fs::copy` then truncates the file
    // before reading it, and a restore emptied the document it was asked to put
    // back.
    if is_same_file(source, destination) {
        return Ok(());
    }
    check_destination(source, destination)?;
    // A destination that is a *hard link* is replaced rather than written
    // through. `std::fs::copy` truncates the inode, so a document that is a
    // second name for a file outside the home had that file edited — the same
    // escape the symlink guard refuses, one `symlink_metadata` further down.
    // Unlinking the name first breaks the link: the copy gets a fresh inode,
    // and the other name keeps what it had. A refusal would be defensible, but
    // a home whose documents were hard-linked by `cp -al` should still be
    // restorable, and this is the answer that allows both.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if std::fs::metadata(destination).is_ok_and(|meta| meta.nlink() > 1) {
            std::fs::remove_file(destination).map_err(|e| Error::io(destination, e))?;
        }
    }
    // A destination that is a *symlink* is refused rather than followed, with
    // one exception that comes before this: a link pointing at the *source*
    // returns early above, because the destination already is the source's
    // file and there is nothing to do. That is deliberate rather than an
    // oversight — the alternative is refusing a restore that would have
    // succeeded — but it means this comment is about every other link.
    //
    // The reason for the refusal: `std::fs::copy` opens the destination for
    // writing, which follows the link, so a home whose `profiles/L1.yaml` is a
    // link into somebody's dotfiles had a restore overwrite that file, outside
    // the home, silently. The link is the user's arrangement and is not this
    // function's to replace.
    if std::fs::symlink_metadata(destination).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(Error::invalid(
            "backup",
            format!(
                "{} is a symbolic link; a restore would write through it to \
                 wherever it points. Remove the link, or restore by hand.",
                destination.display()
            ),
        ));
    }
    std::fs::copy(source, destination).map_err(|e| Error::io(destination, e))?;
    Ok(())
}

/// Copy a directory of files, skipping anything that is not a regular file.
///
/// Written out rather than pulled from a crate: these are two directories of
/// small text files, and the cases worth stating — a missing directory, a
/// symlink — are clearer here than in a configuration.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    // A directory reached *through a symlink* is not this home's. Reading one
    // copies files from wherever the link points — the test that found this
    // put a `secret.yaml` in somebody else's directory and it arrived in the
    // backup — and writing one puts files there, which is a restore escaping
    // the home.
    //
    // The two directions answer differently and the asymmetry is deliberate. A
    // source that is a link is *skipped*: a backup that quietly omits something
    // is recoverable, and refusing would mean a user with `profiles/` symlinked
    // to another disk can never take one at all. A destination that is a link
    // is *refused*: a restore that silently writes outside the home is not
    // recoverable, and one that silently does nothing would be a lie.
    if std::fs::symlink_metadata(from).is_ok_and(|meta| meta.file_type().is_symlink()) {
        tracing::warn!(
            path = %from.display(),
            "skipping a directory reached through a symbolic link"
        );
        return Ok(());
    }
    if std::fs::symlink_metadata(to).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(Error::invalid(
            "backup",
            format!(
                "{} is a symbolic link; writing through it would put the files \
                 outside the home. Remove the link, or restore by hand.",
                to.display()
            ),
        ));
    }
    if !from.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(to).map_err(|e| Error::io(to, e))?;
    let entries = std::fs::read_dir(from).map_err(|e| Error::io(from, e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        // Only regular files. A symlink here is not something this program
        // writes, and following one would copy a file from wherever the link
        // points — including out of the home entirely.
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        copy_file(&path, &to.join(name))?;
    }
    Ok(())
}

impl Service {
    /// Copy the user's own state into a timestamped directory.
    ///
    /// What is copied is what a person would have to recreate by hand if it
    /// were lost: the settings, the profile index, the profile documents and
    /// the overrides. Not the generated configuration, which is derived and is
    /// already snapshotted by the pipeline, and not the core's working
    /// directory or the logs, which are large and reproducible.
    ///
    /// # Errors
    /// [`Error::Io`] when a file cannot be read or written, and
    /// [`Error::InvalidValue`] when the backup directory is unsafe or all names
    /// for the current second are taken.
    pub fn backup(&self) -> Result<PathBuf> {
        let destination = self.create_backup()?;
        self.prune_backups_keeping(BACKUP_LIMIT, &destination)?;
        Ok(destination)
    }

    /// Create a complete backup without pruning any existing restore source.
    /// Callers prune after they have finished using the previous backups.
    fn create_backup(&self) -> Result<PathBuf> {
        let destination = self.reserve_backup_path(Utc::now().timestamp())?;
        if let Err(error) = copy_state(self.paths.home(), &destination) {
            // Do not offer an incomplete backup for a future restore.
            let _ = std::fs::remove_dir_all(&destination);
            return Err(error);
        }
        Ok(destination)
    }

    /// A backup name that is not already taken, *created* so that it stays
    /// that way.
    ///
    /// `exists()` and then a write is a race two backups in the same second
    /// both win: both see the same free name, both write into one directory,
    /// and each prunes around a directory the other is still filling.
    /// `create_dir` fails when the name is taken, and that failure is the lock.
    ///
    /// # Errors
    /// [`Error::Io`] when a name cannot be created.
    fn reserve_backup_path(&self, stamp: i64) -> Result<PathBuf> {
        let dir = self.paths.backups_dir();
        // The one directory that receives the whole state, and the only one of
        // the three this program writes into that nothing checked.
        if std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(Error::invalid(
                "backup",
                format!(
                    "{} is a symbolic link; the backups would be written outside \
                     the home",
                    dir.display()
                ),
            ));
        }
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let mut candidates = vec![dir.join(stamp.to_string())];
        for n in 2..1000 {
            candidates.push(dir.join(format!("{stamp}-{n}")));
        }
        candidates.push(dir.join(format!("{stamp}-{}", u32::MAX)));
        for candidate in &candidates {
            match std::fs::create_dir(candidate) {
                Ok(()) => return Ok(candidate.clone()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(Error::io(candidate, e)),
            }
        }
        Err(Error::invalid(
            "backup",
            format!("a thousand backups in the second {stamp}; try again in a moment"),
        ))
    }

    /// Every backup, newest first.
    ///
    /// # Errors
    /// [`Error::Io`] when the directory cannot be read.
    pub fn backups(&self) -> Result<Vec<Backup>> {
        let dir = self.paths.backups_dir();
        // A link here is not this program's directory. Listing through it
        // offers the user's own directories as backups — the name parse and
        // nothing else — and pruning then deletes them, in a tree this program
        // never created and cannot describe.
        if std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
            tracing::warn!(
                path = %dir.display(),
                "the backups directory is a symbolic link, so it is not listed"
            );
            return Ok(Vec::new());
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let mut found: Vec<Backup> = entries
            .flatten()
            .filter_map(|entry| {
                // `symlink_metadata`, not `is_dir`: that follows the link, so a
                // symlink pointing at somebody else's directory was listed as a
                // backup and offered for restore. A backup is a directory this
                // program wrote, and a link is not one.
                let kind = entry.file_type().ok()?;
                if !kind.is_dir() {
                    return None;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                // `1790363784`, or `1790363784-2` when that second was taken.
                //
                // The mapping from name to `(created, sequence)` has to be
                // *injective*, because the sequence exists to break ties and a
                // tie it cannot break is the bug it was added for. So a name
                // this program would not write is not a backup: `<stamp>-2-3`,
                // `<stamp>-overflow` and `<stamp>-1` each used to parse to
                // something, and two of them to the same something.
                let (stamp, sequence) = match name.split_once('-') {
                    None => (name.as_str(), 1),
                    Some((stamp, rest)) => {
                        let n = rest.parse::<u32>().ok()?;
                        // Canonical spelling, and `-1` is the bare name.
                        if n < 2 || rest != n.to_string() {
                            return None;
                        }
                        (stamp, n)
                    }
                };
                let created = stamp.parse::<i64>().ok()?;
                // Canonicalised as well, and not only the suffix: `02000-2`
                // and `2000-2` both parsed to `(2000, 2)`, so the tie was back
                // and the survivor was the filesystem's again — canonicalising
                // one half of a key and not the other.
                if stamp != created.to_string() {
                    return None;
                }
                let items = std::fs::read_dir(entry.path())
                    .map(|inner| inner.flatten().count())
                    .unwrap_or(0);
                Some(Backup {
                    path: entry.path(),
                    created,
                    sequence,
                    items,
                })
            })
            .collect();
        // Newest first, and *within a second* by the order they were taken.
        // `created` alone left ties in `read_dir` order.
        found.sort_by_key(|backup| std::cmp::Reverse((backup.created, backup.sequence)));
        Ok(found)
    }

    /// Put a backup back, keeping the state it replaces.
    ///
    /// **Additive, not destructive.** The files the backup holds are written
    /// back; a file that appeared after it was taken stays where it is. So this
    /// is "put back what was saved", not "make the home identical to the
    /// backup", and the difference matters for a directory full of profile
    /// documents: a document no entry in the restored index mentions is an
    /// orphan, and this program preserves orphans rather than deleting them —
    /// that is what the import path was fixed to do, for the same reason.
    /// Anything a restore should not keep, a person can delete.
    ///
    /// The state being replaced is copied to a fresh backup first, so restoring
    /// the wrong one is itself undoable. That is why this is a method rather
    /// than a directory copy: the moment somebody needs it is the moment they
    /// are least sure which one they want.
    ///
    /// # Errors
    /// [`Error::InvalidValue`] when the directory is not a backup of this home,
    /// and [`Error::Io`] when a file cannot be copied.
    pub fn restore(&self, from: &Path) -> Result<PathBuf> {
        // Before anything else, because `std::fs::copy(x, x)` truncates: the
        // destination is opened for writing before a byte is read, and the copy
        // reports success having written nothing. Restoring a home onto itself
        // therefore emptied the index, the settings and every document — and it
        // *succeeded*, which is the worst way to lose data.
        //
        // Compared by canonical path, so `…/`, `…/.` and a symlink to it are
        // all recognised as the same place. A directory that cannot be
        // canonicalised is not there, and is refused by the check below.
        let same = std::fs::canonicalize(from).ok();
        if same.is_some() && same == std::fs::canonicalize(self.paths.home()).ok() {
            return Err(Error::invalid(
                "backup",
                format!(
                    "{} is this home; a restore copies over its own source and would \
                     empty it",
                    from.display()
                ),
            ));
        }
        // What `backup()` produces, rather than the one file it *usually*
        // produces: a home that has never had an index copied to a backup with
        // no `profiles.yaml` in it, and this check then refused a directory
        // this program had written itself.
        if !looks_like_a_backup(from) {
            return Err(Error::invalid(
                "backup",
                format!(
                    "{} holds none of the settings, the profile index or the profile \
                     directories, so it is not a backup of this home",
                    from.display()
                ),
            ));
        }
        // Before the safety copy, and before a single byte moves: a refusal
        // that has already written half of the backup leaves the home as two
        // configurations at once, with nothing saying so.
        check_copy(from, self.paths.home())?;
        let safety = self.create_backup()?;
        copy_state(from, self.paths.home())?;
        // Creating the safety copy can exceed the retention limit. Prune only
        // after reading the source: it may be the oldest backup in this home.
        self.prune_backups_keeping(BACKUP_LIMIT, &safety)?;
        Ok(safety)
    }

    /// Keep the newest `keep` backups, and `keep_this` whatever its age.
    fn prune_backups_keeping(&self, keep: usize, keep_this: &Path) -> Result<usize> {
        let mut removed = 0;
        // Only what is *older* than the backup just taken. Two backups in the
        // same second are otherwise indistinguishable from two backups days
        // apart, and each pruned around a directory the other was still
        // filling: 24 at once left 3 of 3 returned paths holding nothing.
        let newest = self
            .backups()?
            .into_iter()
            .find(|backup| backup.path == keep_this)
            .map(|backup| (backup.created, backup.sequence));
        for backup in self.backups()?.into_iter().skip(keep) {
            if backup.path == keep_this {
                continue;
            }
            // Same *second*, not merely same-or-newer sequence. Pruning frees
            // names, and a freed name is one another `backup()` in the same
            // second will reserve and return — so two threads were handed the
            // same directory, one of them writing into what the other had
            // already handed back.
            if newest.is_some_and(|(created, _)| backup.created >= created) {
                continue;
            }
            std::fs::remove_dir_all(&backup.path).map_err(|e| Error::io(&backup.path, e))?;
            removed += 1;
        }
        removed += self.remove_links()?;
        Ok(removed)
    }

    /// Remove the symlinks in the backups directory, which are never backups.
    fn remove_links(&self) -> Result<usize> {
        let mut removed = 0;
        let dir = self.paths.backups_dir();
        // Not through a link: what is inside the target is the user's, and this
        // function deletes what it finds.
        if std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Ok(0);
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(0);
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                std::fs::remove_file(entry.path()).map_err(|e| Error::io(entry.path(), e))?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Keep the newest `keep` backups and delete the rest.
    ///
    /// # Errors
    /// [`Error::Io`] when a directory exists and cannot be removed.
    pub fn prune_backups(&self, keep: usize) -> Result<usize> {
        let mut removed = 0;
        for backup in self.backups()?.into_iter().skip(keep) {
            std::fs::remove_dir_all(&backup.path).map_err(|e| Error::io(&backup.path, e))?;
            removed += 1;
        }
        // A symlink in this directory is not something this program writes, and
        // leaving it means `backup restore <name>` can be pointed at a
        // directory outside the home. Anything else a person put here stays:
        // deleting a file this program did not write and cannot explain is
        // worse than leaving it.
        removed += self.remove_links()?;
        Ok(removed)
    }
}
