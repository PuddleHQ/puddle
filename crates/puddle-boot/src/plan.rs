// SPDX-License-Identifier: AGPL-3.0-or-later
//! The boot plan: everything one run of `boot.sh` applies, rendered as the shell fragment it
//! reads on stdin.
//!
//! The hook holds no provider logic. Providers (proxy config, corporate roots, the
//! per-sandbox CA) hand over [`GuestFile`]s and a [`GuestEnv`]; the plan adds Puddle's own files
//! (the image `PATH` fix, the env file for login shells, the git settings and VS Code's Machine
//! settings) and the image's ENTRYPOINT, and checks that no two of them claim the same path.

use std::collections::BTreeSet;
use std::time::Duration;

use puddle_compute::{ExecRequest, ImageConfig};
use puddle_types::{ApplyKind, FileMode, GuestEnv, GuestFile, GuestPath, SessionUser, guest_path};

use crate::assets::{AGENT_GUEST, BOOT_SH_GUEST, MOUNT_DIR_GUEST};
use crate::machine::machine_settings;
use crate::quote::{printf_format, sh_word};

mod git;
mod shell_env;
pub use git::{GitAuthorRule, GitIdentity};
use git::{git_author_files, git_config};
pub use shell_env::{BASH_RC_GUEST, SUDOERS_ENV_GUEST, ZSH_ENV_GUEST};

/// The plan's first line; `boot.sh` refuses any other.
pub const PLAN_HEADER: &str = "# puddle-boot-plan 1";

/// Restores the image's `ENV PATH` in login shells, which Debian's `/etc/profile` resets.
pub const PATH_FILE_GUEST: &str = "/etc/profile.d/00-puddle-path.sh";

/// The provider environment, for login shells and the chained ENTRYPOINT.
pub const ENV_FILE_GUEST: &str = "/etc/profile.d/01-puddle-env.sh";

/// Moves an interactive login shell that starts in the home directory to the workspace's start
/// folder. It sorts after the environment file and the shell init loader.
pub const START_FOLDER_FILE_GUEST: &str = "/etc/profile.d/03-puddle-start-folder.sh";

/// The login-shell file that shows the workspace's name where an interactive bash prompt shows the
/// host name (see [`prompt_name_file`]).
pub const PROMPT_NAME_FILE_GUEST: &str = "/etc/profile.d/04-puddle-prompt-name.sh";

/// The sudo rule that gives a non-root session user root without a password.
pub const SUDOERS_SESSION_GUEST: &str = "/etc/sudoers.d/puddle-session";

/// sudo refuses a sudoers file others can write; `0440` is what it expects.
const SUDOERS_MODE: FileMode = FileMode::new(0o440);

/// Puddle's git settings, included from `/etc/gitconfig`.
pub const GIT_CONFIG_GUEST: &str = "/etc/puddle/gitconfig";

/// The directory of the per-rule author files [`GitAuthorRule`] writes, next to
/// [`GIT_CONFIG_GUEST`] (which includes them by relative path).
pub const GIT_AUTHOR_DIR_GUEST: &str = "/etc/puddle";

/// The most author rules a plan holds.
pub const MAX_GIT_AUTHOR_RULES: usize = 64;

/// The most remote globs one author rule holds.
pub const MAX_GIT_AUTHOR_GLOBS: usize = 32;

/// When a file below this directory changes, the hook runs `update-ca-certificates`; when none
/// did, it skips it (it costs 0.6–0.9 s per boot).
pub const CA_DIR_GUEST: &str = "/usr/local/share/ca-certificates";

/// The port `puddle-agent` listens on in the guest (the proxy address in `HTTP(S)_PROXY`).
pub const DEFAULT_AGENT_PORT: u16 = 3128;

/// Env names the hook keeps for itself.
const RESERVED_ENV_PREFIX: &str = "PUDDLE_";

const GENERATED: &str = "# Written by Puddle at every boot; changes here are overwritten.\n";

