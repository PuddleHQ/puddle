// SPDX-License-Identifier: GPL-3.0-or-later
//! Packages a test brings into a guest (`nft`, `pip`) without a live package mirror on a warm cache.
//!
//! A [`PackageKit`] is the set of package files (`.deb` or `.apk`, dependencies included) that
//! one install needs. The first run, with no kit in the host's cache, installs from the real
//! mirror in the guest, names that mirror in the failure message, and saves the files; every later
//! run serves them to the guest from the fake website (`kit.invalid`, behind the company proxy) and
//! installs with no mirror. A kit that does not install (the image moved under it) falls back to
//! the mirror, so a stale cache costs a download, never a red test.
//!
//! The cache folder is `PUDDLE_VM_TOOLS_CACHE` (CI restores it between runs) or
//! `~/.cache/puddle-vm-tools`, under `capture-kits/`; with neither, every run is cold.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use puddle_compute::{ExecRequest, Runtime, Sandbox};
use puddle_e2e::lab::{Responder, WebAnswer, WebOptions};
use puddle_types::WorkspaceId;

use super::VmHost;

/// The fake website's name for kits; a workspace that installs from one allows it.
pub(crate) const KIT_HOST: &str = "kit.invalid";

/// Bumped when the way kits are built or installed changes, so old cache files are not reused.
const KIT_VERSION: u32 = 1;

/// The package manager of the guest image.
#[derive(Clone, Copy)]
pub(crate) enum Flavour {
    /// Debian's `apt` (the default workspace image).
    Apt,
    /// Alpine's `apk` (`docker:dind`).
    Apk,
}

