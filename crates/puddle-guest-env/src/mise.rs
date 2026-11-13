// SPDX-License-Identifier: AGPL-3.0-or-later
//! The environment of the managed mise layer: where mise keeps its data on the workspace volume,
//! what it may trust, and the `PATH` that puts its shims first.
//!
//! Every setting is an environment variable, so Puddle writes no `/etc/mise` file and an image's
//! own system config keeps working.

use puddle_types::{GuestEnv, GuestFile, GuestPath};

use crate::{ConfigError, SUDOERS_MISE_GUEST};

/// `PATH` of a Debian image that sets none (what `login` gives a user).
pub const DEBIAN_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Where mise's own folders sit below [`MiseSettings::dir`].
const DATA: &str = "data";
const CACHE: &str = "cache";
const STATE: &str = "state";
const BIN: &str = "bin";
const SHIMS: &str = "shims";

/// Header of the generated sudoers file.
const GENERATED: &str = "Written by Puddle at every boot; changes here are overwritten.";

/// What [`mise_config`] builds from.
///
/// ```
/// use puddle_guest_env::{MiseSettings, mise_config};
/// use puddle_types::GuestPath;
/// let settings = MiseSettings {
///     dir: GuestPath::new("/workspaces/w/.puddle/mise").unwrap(),
///     ceiling: GuestPath::new("/workspaces").unwrap(),
///     trusted: None,
///     image_path: None,
/// };
/// let config = mise_config(&settings).unwrap();
/// assert!(config.env.get("PATH").unwrap().starts_with("/workspaces/w/.puddle/mise/data/shims:"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiseSettings {
    /// Puddle's mise folder on the volume (`<volume>/.puddle/mise`): the binary's `bin`, and
    /// mise's `data`, `cache` and `state`.
    pub dir: GuestPath,
    /// The folder above every workspace's volume: mise looks for config files no higher.
    pub ceiling: GuestPath,
    /// The folder whose config files mise trusts (the workspace's volume), or `None` when the
    /// repository's own code may not run.
    pub trusted: Option<GuestPath>,
    /// The `PATH` the image (or the user's variables) sets, or `None` for the Debian default.
    pub image_path: Option<String>,
}

/// The variables and files of the managed mise layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiseConfig {
    /// Variables for every process in the guest.
    pub env: GuestEnv,
    /// Files for the boot hook: the sudo drop-in.
    pub files: Vec<GuestFile>,
}

/// The variables of mise for one workspace, and the sudo drop-in that keeps them (and the `PATH`)
/// under `sudo`.
///
/// # Errors
///
/// [`ConfigError::ImageEnv`] when the image's `PATH` can't be an environment value;
/// [`ConfigError::GuestPath`] when the sudo drop-in's path is not a valid guest path.
pub fn mise_config(settings: &MiseSettings) -> Result<MiseConfig, ConfigError> {
    let dir = settings.dir.as_str();
    let mut env = GuestEnv::new();
    let mut set = |name: &str, value: &str| {
        env.set(name, value).map_err(|e| ConfigError::ImageEnv {
            name: name.to_owned(),
            reason: e.reason().to_owned(),
        })
    };
    set("MISE_DATA_DIR", &format!("{dir}/{DATA}"))?;
    set("MISE_CACHE_DIR", &format!("{dir}/{CACHE}"))?;
    set("MISE_STATE_DIR", &format!("{dir}/{STATE}"))?;
    // Not the workspace's own folder: a ceiling hides the config file inside it.
    set("MISE_CEILING_PATHS", settings.ceiling.as_str())?;
    if let Some(trusted) = &settings.trusted {
        set("MISE_TRUSTED_CONFIG_PATHS", trusted.as_str())?;
    }
    set("MISE_DISABLE_UPDATE_WARNING", "1")?;
    // mise's anonymous install counter.
    set("MISE_USE_VERSIONS_HOST_TRACK", "0")?;
    let tail = settings.image_path.as_deref().unwrap_or(DEBIAN_PATH);
    let path = format!("{dir}/{DATA}/{SHIMS}:{tail}:{dir}/{BIN}");
    set("PATH", &path)?;

    let file = GuestFile::new(
        GuestPath::new(SUDOERS_MISE_GUEST).map_err(|e| ConfigError::GuestPath {
            reason: e.to_string(),
        })?,
        sudoers(&env, &path).into_bytes(),
    )
    .with_mode(0o440)
    .map_err(|e| ConfigError::GuestPath {
        reason: e.to_string(),
    })?;
    Ok(MiseConfig {
        env,
        files: vec![file],
    })
}

/// `env_keep` for the `MISE_*` names, and `secure_path` so that `sudo node` finds the shims (sudo
/// resets `PATH` to `secure_path` where the image sets one, whatever `env_keep` says). A `PATH`
/// with a character that could end a sudoers string leaves `secure_path` alone: a bad line there
/// would lock `sudo` for everyone.
fn sudoers(env: &GuestEnv, path: &str) -> String {
    let names: Vec<&str> = env
        .iter()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("MISE_"))
        .collect();
    let secure_path = if path.chars().all(is_plain_path_char) {
        format!("Defaults secure_path=\"{path}\"\n")
    } else {
        String::new()
    };
    format!(
        "# {GENERATED}\n# Keeps mise's variables under sudo.\nDefaults env_keep += \"{}\"\n{secure_path}",
        names.join(" ")
    )
}

fn is_plain_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '/' | ':' | '.' | '_' | '-' | '+' | '@' | '~' | ',')
}

#[cfg(test)]
mod tests;