/// Why a plan can't be built. Every variant names the offending item.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PlanError {
    /// Two files (from providers or Puddle) claim the same guest path.
    #[error("two boot files claim {path}")]
    DuplicatePath {
        /// The path.
        path: String,
    },
    /// A file would land below Puddle's read-only mounts.
    #[error("boot file {path} is below {MOUNT_DIR_GUEST}, which holds Puddle's read-only mounts")]
    ReservedPath {
        /// The path.
        path: String,
    },
    /// A path contains a control character, which the hook's file list can't hold.
    #[error("boot file path {path:?} contains a control character")]
    UnsafePath {
        /// The path.
        path: String,
    },
    /// A provider env name is reserved for the hook.
    #[error("environment variable {name} is reserved for Puddle's boot hook")]
    ReservedEnv {
        /// The name.
        name: String,
    },
    /// The git identity can't be written to a git config file.
    #[error("git {field} {reason}")]
    GitIdentity {
        /// `user.name` or `user.email`.
        field: &'static str,
        /// What is wrong.
        reason: &'static str,
    },
    /// An author rule can't be written to a git config file.
    #[error("git author rule {reason}")]
    GitAuthorRule {
        /// What is wrong.
        reason: &'static str,
    },
    /// A file asks for an [`ApplyKind`] this hook doesn't know.
    #[error("boot file {path} has an apply kind this boot hook can't apply")]
    UnknownApplyKind {
        /// The path.
        path: String,
    },
    /// A value from the image can't be passed on.
    #[error("the image's {0}")]
    ImageValue(ImageValue),
    /// A setting for the agent can't be passed on.
    #[error("agent setting {name}: {reason}")]
    AgentSetting {
        /// The variable's name.
        name: String,
        /// What is wrong.
        reason: &'static str,
    },
    /// The VS Code Machine settings can't be written as a merge.
    #[error("the VS Code Machine settings: {reason}")]
    MachineSettings {
        /// What is wrong.
        reason: String,
    },
    /// The environment can't be written to the `sudo` and shell files.
    #[error("the environment can't be written to the sudo and shell files: {reason}")]
    ShellEnv {
        /// What is wrong.
        reason: String,
    },
    /// A provider step names a script the plan doesn't write.
    #[error("boot step {path} is not a file of this plan")]
    StepNotInPlan {
        /// The step's path.
        path: String,
    },
}

/// An image value the plan can't pass on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageValue {
    /// `ENV PATH`: written as one line of a file, so a NUL or a newline can't be in it.
    Path,
    /// ENTRYPOINT/CMD: exec arguments, so a NUL can't be in them (a newline can).
    Command,
}

impl std::fmt::Display for ImageValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Path => "ENV PATH contains a NUL or newline",
            Self::Command => "ENTRYPOINT/CMD contains a NUL",
        })
    }
}

/// Where the hook finds `puddle-agent` and the port it waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    /// The binary in the guest (a read-only mount).
    pub binary: GuestPath,
    /// The port on 127.0.0.1 the hook waits for before it reports ready.
    pub port: u16,
}

impl Default for AgentConfig {
    /// [`AGENT_GUEST`] on [`DEFAULT_AGENT_PORT`].
    fn default() -> Self {
        Self {
            binary: guest_path!(AGENT_GUEST),
            port: DEFAULT_AGENT_PORT,
        }
    }
}

/// One boot's worth of work for `boot.sh`. Build it with [`BootPlan::builder`].
///
/// ```
/// use puddle_boot::{BootPlan, GitIdentity};
/// use puddle_compute::ImageConfig;
/// use puddle_types::{GuestEnv, GuestFile, GuestPath};
///
/// let image = ImageConfig {
///     entrypoint: vec!["dockerd-entrypoint.sh".into()],
///     env: vec![("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into())],
///     ..ImageConfig::default()
/// };
/// let mut env = GuestEnv::new();
/// env.set("HTTPS_PROXY", "http://127.0.0.1:3128").unwrap();
/// let plan = BootPlan::builder(&image)
///     .env(&env)
///     .file(GuestFile::new(GuestPath::new("/etc/npmrc").unwrap(), b"proxy=x\n".to_vec()))
///     .git_identity(GitIdentity::new("Ada", "ada@example.org").unwrap())
///     .build()
///     .unwrap();
/// assert_eq!(plan.entrypoint(), Some(&["dockerd-entrypoint.sh".to_owned()][..]));
/// assert!(plan.files().iter().any(|f| f.path().as_str() == "/etc/npmrc"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootPlan {
    files: Vec<GuestFile>,
    steps: Vec<GuestPath>,
    entry_env: Vec<GuestPath>,
    entrypoint: Vec<String>,
    entrypoint_cwd: Option<GuestPath>,
    agent: Option<AgentConfig>,
    agent_env: Vec<(String, String)>,
    capture: bool,
    session_user: SessionUser,
}

