// SPDX-License-Identifier: AGPL-3.0-or-later
//! One isolated msb world per test process: settings, private home, scoped SDK backend.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use microsandbox::sandbox::SandboxBuilder;
use microsandbox::{Backend, LocalBackend, Sandbox};
use puddle_types::SandboxName;

use crate::error::HarnessError;
use crate::prefix::RunPrefix;
use crate::runtime::{RuntimePair, refuse_ambient_msb_vars};
use crate::{MSB_LOG_LEVEL_VAR, PREFIX_VAR, ROOT_VAR, RUN_LABEL, RUNTIME_DIR_VAR};

/// Memory for harness sandboxes: enough for the devcontainer image, small enough that three
/// runs fit a 2-vCPU / 8 GB hosted runner.
const SANDBOX_MEMORY_MIB: u32 = 512;

/// How long cleanup waits for a sandbox to stop before killing it.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a run's runtime, prefix and private home come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Directory holding `msb` and `libkrunfw`.
    pub runtime_dir: PathBuf,
    /// The run prefix.
    pub prefix: RunPrefix,
    /// Directory the private home goes under.
    pub root: PathBuf,
    /// Default log level of msb's sandbox runtimes (`runtime.log`); `None` leaves them silent.
    pub msb_log_level: Option<String>,
}

/// The levels msb's `config.json` accepts for `log_level`.
const MSB_LOG_LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];

impl Settings {
    /// Reads the settings through `lookup` (the process environment in [`VmEnv::from_env`]).
    ///
    /// # Errors
    ///
    /// [`HarnessError::AmbientMsbVar`] when an msb variable is set,
    /// [`HarnessError::MissingVar`] without a runtime directory,
    /// [`HarnessError::InvalidPrefix`] for a bad `PUDDLE_VM_PREFIX`,
    /// [`HarnessError::InvalidLogLevel`] for a bad `PUDDLE_VM_MSB_LOG_LEVEL`.
    #[expect(
        clippy::disallowed_methods,
        reason = "a fixed default root `pvm`; each run's folders inside it are named by the prefix, and the root variable overrides it"
    )]
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, HarnessError> {
        let get = |var: &str| lookup(var).filter(|value| !value.is_empty());
        refuse_ambient_msb_vars(get)?;
        let runtime_dir = get(RUNTIME_DIR_VAR).ok_or(HarnessError::MissingVar {
            var: RUNTIME_DIR_VAR,
            hint: "a directory with msb and libkrunfw from msb's release archive (ci/fetch-msb.sh)",
        })?;
        let prefix = RunPrefix::from_value_or_generate(get(PREFIX_VAR).as_deref())?;
        let root = get(ROOT_VAR).map_or_else(|| std::env::temp_dir().join("pvm"), PathBuf::from);
        let msb_log_level = get(MSB_LOG_LEVEL_VAR).map(|v| v.to_ascii_lowercase());
        if let Some(level) = &msb_log_level
            && !MSB_LOG_LEVELS.contains(&level.as_str())
        {
            return Err(HarnessError::InvalidLogLevel {
                value: level.chars().take(64).collect(),
            });
        }
        Ok(Self {
            runtime_dir: PathBuf::from(runtime_dir),
            prefix,
            root,
            msb_log_level,
        })
    }

    /// The private msb home of this run: `<root>/<prefix>`.
    #[must_use]
    pub fn home(&self) -> PathBuf {
        self.root.join(self.prefix.as_str())
    }

    /// A second, empty msb home beside the run's: `<root>/<prefix>-<tag>`. For a test that needs
    /// an image cache nothing else has filled. It stays a sibling (not below [`Self::home`]) and
    /// the tag should be a few characters, because msb derives Unix socket paths from the home
    /// and refuses 108 bytes or more.
    #[must_use]
    pub fn scratch_home(&self, tag: &str) -> PathBuf {
        self.root.join(format!("{}-{tag}", self.prefix.as_str()))
    }

    /// [`Self::scratch_home`] for the test `owner`, which must be the only one using `tag` in this
    /// run: a note file beside the folder (`<root>/<prefix>-<tag>.owner`) records the first
    /// owner, and a different one asking later fails here, at setup, instead of reading what the
    /// first left in the folder. The same owner may ask again (a rerun). Tests run one per
    /// process, so the note is how they see each other. Use [`test_owner`] for `owner`.
    ///
    /// # Errors
    ///
    /// [`HarnessError::ScratchTagTaken`] when another owner holds the tag,
    /// [`HarnessError::Io`] when the note or the root can't be written.
    pub fn claim_scratch_home(&self, tag: &str, owner: &str) -> Result<PathBuf, HarnessError> {
        use std::io::Write as _;

        self.create_root()?;
        let home = self.scratch_home(tag);
        let mut note = home.clone().into_os_string();
        note.push(".owner");
        let note = PathBuf::from(note);
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&note);
        match created {
            Ok(mut file) => file
                .write_all(owner.as_bytes())
                .map_err(|e| HarnessError::io("write", &note, e))?,
            // Taken: by whom? (Unreadable means it was not an existing note.)
            Err(e) => match std::fs::read_to_string(&note) {
                Ok(first) if first == owner => {}
                Ok(first) => {
                    return Err(HarnessError::ScratchTagTaken {
                        tag: tag.chars().take(64).collect(),
                        first,
                        second: owner.to_owned(),
                    });
                }
                Err(_) => return Err(HarnessError::io("create", &note, e)),
            },
        }
        Ok(home)
    }

    /// Where [`VmEnv::cleanup`] keeps the logs of the sandboxes it removes:
    /// `<root>/<prefix>-kept/<sandbox>/logs/`. A sibling of [`Self::home`], so a later passing
    /// test's [`VmEnv::remove_home`] doesn't delete a failed test's evidence.
    #[must_use]
    pub fn kept_logs(&self) -> PathBuf {
        self.root.join(format!("{}-kept", self.prefix.as_str()))
    }

    /// Creates the root owner-only. msb refuses a home with an ancestor other accounts can write
    /// to, unless an owner-only folder sits between them, and a umask of `0002` would otherwise
    /// leave a new root group-writable. An existing root is left as it is.
    fn create_root(&self) -> Result<(), HarnessError> {
        puddle_fs::private::create_dir(&self.root)
            .map_err(|e| HarnessError::io("create", &self.root, e))
    }

    /// Finds the runtime pair, creates the private home and writes its `config.json`.
    ///
    /// # Errors
    ///
    /// Whatever [`RuntimePair::find_in`] reports, or [`HarnessError::Io`].
    pub fn prepare(&self) -> Result<RuntimePair, HarnessError> {
        let pair = RuntimePair::find_in(&self.runtime_dir)?;
        self.create_root()?;
        let home = self.home();
        std::fs::create_dir_all(&home).map_err(|e| HarnessError::io("create", &home, e))?;
        let config = config_path(&home);
        std::fs::write(&config, pair.config_json(self.msb_log_level.as_deref()))
            .map_err(|e| HarnessError::io("write", &config, e))?;
        Ok(pair)
    }
}

