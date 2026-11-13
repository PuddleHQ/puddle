// SPDX-License-Identifier: GPL-3.0-or-later
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
use puddle_types::{ApplyKind, GuestEnv, GuestFile, GuestPath, SessionUser};

use crate::assets::{AGENT_GUEST, BOOT_SH_GUEST, MOUNT_DIR_GUEST, guest_path};
use crate::machine::machine_settings;
use crate::quote::{printf_format, sh_word};

mod shell_env;
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

/// The sudo rule that gives a non-root session user root without a password.
pub const SUDOERS_SESSION_GUEST: &str = "/etc/sudoers.d/puddle-session";

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
    #[error("the image's {what} contains a NUL or newline")]
    ImageValue {
        /// `ENV PATH` or `ENTRYPOINT/CMD`.
        what: &'static str,
    },
    /// A setting for the agent can't be passed on.
    #[error("agent setting {name}: {reason}")]
    AgentSetting {
        /// The variable's name.
        name: String,
        /// What is wrong.
        reason: &'static str,
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

/// The git identity Puddle writes into the sandbox (a basic identities feature: the user
/// sets it in Puddle, nothing is copied from the host's `.gitconfig`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIdentity {
    name: String,
    email: String,
}

impl GitIdentity {
    /// Checks and wraps `user.name` and `user.email`.
    ///
    /// # Errors
    ///
    /// [`PlanError::GitIdentity`] when either is empty or contains a control character.
    pub fn new(name: &str, email: &str) -> Result<Self, PlanError> {
        for (field, value) in [("user.name", name), ("user.email", email)] {
            if value.trim().is_empty() {
                return Err(PlanError::GitIdentity {
                    field,
                    reason: "is empty",
                });
            }
            if value.chars().any(char::is_control) {
                return Err(PlanError::GitIdentity {
                    field,
                    reason: "contains a control character",
                });
            }
        }
        Ok(Self {
            name: name.to_owned(),
            email: email.to_owned(),
        })
    }

    /// `user.name`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `user.email`.
    #[must_use]
    pub fn email(&self) -> &str {
        &self.email
    }
}

/// An author for the repositories that have a remote matching one of `globs`: git's
/// `includeIf "hasconfig:remote.*.url:<glob>"`. The match is on any remote of the repository, is
/// case-sensitive, and `pushurl` is ignored.
///
/// Rules go into the plan in git's order of precedence: when a repository matches several, the
/// one written **last** wins, whatever the order of its remotes. The caller orders them, lowest
/// priority first. The author is a convenience for the user's commits, not a security control:
/// the guest can set any `user.email` in a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitAuthorRule {
    globs: Vec<String>,
    author: GitIdentity,
}

impl GitAuthorRule {
    /// Checks and wraps a rule.
    ///
    /// # Errors
    ///
    /// [`PlanError::GitAuthorRule`] when there is no glob, too many, or one is empty, too long or
    /// holds a control character, a quote or a backslash (nothing a remote glob needs).
    pub fn new(globs: Vec<String>, author: GitIdentity) -> Result<Self, PlanError> {
        let bad = |reason| Err(PlanError::GitAuthorRule { reason });
        if globs.is_empty() {
            return bad("has no remote glob");
        }
        if globs.len() > MAX_GIT_AUTHOR_GLOBS {
            return bad("has too many remote globs");
        }
        for glob in &globs {
            if glob.is_empty() || glob.len() > 1024 {
                return bad("has an empty or over-long remote glob");
            }
            if glob
                .chars()
                .any(|c| c.is_control() || c == '"' || c == '\\')
            {
                return bad("has a remote glob with a control character, quote or backslash");
            }
        }
        Ok(Self { globs, author })
    }

    /// The remote globs.
    #[must_use]
    pub fn globs(&self) -> &[String] {
        &self.globs
    }

    /// The author.
    #[must_use]
    pub fn author(&self) -> &GitIdentity {
        &self.author
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
            binary: guest_path(AGENT_GUEST),
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
            .map_or_else(|| guest_path(AGENT_GUEST), |a| a.binary.clone())
    }

