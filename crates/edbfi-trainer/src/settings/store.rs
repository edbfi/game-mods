// SPDX-License-Identifier: AGPL-3.0-only
//! The settings file on disk: exclusive creation, atomic replacement, and never losing someone else's edit.
//!
//! A [`Store`] holds a writer lease on its folder, so a second trainer using the same file refuses to start
//! instead of racing the first. It remembers the exact bytes it last read or wrote. Each save writes a complete
//! temporary file, flushes it, and atomically replaces the settings file, keeping the replaced version aside. If
//! that version is not the one the store last saw, someone edited the file meanwhile: their version is kept as
//! `settings.external-<n>.toml` and the save reports it.
//!
//! On Windows the replacement is `ReplaceFileW`, which hands back the replaced file. Elsewhere it is a hard
//! link to the old file followed by `rename`, so an editor that itself renames a file into place between those
//! two steps can still be lost; only Windows builds ship.
#![forbid(unsafe_code)]

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::profile::ValidProfile;
use crate::recovery::RecoveryAction;

use super::{MAX_BYTES, Parsed, Settings, SettingsError};

/// The settings file's name.
const FILE_NAME: &str = "settings.toml";
const LOCK_NAME: &str = "settings.lock";

/// How many times an operation retries when the file appears or disappears under it.
const RACE_RETRIES: usize = 3;
/// The most numbered backups of one kind kept beside the file.
const MAX_NUMBERED: u32 = 9999;

/// The settings file of one mod, leased for writing.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    path: PathBuf,
    _lease: File,
    /// The exact bytes last read or written; `None` before the first load.
    baseline: Option<Vec<u8>>,
    sequence: u32,
    disk: Disk,
}

/// What [`Store::load`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load {
    /// There was no file; one with these defaults now exists.
    Created(Settings),
    /// The file was read. If [`Parsed::upgrade`] is set, save the settings to write the current schema.
    Read(Parsed),
    /// The file exists but cannot be used. Fix it and load again, or [`Store::reset`] it.
    Invalid(SettingsError),
}

/// What [`Store::save`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Saved {
    /// The file already held exactly these settings; nothing was written.
    Unchanged,
    /// The file was replaced.
    Written {
        /// Set if the file had changed since the store last read or wrote it.
        external: Option<External>,
    },
}

/// A change someone else made to the file, found while saving.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum External {
    /// The file had been edited or replaced; that version is kept at this path.
    Kept(PathBuf),
    /// The file had been deleted; it was written again.
    Deleted,
}

/// What [`Store::reset`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reset {
    /// The defaults now in the file.
    pub settings: Settings,
    /// Where the file's previous contents were kept, if there was a file.
    pub backup: Option<PathBuf>,
    /// Set if the file changed again while it was being reset.
    pub external: Option<External>,
}