/// Builds a [`BootPlan`]; see there.
#[derive(Debug, Clone)]
#[must_use]
pub struct BootPlanBuilder {
    image: ImageConfig,
    env: GuestEnv,
    files: Vec<GuestFile>,
    steps: Vec<GuestPath>,
    git: Option<GitIdentity>,
    git_rules: Vec<GitAuthorRule>,
    agent: Option<AgentConfig>,
    agent_env: Vec<(String, String)>,
    capture: bool,
    session_user: SessionUser,
}

impl BootPlan {
    /// The most of the boot hook's stdout and stderr the host keeps (256 KiB each): a hostile
    /// guest cannot grow host memory through it.
    pub const HOOK_MAX_OUTPUT: usize = 256 * 1024;

    /// A builder for a sandbox of an image with `image`'s config (from
    /// [`puddle_compute::Runtime::pull_image`]). By default the plan starts the agent with
    /// [`AgentConfig::default`].
    pub fn builder(image: &ImageConfig) -> BootPlanBuilder {
        BootPlanBuilder {
            image: image.clone(),
            env: GuestEnv::new(),
            files: Vec::new(),
            steps: Vec::new(),
            git: None,
            git_rules: Vec::new(),
            agent: Some(AgentConfig::default()),
            agent_env: Vec::new(),
            capture: true,
            session_user: SessionUser::Root,
        }
    }

    /// The guest user sessions run as; the hook makes it when the image has none.
    #[must_use]
    pub fn session_user(&self) -> SessionUser {
        self.session_user
    }

    /// Every file the hook writes, Puddle's own first, then the providers' in the order given.
    #[must_use]
    pub fn files(&self) -> &[GuestFile] {
        &self.files
    }

    /// Provider steps: scripts of this plan the hook runs with `/bin/sh` at every boot, in this
    /// order, after the files and `update-ca-certificates`.
    #[must_use]
    pub fn steps(&self) -> &[GuestPath] {
        &self.steps
    }

    /// The command the hook chains after setup: the image's ENTRYPOINT + CMD, or `None` when the
    /// image declares no ENTRYPOINT (a CMD alone, such as `bash`, is the interactive default,
    /// not a service).
    #[must_use]
    pub fn entrypoint(&self) -> Option<&[String]> {
        (!self.entrypoint.is_empty()).then_some(self.entrypoint.as_slice())
    }

    /// The agent the hook starts and waits for, if any.
    #[must_use]
    pub fn agent(&self) -> Option<&AgentConfig> {
        self.agent.as_ref()
    }

    /// The settings the agent starts with (`PUDDLE_AGENT_*` variables), in the order given.
    #[must_use]
    pub fn agent_env(&self) -> &[(String, String)] {
        &self.agent_env
    }

    /// Whether the hook turns transparent capture on: the agent starts with it and the guest's
    /// resolver is pointed at the stub once that answers. Always false without an agent.
    #[must_use]
    pub fn capture(&self) -> bool {
        self.capture && self.agent.is_some()
    }

    /// The binary that applies merged files (`puddle-agent merge-file`): the agent's,
    /// or [`AGENT_GUEST`] when the plan starts no agent. It must be mounted when
    /// [`BootPlan::has_merged_files`]; the hook also needs it to remove the keys of a merged file
    /// an earlier plan listed, and keeps that record for a later boot when it is missing.
    #[must_use]
    pub fn merge_tool(&self) -> GuestPath {
        self.agent
            .as_ref()
            .map_or_else(|| guest_path!(AGENT_GUEST), |a| a.binary.clone())
    }