impl Flavour {
    /// Guest script: downloads the package files of `packages` and everything they need and prints
    /// them as one base64 tar (nothing else goes to stdout).
    fn build(self, packages: &str) -> String {
        match self {
            Self::Apt => format!(
                "export DEBIAN_FRONTEND=noninteractive
                 rm -rf /root/kit && mkdir -p /root/kit
                 apt-get update -qq && apt-get install -y -qq --download-only --no-install-recommends {packages} >/root/kit-build.log 2>&1 \
                   || {{ echo 'kit build failed:'; tail -15 /root/kit-build.log; exit 1; }} >&2
                 cp /var/cache/apt/archives/*.deb /root/kit/ && cd /root/kit && tar -cf - . | base64 -w0"
            ),
            Self::Apk => format!(
                "rm -rf /root/kit && mkdir -p /root/kit
                 apk --no-cache fetch -R -o /root/kit {packages} >/root/kit-build.log 2>&1 \
                   || {{ echo 'kit build failed:'; tail -15 /root/kit-build.log; exit 1; }} >&2
                 cd /root/kit && tar -cf - . | base64 -w0"
            ),
        }
    }

    /// Guest script: installs what `/root/kit` holds, with no network.
    fn install(self) -> &'static str {
        match self {
            Self::Apt => {
                "export DEBIAN_FRONTEND=noninteractive
                 apt-get install -y -qq --no-install-recommends --no-download /root/kit/*.deb >/root/kit-install.log 2>&1 || { tail -15 /root/kit-install.log; exit 1; }"
            }
            Self::Apk => {
                "apk add --allow-untrusted --no-network /root/kit/*.apk >/root/kit-install.log 2>&1 || { tail -15 /root/kit-install.log; exit 1; }"
            }
        }
    }
}

/// One install a test needs, from the cache or from the mirror.
pub(crate) struct PackageKit {
    flavour: Flavour,
    packages: &'static str,
    mirror: &'static str,
    file: Option<PathBuf>,
    /// The kit's tar: read from the cache at [`Self::open`], or filled by the cold run; what the
    /// fake website serves.
    bytes: Arc<Mutex<Option<Arc<Vec<u8>>>>>,
}

impl PackageKit {
    /// The kit for `packages` on `image` (named only to key the cache); `mirror` names the live
    /// third party a cold run needs, for the failure message.
    pub(crate) fn open(
        flavour: Flavour,
        image: &str,
        packages: &'static str,
        mirror: &'static str,
    ) -> Self {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        (KIT_VERSION, image, packages, flavour.build(packages)).hash(&mut hasher);
        let file = super::cache_root().map(|root| {
            root.join("capture-kits")
                .join(format!("kit-{:016x}.tar", hasher.finish()))
        });
        let cached = file
            .as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .filter(|bytes| !bytes.is_empty())
            .map(Arc::new);
        Self {
            flavour,
            packages,
            mirror,
            file,
            bytes: Arc::new(Mutex::new(cached)),
        }
    }

    /// `base` with the fake website also serving this kit at `http://kit.invalid/kit.tar` (and
    /// whatever `base` already answers).
    pub(crate) fn options(&self, base: WebOptions) -> WebOptions {
        let slot = self.bytes.clone();
        let inner: Option<Responder> = base.responder.clone();
        let responder: Responder = Arc::new(move |seen| {
            let on_kit = seen
                .headers_named("host")
                .first()
                .is_some_and(|h| h.split(':').next() == Some(KIT_HOST));
            if on_kit && seen.target.split('?').next() == Some("/kit.tar") {
                let bytes = slot.lock().unwrap().clone()?;
                return Some(WebAnswer {
                    status: 200,
                    content_type: "application/x-tar",
                    body: bytes.to_vec(),
                });
            }
            inner.as_ref().and_then(|ask| ask(seen))
        });
        WebOptions {
            responder: Some(responder),
            ..base
        }
    }

    /// Installs the packages in `workspace` and runs `check` (a guest command that fails when they
    /// are not usable); returns what `check` printed. The workspace must allow [`KIT_HOST`] and, for
    /// a cold run, the mirror.
    pub(crate) async fn install(
        &self,
        vm: &VmHost,
        workspace: &WorkspaceId,
        check: &str,
    ) -> String {
        let note = format!(
            " (live third party: {}; an outage there fails this test)",
            self.mirror
        );
        if self.bytes.lock().unwrap().is_some() {
            let (code, out, err) = self.unpack_and_install(vm, workspace).await;
            let (checked, said, _) = exec(vm, workspace, check, Duration::from_secs(60)).await;
            if code == 0 && checked == 0 {
                return said;
            }
            eprintln!(
                "the cached kit did not install (exit {code}, check {checked}); using the mirror: {out}{err}{said}"
            );
        }
        // Cold, or the cached kit did not fit this image: download from the mirror, keep the files
        // for the next run and install them the way a warm run does.
        let (code, packed, err) = exec(
            vm,
            workspace,
            &self.flavour.build(self.packages),
            Duration::from_secs(300),
        )
        .await;
        assert_eq!(
            code, 0,
            "the packages could not be fetched{note}: {packed}{err}"
        );
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(packed.trim())
            .unwrap_or_else(|e| panic!("the kit came out of the guest unreadable: {e}"));
        if let Some(file) = &self.file {
            save(file, &bytes);
        }
        *self.bytes.lock().unwrap() = Some(Arc::new(bytes));
        let (code, out, err) = self.unpack_and_install(vm, workspace).await;
        assert_eq!(code, 0, "the packages did not install{note}: {out}{err}");
        let (code, said, err) = exec(vm, workspace, check, Duration::from_secs(60)).await;
        assert_eq!(code, 0, "the packages are not usable{note}: {said}{err}");
        said
    }

    async fn unpack_and_install(
        &self,
        vm: &VmHost,
        workspace: &WorkspaceId,
    ) -> (i32, String, String) {
        let script = format!(
            "rm -rf /root/kit && mkdir -p /root/kit && cd /root/kit
             {{ curl -fsS --max-time 120 -o kit.tar http://{KIT_HOST}/kit.tar 2>/dev/null \
                || wget -qO kit.tar http://{KIT_HOST}/kit.tar; }} || {{ echo 'kit download failed'; exit 1; }}
             tar -xf kit.tar && rm kit.tar
             {}",
            self.flavour.install()
        );
        exec(vm, workspace, &script, Duration::from_secs(180)).await
    }
}

/// Writes `bytes` to `file` by a rename, so a run that reads it never sees half a kit.
fn save(file: &std::path::Path, bytes: &[u8]) {
    let Some(dir) = file.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let part = file.with_extension(format!("part{}", std::process::id()));
    if std::fs::write(&part, bytes).is_ok() && std::fs::rename(&part, file).is_err() {
        let _ = std::fs::remove_file(&part);
    }
}

/// Runs `script` as root in the guest (with the workspace's proxy settings): exit code, stdout,
/// stderr. A script that does not finish in `timeout` fails the test.
async fn exec(
    vm: &VmHost,
    workspace: &WorkspaceId,
    script: &str,
    timeout: Duration,
) -> (i32, String, String) {
    let handle = vm.runtime.get(&workspace.sandbox_name()).await.unwrap();
    let out = handle
        .exec(
            ExecRequest::sh(format!(
                "cd && exec sh -l -c '{}'",
                script.replace('\'', "'\\''")
            ))
            .as_user("root")
            .with_timeout(timeout),
        )
        .await
        .unwrap_or_else(|e| panic!("the guest script did not finish: {e}\n{script}"));
    (
        out.status.code,
        out.stdout_text().to_string(),
        out.stderr_text().to_string(),
    )
}
