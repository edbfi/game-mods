// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where a mod keeps its settings and logs: `edbfi\<game>\<mod>\` under the user's roaming and local
//! application data on Windows, and under the XDG config and state folders elsewhere.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::profile::ModSlug;
use crate::recovery::RecoveryAction;

/// A mod's settings and log folders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locations {
    /// Holds `settings.toml`: `%APPDATA%\edbfi\<game>\<mod>\`.
    pub settings: PathBuf,
    /// Holds the log: `%LOCALAPPDATA%\edbfi\<game>\<mod>\`.
    pub logs: PathBuf,
}

impl Locations {
    /// The folders for `slug` under the given roots.
    #[must_use]
    pub fn under(slug: ModSlug, config: &Path, data: &Path) -> Self {
        let folder = |root: &Path| root.join("edbfi").join(slug.game).join(slug.r#mod);
        Self {
            settings: folder(config),
            logs: folder(data),
        }
    }

    /// The folders for `slug` from this process's environment.
    ///
    /// # Errors
    ///
    /// A [`LocationError`] if a variable the platform needs is unset or not an absolute path.
    pub fn from_env(slug: ModSlug) -> Result<Self, LocationError> {
        Self::from_vars(slug, cfg!(windows), |name| std::env::var_os(name))
    }

    /// The folders for `slug`, reading environment variables through `var`: `APPDATA` and `LOCALAPPDATA`
    /// if `windows`, otherwise `XDG_CONFIG_HOME` and `XDG_STATE_HOME`, falling back to `$HOME/.config` and
    /// `$HOME/.local/state`.
    ///
    /// # Errors
    ///
    /// A [`LocationError`] if a needed variable is unset, empty or not an absolute path.
    pub fn from_vars(
        slug: ModSlug,
        windows: bool,
        var: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self, LocationError> {
        let absolute = |name: &'static str| -> Result<Option<PathBuf>, LocationError> {
            match var(name).filter(|value| !value.is_empty()) {
                None => Ok(None),
                Some(value) => {
                    let path = PathBuf::from(value);
                    if path.is_absolute() {
                        Ok(Some(path))
                    } else {
                        Err(LocationError::Relative { var: name, path })
                    }
                }
            }
        };
        let required = |name| absolute(name)?.ok_or(LocationError::Unset { var: name });
        let (config, data) = if windows {
            (required("APPDATA")?, required("LOCALAPPDATA")?)
        } else {
            let home = || required("HOME");
            let config = match absolute("XDG_CONFIG_HOME") {
                Ok(Some(path)) => path,
                // The XDG specification says to ignore a relative path.
                Ok(None) | Err(LocationError::Relative { .. }) => home()?.join(".config"),
                Err(error) => return Err(error),
            };
            let data = match absolute("XDG_STATE_HOME") {
                Ok(Some(path)) => path,
                Ok(None) | Err(LocationError::Relative { .. }) => home()?.join(".local/state"),
                Err(error) => return Err(error),
            };
            (config, data)
        };
        Ok(Self::under(slug, &config, &data))
    }

    /// The settings file in [`Self::settings`].
    #[must_use]
    pub fn settings_file(&self) -> PathBuf {
        self.settings.join(crate::settings::FILE_NAME)
    }

    /// The current log file in [`Self::logs`].
    #[must_use]
    pub fn log_file(&self) -> PathBuf {
        self.logs.join(crate::journal::LOG_NAME)
    }
}

/// Why the folders cannot be found. Settings cannot be saved; the user may continue with unsaved defaults.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LocationError {
    /// A needed environment variable is unset or empty.
    #[error("the environment variable {var} is not set")]
    Unset {
        /// The variable.
        var: &'static str,
    },
    /// A needed environment variable is not an absolute path.
    #[error("the environment variable {var} is not an absolute path: {}", path.display())]
    Relative {
        /// The variable.
        var: &'static str,
        /// Its value.
        path: PathBuf,
    },
}

impl LocationError {
    /// Retry from an environment that sets the variable, or continue with unsaved defaults.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        RecoveryAction::Retry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLUG: ModSlug = ModSlug {
        game: "metro-2033-redux",
        r#mod: "trainer",
    };

    fn root(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    fn vars(pairs: &[(&str, PathBuf)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, OsString)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone().into_os_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn windows_uses_roaming_settings_and_local_logs() {
        let (roaming, local) = (root("Roaming"), root("Local"));
        let found = Locations::from_vars(
            SLUG,
            true,
            vars(&[
                ("APPDATA", roaming.clone()),
                ("LOCALAPPDATA", local.clone()),
            ]),
        );
        let folder = |root: &Path| root.join("edbfi").join("metro-2033-redux").join("trainer");
        assert_eq!(
            found,
            Ok(Locations {
                settings: folder(&roaming),
                logs: folder(&local),
            })
        );
        let found = found.unwrap();
        assert_eq!(
            found.settings_file(),
            folder(&roaming).join("settings.toml")
        );
        assert_eq!(found.log_file(), folder(&local).join("trainer.log"));
    }

    #[test]
    fn windows_refuses_a_missing_or_relative_folder() {
        let found = Locations::from_vars(SLUG, true, vars(&[("APPDATA", root("Roaming"))]));
        assert_eq!(
            found,
            Err(LocationError::Unset {
                var: "LOCALAPPDATA"
            })
        );
        let found = Locations::from_vars(
            SLUG,
            true,
            vars(&[
                ("APPDATA", PathBuf::from("relative")),
                ("LOCALAPPDATA", root("Local")),
            ]),
        );
        assert!(matches!(
            found,
            Err(LocationError::Relative { var: "APPDATA", .. })
        ));
    }

    #[test]
    fn elsewhere_uses_xdg_folders_or_home() {
        let home = root("home");
        let found = Locations::from_vars(SLUG, false, vars(&[("HOME", home.clone())]));
        let found = found.unwrap();
        assert!(found.settings.starts_with(home.join(".config/edbfi")));
        assert!(found.logs.starts_with(home.join(".local/state/edbfi")));

        let found = Locations::from_vars(
            SLUG,
            false,
            vars(&[
                ("XDG_CONFIG_HOME", root("config")),
                ("XDG_STATE_HOME", PathBuf::from("relative")),
                ("HOME", home.clone()),
            ]),
        )
        .unwrap();
        assert!(found.settings.starts_with(root("config").join("edbfi")));
        assert!(found.logs.starts_with(home.join(".local/state")));

        let found = Locations::from_vars(SLUG, false, vars(&[]));
        assert_eq!(found, Err(LocationError::Unset { var: "HOME" }));
    }
}