/// The name of the running test, for [`Settings::claim_scratch_home`]: libtest runs each test on
/// a thread named after it (nextest runs one test per process, so this is its name). A test
/// run on the main thread (`--test-threads=1` in plain `cargo test`) can't be told apart from
/// another, so there the claim records `main` and catches nothing.
#[must_use]
pub fn test_owner() -> String {
    std::thread::current().name().unwrap_or("main").to_owned()
}

fn config_path(home: &Path) -> PathBuf {
    home.join("config.json")
}

/// A ready VM test environment: settings, runtime pair and an SDK backend on the private home.
pub struct VmEnv {
    settings: Settings,
    runtime: RuntimePair,
    backend: Arc<dyn Backend>,
    sandboxes_dir: PathBuf,
}

impl VmEnv {
    /// Sets up from the process environment (see the crate docs for the variables).
    ///
    /// # Errors
    ///
    /// Anything [`Settings::from_lookup`] or [`Settings::prepare`] reports, or
    /// [`HarnessError::Sdk`] when the backend can't open its database.
    pub async fn from_env() -> Result<Self, HarnessError> {
        Self::new(Settings::from_lookup(|var| std::env::var(var).ok())?).await
    }

    /// Sets up from explicit settings.
    ///
    /// # Errors
    ///
    /// As [`VmEnv::from_env`].
    pub async fn new(settings: Settings) -> Result<Self, HarnessError> {
        let runtime = settings.prepare()?;
        let home = settings.home();
        let local = LocalBackend::builder()
            .home(&home)
            .config_path(config_path(&home))
            .build()
            .await?;
        Ok(Self {
            settings,
            runtime,
            sandboxes_dir: local.sandboxes_dir(),
            backend: Arc::new(local),
        })
    }

    /// The settings this environment was built from.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The runtime pair in use.
    #[must_use]
    pub fn runtime(&self) -> &RuntimePair {
        &self.runtime
    }