/// Why the settings file could not be opened, read or written.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The settings folder or its lock file cannot be created.
    #[error("cannot use the settings folder {}: {source}", path.display())]
    Folder {
        /// The folder.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// Another trainer holds the lease.
    #[error("another trainer is using {}", path.display())]
    Leased {
        /// The settings folder.
        path: PathBuf,
    },
    /// The file exists but cannot be read.
    #[error("cannot read {}: {source}", path.display())]
    Unreadable {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// The file did not exist and the defaults could not be written.
    #[error("cannot create {}: {source}", path.display())]
    Create {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// A save or reset failed; the file holds its previous contents.
    #[error("cannot save {}: {source}", path.display())]
    Write {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
}

impl StoreError {
    /// Close the other trainer for a lease; otherwise retry.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        match self {
            Self::Leased { .. } => RecoveryAction::CloseOtherTrainer,
            _ => RecoveryAction::Retry,
        }
    }

    /// Whether no saved settings can exist, so the user may continue with unsaved defaults. An existing file
    /// that cannot be read never allows this: its choices must not silently turn into defaults.
    #[must_use]
    pub const fn allows_session_only(&self) -> bool {
        matches!(self, Self::Folder { .. } | Self::Create { .. })
    }
}

impl Store {
    /// Creates `dir` if needed and takes the writer lease on its settings file.
    ///
    /// # Errors
    ///
    /// [`StoreError::Leased`] if another trainer holds the lease, [`StoreError::Folder`] if the folder or its
    /// lock file cannot be created.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        let folder = |source| StoreError::Folder {
            path: dir.to_owned(),
            source,
        };
        fs::create_dir_all(dir).map_err(folder)?;
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(LOCK_NAME))
            .map_err(folder)?;
        match lease.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(StoreError::Leased {
                    path: dir.to_owned(),
                });
            }
            Err(TryLockError::Error(source)) => return Err(folder(source)),
        }
        Ok(Self {
            dir: dir.to_owned(),
            path: dir.join(FILE_NAME),
            _lease: lease,
            baseline: None,
            sequence: 0,
            disk: Disk::default(),
        })
    }

    /// The settings file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the file, or writes the defaults if there is none.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unreadable`] if the file exists but cannot be read, [`StoreError::Create`] if it does not
    /// exist and cannot be created.
    pub fn load(&mut self, profile: &ValidProfile<'_>) -> Result<Load, StoreError> {
        for _ in 0..RACE_RETRIES {
            match self.read(&self.path.clone()) {
                Ok(Some(bytes)) => {
                    let parsed = Settings::parse(profile, &bytes);
                    self.baseline = Some(bytes);
                    return Ok(match parsed {
                        Ok(parsed) => Load::Read(parsed),
                        Err(error) => Load::Invalid(error),
                    });
                }
                Ok(None) => {
                    let defaults = Settings::defaults(profile);
                    let bytes = defaults.to_toml(profile).into_bytes();
                    match self.create(&bytes) {
                        Ok(true) => {
                            self.baseline = Some(bytes);
                            return Ok(Load::Created(defaults));
                        }
                        // Someone created the file meanwhile: read theirs.
                        Ok(false) => {}
                        Err(source) => return Err(self.error_create(source)),
                    }
                }
                Err(source) => {
                    return Err(StoreError::Unreadable {
                        path: self.path.clone(),
                        source,
                    });
                }
            }
        }
        Err(self.error_create(io::Error::other("the file kept appearing and disappearing")))
    }

    /// Writes `settings` unless the file already holds exactly them.
    ///
    /// # Errors
    ///
    /// [`StoreError::Write`]; the file then holds its previous contents and saving can be retried.
    ///
    /// # Panics
    ///
    /// If the settings belong to another profile.
    pub fn save(
        &mut self,
        profile: &ValidProfile<'_>,
        settings: &Settings,
    ) -> Result<Saved, StoreError> {
        let bytes = settings.to_toml(profile).into_bytes();
        if self.baseline.as_ref() == Some(&bytes) {
            return Ok(Saved::Unchanged);
        }
        let external = self
            .write(&bytes)
            .map_err(|source| self.error_write(source))?;
        self.baseline = Some(bytes);
        Ok(Saved::Written { external })
    }

    /// Keeps the file's current contents as `settings.invalid-<n>.toml`, never replacing an older backup,
    /// then writes the defaults.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unreadable`] if the current file cannot be read for its backup, [`StoreError::Write`] if
    /// the backup or the defaults cannot be written. The file is unchanged on error.
    pub fn reset(&mut self, profile: &ValidProfile<'_>) -> Result<Reset, StoreError> {
        let current = self
            .read(&self.path.clone())
            .map_err(|source| StoreError::Unreadable {
                path: self.path.clone(),
                source,
            })?;
        let backup = match &current {
            Some(bytes) => Some(
                self.numbered("invalid", |disk, path| disk.op(|| write_new(path, bytes)))
                    .map_err(|source| self.error_write(source))?,
            ),
            None => None,
        };
        // The backup holds these bytes, so replacing them loses nothing; a later change is still kept.
        self.baseline = current;
        let settings = Settings::defaults(profile);
        let bytes = settings.to_toml(profile).into_bytes();
        let external = self
            .write(&bytes)
            .map_err(|source| self.error_write(source))?;
        self.baseline = Some(bytes);
        Ok(Reset {
            settings,
            backup,
            external,
        })
    }

    fn error_create(&self, source: io::Error) -> StoreError {
        StoreError::Create {
            path: self.path.clone(),
            source,
        }
    }

    fn error_write(&self, source: io::Error) -> StoreError {
        StoreError::Write {
            path: self.path.clone(),
            source,
        }
    }

    /// The file's bytes, `None` if it does not exist. Reads at most one byte past [`MAX_BYTES`].
    fn read(&mut self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        let opened = self.disk.op(|| File::open(path));
        let file = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        self.disk.op(|| {
            file.take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map(drop)
        })?;
        Ok(Some(bytes))
    }

    /// A fresh name in the folder for this process's own scratch files.
    fn scratch(&mut self, kind: &str) -> PathBuf {
        self.sequence = self.sequence.wrapping_add(1);
        let name = format!(
            "{FILE_NAME}.{}-{}.{kind}",
            std::process::id(),
            self.sequence
        );
        self.dir.join(name)
    }

    /// A complete, flushed temporary file holding `bytes`.
    fn temp(&mut self, bytes: &[u8]) -> io::Result<PathBuf> {
        let temp = self.scratch("tmp");
        self.disk.op(|| write_new(&temp, bytes)).map(|()| temp)
    }

    /// Removes one of this process's scratch files. A failure only leaves a stray file behind.
    fn remove(&mut self, path: &Path) {
        let _ignored = self.disk.op(|| fs::remove_file(path));
    }

    /// Creates the file holding `bytes` unless it exists. Returns whether it was created.
    fn create(&mut self, bytes: &[u8]) -> io::Result<bool> {
        let temp = self.temp(bytes)?;
        let linked = self.disk.op(|| fs::hard_link(&temp, &self.path));
        self.remove(&temp);
        match linked {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Atomically replaces the file with `bytes`, keeping any version the store has not seen.
    fn write(&mut self, bytes: &[u8]) -> io::Result<Option<External>> {
        let temp = self.temp(bytes)?;
        let installed = self.install(&temp);
        self.remove(&temp);
        installed
    }

    fn install(&mut self, temp: &Path) -> io::Result<Option<External>> {
        for _ in 0..RACE_RETRIES {
            let path = self.path.clone();
            if self.disk.op(|| path.try_exists())? {
                let backup = self.scratch("replaced");
                match self.disk.op(|| replace(&path, temp, &backup)) {
                    Ok(()) => return Ok(self.settle(&backup)),
                    Err(error) => {
                        self.salvage(&backup);
                        if error.kind() != io::ErrorKind::NotFound {
                            return Err(error);
                        }
                    }
                }
            } else {
                match self.disk.op(|| fs::hard_link(temp, &path)) {
                    Ok(()) => return Ok(self.baseline.is_some().then_some(External::Deleted)),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Err(io::Error::other("the file kept appearing and disappearing"))
    }

    /// Deals with the replaced version: dropped if it is what the store last saw, otherwise kept.
    fn settle(&mut self, backup: &Path) -> Option<External> {
        if let Ok(Some(previous)) = self.read(backup)
            && self.baseline.as_ref() == Some(&previous)
        {
            self.remove(backup);
            return None;
        }
        // Unseen or unreadable: never discard it.
        let kept = self
            .numbered("external", |disk, path| {
                disk.op(|| fs::hard_link(backup, path))
            })
            .map_or_else(
                |_| backup.to_owned(),
                |kept| {
                    self.remove(backup);
                    kept
                },
            );
        Some(External::Kept(kept))
    }

    /// After a failed replacement, puts back a replaced file that was moved aside but not replaced.
    fn salvage(&mut self, backup: &Path) {
        let path = self.path.clone();
        if !matches!(self.disk.op(|| backup.try_exists()), Ok(true)) {
            return;
        }
        if matches!(self.disk.op(|| path.try_exists()), Ok(false)) {
            let _ignored = self.disk.op(|| fs::hard_link(backup, &path));
        }
        let same = matches!(
            (self.read(backup), self.read(&path)),
            (Ok(Some(a)), Ok(Some(b))) if a == b
        );
        if same {
            self.remove(backup);
        }
    }

    /// Runs `create` on `settings.<kind>-<n>.toml` for n = 1, 2, … until one did not exist yet.
    fn numbered(
        &mut self,
        kind: &str,
        mut create: impl FnMut(&mut Disk, &Path) -> io::Result<()>,
    ) -> io::Result<PathBuf> {
        for n in 1..=MAX_NUMBERED {
            let path = self.dir.join(format!("settings.{kind}-{n}.toml"));
            match create(&mut self.disk, &path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other(format!(
            "{MAX_NUMBERED} settings.{kind}-<n>.toml files already exist"
        )))
    }
}

/// Creates `path`, which must not exist, holding `bytes`, flushed to disk. If writing fails, the new file is
/// removed again.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if written.is_err() {
        drop(file);
        let _ignored = fs::remove_file(path);
    }
    written
}

/// Replaces `target` with `replacement`, leaving the replaced file at `backup`.
#[cfg(windows)]
fn replace(target: &Path, replacement: &Path, backup: &Path) -> io::Result<()> {
    crate::win32::fs::replace_file(target, replacement, backup)
}

/// Replaces `target` with `replacement`, leaving the replaced file at `backup`.
#[cfg(not(windows))]
fn replace(target: &Path, replacement: &Path, backup: &Path) -> io::Result<()> {
    fs::hard_link(target, backup)?;
    fs::rename(replacement, target).inspect_err(|_| {
        let _ignored = fs::remove_file(backup);
    })
}

/// Every filesystem call of a [`Store`]. Tests use it to fail a call or to change the folder before one.
#[derive(Default)]
pub(super) struct Disk {
    #[cfg(test)]
    pub(super) calls: usize,
    #[cfg(test)]
    pub(super) before: Option<(usize, Hook)>,
}

/// A test's action before a filesystem call; an error it returns fails that call.
#[cfg(test)]
pub(super) type Hook = Box<dyn FnMut() -> Option<io::Error>>;

impl std::fmt::Debug for Disk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Disk")
    }
}

impl Disk {
    fn op<T>(&mut self, call: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        #[cfg(test)]
        {
            self.calls += 1;
            if let Some((at, hook)) = &mut self.before
                && *at == self.calls
                && let Some(error) = hook()
            {
                return Err(error);
            }
        }
        call()
    }
}

#[cfg(test)]
impl Store {
    /// Runs `hook` before the `at`-th filesystem call from now; if it returns an error, that call fails with it.
    pub(super) fn before_call(
        &mut self,
        at: usize,
        hook: impl FnMut() -> Option<io::Error> + 'static,
    ) {
        self.disk.calls = 0;
        self.disk.before = Some((at, Box::new(hook)));
    }

    /// How many filesystem calls ran since the last [`Store::before_call`].
    pub(super) fn calls(&self) -> usize {
        self.disk.calls
    }
}