    /// Whether any file is applied with [`ApplyKind::Merge`].
    #[must_use]
    pub fn has_merged_files(&self) -> bool {
        self.files
            .iter()
            .any(|f| matches!(f.apply(), ApplyKind::Merge(_)))
    }

    /// The guest paths of the files applied with [`ApplyKind::Merge`].
    #[must_use]
    pub fn merged_paths(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|f| matches!(f.apply(), ApplyKind::Merge(_)))
            .map(|f| f.path().as_str().to_owned())
            .collect()
    }

    /// The plan as `boot.sh` reads it on stdin.
    #[must_use]
    pub fn render(&self) -> Vec<u8> {
        let mut lines = vec![
            PLAN_HEADER.to_owned(),
            format!("puddle_merge_tool {}", sh_word(self.merge_tool().as_str())),
        ];
        // First, so the files below the user's home are made as that user's.
        if !self.session_user.is_root() {
            lines.push(format!(
                "puddle_session_user {} {}",
                sh_word(self.session_user.name()),
                sh_word(self.session_user.home())
            ));
        }
        for f in &self.files {
            if f.session_owned() {
                lines.push(format!(
                    "puddle_session_owned {}",
                    sh_word(f.path().as_str())
                ));
            }
            if matches!(f.apply(), ApplyKind::Keep) {
                lines.push(format!("puddle_keep {}", sh_word(f.path().as_str())));
                continue;
            }
            let (call, body) = match f.apply() {
                ApplyKind::Merge(spec) => (
                    "puddle_merge",
                    // A spec of strings always serialises; an empty one would fail the boot
                    // loudly ("bad merge spec"), never write a wrong file.
                    serde_json::to_vec(spec).unwrap_or_default(),
                ),
                // `build` refuses kinds this hook doesn't know.
                _ => ("puddle_file", f.contents().to_vec()),
            };
            lines.push(format!(
                "{call} {} {:04o} {}",
                sh_word(f.path().as_str()),
                f.mode(),
                printf_format(&body)
            ));
        }
        lines.push(format!(
            "puddle_on_change {} update-ca-certificates",
            sh_word(CA_DIR_GUEST)
        ));
        for step in &self.steps {
            lines.push(format!("puddle_step {}", sh_word(step.as_str())));
        }
        lines.push(format!("puddle_git_include {}", sh_word(GIT_CONFIG_GUEST)));
        if let Some(agent) = &self.agent {
            lines.push(format!(
                "puddle_agent {} {}",
                sh_word(agent.binary.as_str()),
                agent.port
            ));
            for (name, value) in &self.agent_env {
                lines.push(format!(
                    "puddle_agent_env {} {}",
                    sh_word(name),
                    sh_word(value)
                ));
            }
            let mode = if self.capture { "on" } else { "off" };
            lines.push(format!("puddle_capture {mode}"));
        }
        for p in &self.entry_env {
            lines.push(format!("puddle_entrypoint_env {}", sh_word(p.as_str())));
        }
        if let Some(cwd) = &self.entrypoint_cwd {
            lines.push(format!("puddle_entrypoint_cwd {}", sh_word(cwd.as_str())));
        }
        lines.push("puddle_plan_end\n".to_owned());
        lines.join("\n").into_bytes()
    }

    /// The exec that runs the hook: `/bin/sh /puddle/boot.sh [ENTRYPOINT CMD...]` as root, the
    /// plan on stdin, `PUDDLE_ROOT` forced empty (it is for the unit tests only).
    #[must_use]
    pub fn exec_request(&self, timeout: Duration) -> ExecRequest {
        let mut env = GuestEnv::new();
        // A fixed, valid name and value: can't fail.
        let _ = env.set("PUDDLE_ROOT", "");
        ExecRequest::new(
            "/bin/sh",
            std::iter::once(BOOT_SH_GUEST.to_owned()).chain(self.entrypoint.iter().cloned()),
        )
        .as_user("root")
        .with_env(&env)
        .with_stdin(self.render())
        .with_timeout(timeout)
        .with_max_output(Self::HOOK_MAX_OUTPUT)
    }
}