    /// The prefixed sandbox name for `tag`.
    ///
    /// # Errors
    ///
    /// [`HarnessError::InvalidName`] when `<prefix>-<tag>` isn't a valid sandbox name.
    pub fn sandbox_name(&self, tag: &str) -> Result<SandboxName, HarnessError> {
        self.settings.prefix.sandbox_name(tag)
    }

    /// A builder with the harness defaults: prefixed name, `image`, 1 vCPU, 512 MiB, the run
    /// label, and `replace` (a leftover of the same name from a crashed attempt is replaced).
    ///
    /// # Errors
    ///
    /// [`HarnessError::InvalidName`] for a bad `tag`.
    pub fn sandbox(&self, tag: &str, image: &str) -> Result<SandboxBuilder, HarnessError> {
        let name = self.sandbox_name(tag)?;
        Ok(Sandbox::builder(name.as_str())
            .image(image)
            .cpus(1)
            .memory(SANDBOX_MEMORY_MIB)
            .label(RUN_LABEL, self.settings.prefix.as_str())
            .replace())
    }

    /// The folder of msb's `runtime.log` and `kernel.log` for the sandbox `name` of this run.
    #[must_use]
    pub fn log_dir(&self, name: &str) -> PathBuf {
        self.sandboxes_dir.join(name).join(MSB_LOGS_DIR)
    }

    /// The id of the process that runs the VM of the sandbox `name`, from msb's own record.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Sdk`] when msb has no such sandbox, [`HarnessError::NoVmProcess`] when its
    /// record holds no process id (the sandbox is not running).
    pub async fn process_id_of(&self, name: &str) -> Result<u32, HarnessError> {
        let handle = self.scope(Box::pin(Sandbox::get(name))).await?;
        pid_of_record(handle.local().and_then(|local| local.pid), name)
    }

    /// Runs `future` with this environment's backend as the SDK default (task-local, so other
    /// tests in the process are unaffected).
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        microsandbox::with_backend(Arc::clone(&self.backend), future).await
    }

    /// Stops and removes every sandbox of this run, including ones a failed test left behind.
    /// Returns the names it removed. Each sandbox's msb logs are copied to
    /// [`Settings::kept_logs`] first, so a boot that failed keeps its evidence for the CI
    /// artefact; [`VmEnv::remove_home`] deletes them after a passing test.
    ///
    /// # Errors
    ///
    /// The first [`HarnessError::Sdk`] error; it still tries every sandbox.
    pub async fn cleanup(&self) -> Result<Vec<String>, HarnessError> {
        // Boxed: the SDK's futures are large (clippy::large_futures).
        self.scope(Box::pin(async {
            let page = Sandbox::list().await?;
            let mut removed = Vec::new();
            let mut first_error = None;
            for handle in page.sandboxes {
                let name = handle.name().to_owned();
                if !self.settings.prefix.owns(&name) {
                    continue;
                }
                if let Ok(sandbox) = handle.connect().await
                    && sandbox.stop_with_timeout(STOP_TIMEOUT).await.is_err()
                    && let Err(e) = sandbox.kill().await
                {
                    first_error.get_or_insert(HarnessError::from(e));
                }
                keep_logs(
                    &self.sandboxes_dir.join(&name).join(MSB_LOGS_DIR),
                    &self.settings.kept_logs().join(&name).join(MSB_LOGS_DIR),
                );
                match Sandbox::remove(&name).await {
                    Ok(()) => removed.push(name),
                    Err(e) => {
                        first_error.get_or_insert(HarnessError::from(e));
                    }
                }
            }
            first_error.map_or(Ok(removed), Err)
        }))
        .await
    }

    /// Deletes the private home (image cache, database, logs). Call it only after a passing
    /// test, so a failure keeps msb's logs for the CI artefact.
    ///
    /// Takes the environment by value: the backend's database must be closed first, because
    /// Windows refuses to delete an open file ("being used by another process"). Files
    /// msb or SQLite release a moment later are retried for up to 5 s.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Io`] when the directory still can't be removed.
    pub async fn remove_home(self) -> Result<(), HarnessError> {
        let home = self.settings.home();
        let kept = self.settings.kept_logs();
        drop(self);
        remove_metrics_segments(&home);
        remove_dir_retrying(&home, REMOVE_ATTEMPTS, REMOVE_BACKOFF).await?;
        remove_dir_retrying(&kept, REMOVE_ATTEMPTS, REMOVE_BACKOFF).await
    }
}