    /// Whether any file is applied with [`ApplyKind::Merge`].
    #[must_use]
    pub fn has_merged_files(&self) -> bool {
        self.files
            .iter()
            .any(|f| matches!(f.apply(), ApplyKind::Merge(_)))
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
    /// editor's server. Applied when the agent next starts: the supervisor reads the latest boot's
    /// settings before each start. Needs an agent in the plan.
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
    /// resolver stays as the guest had it.
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
    /// - [`PlanError::ImageValue`]: the image's `PATH` or ENTRYPOINT/CMD holds a NUL or newline;
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
        let mut entry_env = vec![guest_path(ENV_FILE_GUEST)];
        files.push(env_file(&self.env));
        if let Some(path) = self.image.env_var("PATH") {
            if path.contains(['\0', '\n']) {
                return Err(PlanError::ImageValue { what: "ENV PATH" });
            }
            files.push(GuestFile::new(
                guest_path(PATH_FILE_GUEST),
                format!("{GENERATED}export PATH={}\n", sh_word(path)).into_bytes(),
            ));
            entry_env.push(guest_path(PATH_FILE_GUEST));
        }
        if self.git_rules.len() > MAX_GIT_AUTHOR_RULES {
            return Err(PlanError::GitAuthorRule {
                reason: "has more rules than a plan holds",
            });
        }
        files.push(git_config(&self));
        files.extend(git_author_files(&self.git_rules));
        let port = self.agent.as_ref().map_or(DEFAULT_AGENT_PORT, |a| a.port);
        files.extend(session_files(port, self.session_user));
        files.extend(shell_env::files(&self.env)?);
        files.extend(self.files);

        let mounts = guest_path(MOUNT_DIR_GUEST);
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
                return Err(PlanError::ImageValue {
                    what: "ENTRYPOINT/CMD",
                });
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
fn session_files(agent_port: u16, user: SessionUser) -> Vec<GuestFile> {
    let mut files = vec![machine_settings(agent_port, user)];
    if !user.is_root() {
        let text = format!("{GENERATED}{} ALL=(ALL:ALL) NOPASSWD: ALL\n", user.name());
        #[expect(
            clippy::expect_used,
            reason = "invariant: 0o440 is a valid file mode (the boot tests write it)"
        )]
        let sudoers = GuestFile::new(guest_path(SUDOERS_SESSION_GUEST), text.into_bytes())
            .with_mode(0o440)
            .expect("0o440 is a valid mode");
        files.push(sudoers);
    }
    files
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
    GuestFile::new(guest_path(ENV_FILE_GUEST), text.into_bytes())
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
    GuestFile::new(guest_path(START_FOLDER_FILE_GUEST), text.into_bytes())
}

/// A quoted git config value.
fn git_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A `[user]` section.
fn git_user(id: &GitIdentity) -> String {
    format!(
        "[user]\n\tname = {}\n\temail = {}\n",
        git_quote(&id.name),
        git_quote(&id.email)
    )
}

/// The file name of the author of rule number `n` (counted from 1), relative to
/// [`GIT_CONFIG_GUEST`]'s directory.
fn git_author_name(n: usize) -> String {
    format!("git-author-{n}.gitconfig")
}

/// Puddle's git settings: `core.fsync=committed` always (ADR 0006: no lost commits on a VM
/// kill), the identity when set, then one `includeIf` per remote glob of each author rule, in
/// order (git lets the last match win), each pointing at that rule's author file. Never a
/// credential helper. With a session user other than root, `safe.directory = *`: the checkouts
/// belong to that user, and a root shell (`sudo -i`) would otherwise find every one refused as
/// "dubious ownership". The check guards one user from another's repository, and in a guest
/// whose only user becomes root at will there is no other user to guard.
fn git_config(plan: &BootPlanBuilder) -> GuestFile {
    let mut text = String::from(GENERATED);
    text.push_str("[core]\n\tfsync = committed\n");
    if !plan.session_user.is_root() {
        text.push_str("[safe]\n\tdirectory = *\n");
    }
    if let Some(id) = &plan.git {
        text.push_str(&git_user(id));
    }
    for (i, rule) in plan.git_rules.iter().enumerate() {
        for glob in &rule.globs {
            text.extend([
                "[includeIf \"hasconfig:remote.*.url:",
                glob,
                "\"]\n\tpath = ",
                &git_author_name(i + 1),
                "\n",
            ]);
        }
    }
    GuestFile::new(guest_path(GIT_CONFIG_GUEST), text.into_bytes())
}

/// The author file of each rule: only a `[user]` section (an included file may not define
/// remotes, and needs nothing else).
fn git_author_files(rules: &[GitAuthorRule]) -> Vec<GuestFile> {
    rules
        .iter()
        .enumerate()
        .map(|(i, rule)| {
            GuestFile::new(
                guest_path(&format!(
                    "{GIT_AUTHOR_DIR_GUEST}/{}",
                    git_author_name(i + 1)
                )),
                format!("{GENERATED}{}", git_user(&rule.author)).into_bytes(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests;