impl BootPlanBuilder {
    /// The provider environment (merged by the caller, who also puts it into the
    /// [`puddle_compute::SandboxSpec`] env). The hook writes it to [`ENV_FILE_GUEST`] for login
    /// shells and sources it before chaining the ENTRYPOINT.
    pub fn env(mut self, env: &GuestEnv) -> Self {
        self.env.extend(env);
        self
    }

    /// Adds one provider file.
    pub fn file(mut self, file: GuestFile) -> Self {
        self.files.push(file);
        self
    }

    /// Adds provider files, in order.
    pub fn files(mut self, files: impl IntoIterator<Item = GuestFile>) -> Self {
        self.files.extend(files);
        self
    }

    /// Runs `script`, one of the plan's provider files, with `/bin/sh` at every boot after the
    /// files and `update-ca-certificates` (a provider's own setup that needs the guest's state,
    /// such as merging the image's CA bundle). Steps run in the order added; a step added
    /// twice runs once. A failing step fails the boot with its output.
    pub fn step(mut self, script: GuestPath) -> Self {
        if !self.steps.contains(&script) {
            self.steps.push(script);
        }
        self
    }

    /// Sets `user.name` and `user.email` for the sandbox: the author of a repository no
    /// [`GitAuthorRule`] matches.
    pub fn git_identity(mut self, identity: GitIdentity) -> Self {
        self.git = Some(identity);
        self
    }

    /// Adds an author for the repositories whose remotes match the rule's globs. Rules are
    /// written in the order added, and git lets the last matching one win, so add the lowest
    /// priority first. At most [`MAX_GIT_AUTHOR_RULES`] fit in a plan.
    pub fn git_author_rule(mut self, rule: GitAuthorRule) -> Self {
        self.git_rules.push(rule);
        self
    }

    /// Starts this agent instead of the default one.
    pub fn agent(mut self, agent: AgentConfig) -> Self {
        self.agent = Some(agent);
        self
    }