/// The registry ABI versions whose segment msb may have created for a home (current and legacy).
const METRICS_ABIS: [u32; 2] = [2, 3];

/// The POSIX shared-memory object names (without the leading `/`) msb's metrics registry uses for
/// `home`: `msb-met-<FNV-1a 64 of the home path's bytes>-v<abi>`. msb creates one per home on the
/// first sandbox start and never unlinks it.
fn metrics_segment_names(home: &Path) -> Vec<String> {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in home.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    METRICS_ABIS
        .iter()
        .map(|abi| format!("msb-met-{hash:016x}-v{abi}"))
        .collect()
}

/// Unlinks the metrics segments msb left for the private `home`, so a test run leaves nothing in
/// `/dev/shm`. Best effort. Only Linux shows these as files; Windows frees the mapping with its
/// last handle, and macOS names live outside the file system.
///
/// A stopgap for msb never unlinking the segment: remove it once msb does (upstream report pending).
fn remove_metrics_segments(home: &Path) {
    if cfg!(target_os = "linux") {
        for name in metrics_segment_names(home) {
            let _ = std::fs::remove_file(Path::new("/dev/shm").join(name));
        }
    }
}

/// The directory under `sandboxes/<name>/` where msb writes `runtime.log` and `kernel.log`.
const MSB_LOGS_DIR: &str = "logs";

/// Copies the files of `from` into `to`, best effort: a sandbox that never got a log directory
/// has nothing to keep, and a copy that fails must not fail the cleanup.
fn keep_logs(from: &Path, to: &Path) {
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    if std::fs::create_dir_all(to).is_err() {
        return;
    }
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_file() {
            let _ = std::fs::copy(&path, to.join(entry.file_name()));
        }
    }
}

/// How often [`VmEnv::remove_home`] tries, and how long it waits between tries.
const REMOVE_ATTEMPTS: u32 = 20;
const REMOVE_BACKOFF: Duration = Duration::from_millis(250);

/// Removes `dir` and everything in it; a missing `dir` counts as removed.
async fn remove_dir_retrying(
    dir: &Path,
    attempts: u32,
    backoff: Duration,
) -> Result<(), HarnessError> {
    let mut tried = 1;
    // flake-ok: retries the removal with a backoff, and this crate may not depend on the dev-only test kit
    loop {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) if tried < attempts => {
                tried += 1;
                tokio::time::sleep(backoff).await;
            }
            Err(e) => return Err(HarnessError::io("remove", dir, e)),
        }
    }
}

/// Awaits `future` for at most `limit`.
///
/// # Errors
///
/// [`HarnessError::Timeout`] naming `what` when the budget runs out.
pub async fn within<T>(
    what: &'static str,
    limit: Duration,
    future: impl Future<Output = T>,
) -> Result<T, HarnessError> {
    tokio::time::timeout(limit, future)
        .await
        .map_err(|_| HarnessError::Timeout { what, limit })
}

/// The registry host an image reference pulls from: the part before the first `/` when it looks
/// like a host (a dot, a port or `localhost`), else Docker Hub.
#[must_use]
pub fn registry_of(image: &str) -> &str {
    match image.split_once('/') {
        Some((host, _)) if host.contains('.') || host.contains(':') || host == "localhost" => host,
        _ => "registry-1.docker.io",
    }
}

/// Awaits `creation` (a sandbox create from `image`) within `limit`. The image is pulled from its
/// public registry on purpose (a registry fake would not exercise the image the tests are about),
/// so a failure names that registry.
///
/// # Errors
///
/// [`HarnessError::ImagePull`] when the creation fails or the budget runs out.
pub async fn create_from_registry<T, E: std::fmt::Display>(
    image: &str,
    limit: Duration,
    creation: impl Future<Output = Result<T, E>>,
) -> Result<T, HarnessError> {
    let fail = |detail: String| HarnessError::ImagePull {
        image: image.to_owned(),
        registry: registry_of(image).to_owned(),
        detail,
    };
    match tokio::time::timeout(limit, creation).await {
        Ok(Ok(created)) => Ok(created),
        Ok(Err(e)) => Err(fail(e.to_string())),
        Err(_) => Err(fail(format!("no answer within {limit:?}"))),
    }
}

/// The process id msb recorded for a sandbox, as a `u32`; none means the sandbox is not running.
fn pid_of_record(recorded: Option<i32>, name: &str) -> Result<u32, HarnessError> {
    recorded
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| HarnessError::NoVmProcess {
            name: name.to_owned(),
        })
}

#[cfg(test)]
pub(crate) mod tests;