    /// Starts the agent with the setting `name` (`PUDDLE_AGENT_*`), for example the browser
    /// editor's server. Applied in this boot: an agent that already runs with other settings (a
    /// re-adopted sandbox) is restarted with these. Needs an agent in the plan.
    pub fn agent_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.agent_env.push((name.into(), value.into()));
        self
    }

    /// Runs sessions as `user` (root by default): the hook makes the user when the image has none,
    /// the plan's files below its home are that user's, and for a non-root user the plan writes
    /// [`SUDOERS_SESSION_GUEST`] so the user becomes root with `sudo`.
    pub fn session_user(mut self, user: SessionUser) -> Self {
        self.session_user = user;
        self
    }

    /// Turns transparent capture off (on by default): the agent starts without it and the
    /// resolver stays as the guest had it. A switch either way on a running sandbox restarts its
    /// agent, as any change of [`Self::agent_env`] does.
    pub fn no_capture(mut self) -> Self {
        self.capture = false;
        self
    }

    /// Starts no agent (e.g. a short-lived maintenance sandbox).
    pub fn no_agent(mut self) -> Self {
        self.agent = None;
        self
    }

    /// Checks everything and builds the plan.
    ///
    /// # Errors
    ///
    /// - [`PlanError::DuplicatePath`]: two files claim a path (also a provider file on one of
    ///   Puddle's own paths);
    /// - [`PlanError::ReservedPath`], [`PlanError::UnsafePath`]: a file path is unusable;
    /// - [`PlanError::ReservedEnv`]: an env name starts with `PUDDLE_`;
    /// - [`PlanError::ImageValue`]: the image's `PATH` holds a NUL or newline, or its
    ///   ENTRYPOINT/CMD a NUL;
    /// - [`PlanError::StepNotInPlan`]: a step names no file of the plan;
    /// - [`PlanError::GitAuthorRule`]: more than [`MAX_GIT_AUTHOR_RULES`] author rules.
    pub fn build(self) -> Result<BootPlan, PlanError> {
        if let Some((name, _)) = self
            .env
            .iter()
            .find(|(n, _)| n.starts_with(RESERVED_ENV_PREFIX))
        {
            return Err(PlanError::ReservedEnv {
                name: name.to_owned(),
            });
        }

        let mut files = Vec::new();
        let mut entry_env = vec![guest_path!(ENV_FILE_GUEST)];
        files.push(env_file(&self.env));
        // A PATH among Puddle's own variables (the managed tools' shims first) is the one login
        // shells get too: Debian's /etc/profile resets PATH before the profile.d files run.
        if let Some(path) = self.env.get("PATH").or_else(|| self.image.env_var("PATH")) {
            if path.contains(['\0', '\n']) {
                return Err(PlanError::ImageValue(ImageValue::Path));
            }
            files.push(GuestFile::new(
                guest_path!(PATH_FILE_GUEST),
                format!("{GENERATED}export PATH={}\n", sh_word(path)).into_bytes(),
            ));
            entry_env.push(guest_path!(PATH_FILE_GUEST));
        }
        if self.git_rules.len() > MAX_GIT_AUTHOR_RULES {
            return Err(PlanError::GitAuthorRule {
                reason: "has more rules than a plan holds",
            });
        }
        // The author files first: the config that names them is written last, so the guest never
        // holds a config that points at a file not written yet.
        files.extend(git_author_files(&self.git_rules)?);
        files.push(git_config(&self));
        let port = self.agent.as_ref().map_or(DEFAULT_AGENT_PORT, |a| a.port);
        files.extend(session_files(port, self.session_user)?);
        files.extend(shell_env::files(&self.env)?);
        files.extend(self.files);

        let mounts = guest_path!(MOUNT_DIR_GUEST);
        let mut seen = BTreeSet::new();
        for f in &files {
            let path = f.path();
            if path.as_str().chars().any(char::is_control) {
                return Err(PlanError::UnsafePath {
                    path: path.to_string(),
                });
            }
            if path.is_within(&mounts) {
                return Err(PlanError::ReservedPath {
                    path: path.to_string(),
                });
            }
            let known = matches!(f.apply(), ApplyKind::Replace | ApplyKind::Merge(_));
            if !known && !matches!(f.apply(), ApplyKind::Keep) {
                return Err(PlanError::UnknownApplyKind {
                    path: path.to_string(),
                });
            }
            if !seen.insert(path.as_str()) {
                return Err(PlanError::DuplicatePath {
                    path: path.to_string(),
                });
            }
        }

        if let Some(step) = self.steps.iter().find(|s| !seen.contains(s.as_str())) {
            return Err(PlanError::StepNotInPlan {
                path: step.to_string(),
            });
        }

        let entrypoint = if self.image.entrypoint.is_empty() {
            Vec::new()
        } else {
            let all: Vec<String> = self
                .image
                .entrypoint
                .iter()
                .chain(&self.image.cmd)
                .cloned()
                .collect();
            if all.iter().any(|a| a.contains('\0')) {
                return Err(PlanError::ImageValue(ImageValue::Command));
            }
            all
        };
        // An unusable WORKDIR (relative, `..`) from the image is ignored: the ENTRYPOINT then
        // starts in `/`, as the hook's own working directory.
        let entrypoint_cwd = self
            .image
            .working_dir
            .as_deref()
            .and_then(|d| GuestPath::new(d).ok())
            .filter(|_| !entrypoint.is_empty());

        check_agent_env(&self.agent_env, self.agent.is_some())?;

        Ok(BootPlan {
            files,
            steps: self.steps,
            entry_env,
            entrypoint,
            entrypoint_cwd,
            agent: self.agent,
            agent_env: self.agent_env,
            capture: self.capture,
            session_user: self.session_user,
        })
    }
}

/// The session user's own files: VS Code's Machine settings in its home and, for a user other
/// than root, [`SUDOERS_SESSION_GUEST`] (every command as any user, no password).
fn session_files(agent_port: u16, user: SessionUser) -> Result<Vec<GuestFile>, PlanError> {
    let machine = machine_settings(agent_port, user).map_err(|e| PlanError::MachineSettings {
        reason: e.to_string(),
    })?;
    let mut files = vec![machine];
    if !user.is_root() {
        let text = format!("{GENERATED}{} ALL=(ALL:ALL) NOPASSWD: ALL\n", user.name());
        files.push(
            GuestFile::new(guest_path!(SUDOERS_SESSION_GUEST), text.into_bytes())
                .with_mode(SUDOERS_MODE),
        );
    }
    Ok(files)
}

/// Refuses settings the agent can't take: none without an agent, names outside `PUDDLE_AGENT_`,
/// a NUL or newline in a value, a name twice.
fn check_agent_env(env: &[(String, String)], has_agent: bool) -> Result<(), PlanError> {
    if !has_agent && let Some((name, _)) = env.first() {
        return Err(PlanError::AgentSetting {
            name: name.clone(),
            reason: "the plan starts no agent",
        });
    }
    let mut named = BTreeSet::new();
    for (name, value) in env {
        let fail = |reason| PlanError::AgentSetting {
            name: name.clone(),
            reason,
        };
        let plain = name.strip_prefix("PUDDLE_AGENT_").is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        });
        if !plain {
            return Err(fail("expected a PUDDLE_AGENT_ name in capitals"));
        }
        if value.contains(['\0', '\n']) {
            return Err(fail("the value contains a NUL or newline"));
        }
        if !named.insert(name.as_str()) {
            return Err(fail("set twice"));
        }
    }
    Ok(())
}

fn env_file(env: &GuestEnv) -> GuestFile {
    let mut text = String::from(GENERATED);
    for (name, value) in env.iter() {
        text.extend(["export ", name, "=", &sh_word(value), "\n"]);
    }
    GuestFile::new(guest_path!(ENV_FILE_GUEST), text.into_bytes())
}

/// The login-shell file that opens terminals in `folder`, the start folder inside the workspace's
/// volume `root`.
///
/// Only an interactive shell that starts in the home directory moves: a command run over SSH
/// without a terminal, a shell started in any other folder and a shell that is not a login shell
/// stay where they are. A folder that does not exist (yet) falls back to `root`.
#[must_use]
pub fn start_folder_file(root: &GuestPath, folder: &GuestPath) -> GuestFile {
    let text = format!(
        "{GENERATED}case $- in\n*i*)\n\tif [ \"$PWD\" = \"$HOME\" ]; then\n\t\tif [ -d {folder} ]; then\n\t\t\tcd {folder}\n\t\telif [ -d {root} ]; then\n\t\t\tcd {root}\n\t\tfi\n\tfi\n\t;;\nesac\n",
        folder = sh_word(folder.as_str()),
        root = sh_word(root.as_str()),
    );
    GuestFile::new(guest_path!(START_FOLDER_FILE_GUEST), text.into_bytes())
}

/// The login-shell file that makes an interactive bash prompt, and the terminal title that comes
/// with it, read `agent@<name>` instead of `agent@<guest host name>`.
///
/// The guest's host name stays the workspace id (a typed name is never an identifier), so the
/// file swaps the `\h` and `\H` of whatever `PS1` the user's rc files set, from `PROMPT_COMMAND`
/// (the rc files run after this file, so the swap has to wait for the first prompt). The name sits
/// in a variable that the prompt expands, so a `$`, a backtick or a backslash in it is text, never
/// code. Other shells and a non-interactive bash are left alone.
#[must_use]
pub fn prompt_name_file(name: &str) -> GuestFile {
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let text = format!(
        "{GENERATED}[ -n \"${{BASH_VERSION-}}\" ] || return 0\n\
         case $- in *i*) ;; *) return 0 ;; esac\n\
         shopt -q promptvars || return 0\n\
         __puddle_name={name}\n\
         __puddle_prompt() {{\n\
         \tcase $PS1 in\n\
         \t*'\\h'* | *'\\H'*)\n\
         \t\tPS1=${{PS1//'\\h'/'${{__puddle_name}}'}}\n\
         \t\tPS1=${{PS1//'\\H'/'${{__puddle_name}}'}}\n\
         \t\t;;\n\
         \tesac\n\
         }}\n\
         PROMPT_COMMAND=\"__puddle_prompt${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}\"\n",
        name = sh_word(&name),
    );
    GuestFile::new(guest_path!(PROMPT_NAME_FILE_GUEST), text.into_bytes())
}

#[cfg(test)]
mod tests;
