// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows only: the real WinHTTP engine against a local PAC server, and the registry
//! path end to end. The registry tests edit the *current user's* Internet Settings, so they run
//! only where `GITHUB_ACTIONS` is set (a throwaway runner), never on a developer's machine. They
//! run one at a time, and so does every test that asks WinHTTP for a PAC answer (see `exclusive`).
#![cfg(windows)]
#![expect(
    clippy::unwrap_used,
    clippy::print_stderr,
    reason = "test helpers outside #[test] functions; a panic is how they fail"
)]
#![expect(
    unsafe_code,
    reason = "one registry write through the Win32 call, see `set_in_process`"
)]

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use puddle_upstream::{
    ChangeCallback, Config, Destination, Detected, Discovery, EnvFallback, EnvOs, Hop, ModeKind,
    OsProxy, PacError, PacQuery, ProblemCallback, ProxyAddr, ProxyConfig, RouteSource, Scheme,
    SettingsError, WatchGuard, WinOs,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PAC: &str = r#"function FindProxyForURL(url, host) {
  if (dnsDomainIs(host, ".corp.test")) return "DIRECT";
  if (shExpMatch(host, "github.*")) return "PROXY 127.0.0.1:1; PROXY 127.0.0.1:3128; DIRECT";
  if (url.substring(0, 5) == "http:") return "PROXY 127.0.0.1:8080";
  return "PROXY 127.0.0.1:3128";
}
"#;

/// Serves `PAC` on a loopback port until the runtime ends. Returns the PAC URL.
async fn pac_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/proxy.pac", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf).await; // the request line and headers are not needed
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PAC}",
                    PAC.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    url
}

fn query(pac_url: &str, url: &str) -> PacQuery {
    PacQuery {
        pac_url: Some(pac_url.into()),
        auto_detect: false,
        url: url.into(),
        timeout: Duration::from_secs(20),
    }
}

async fn resolve(q: PacQuery) -> Result<Vec<Hop>, PacError> {
    tokio::task::spawn_blocking(move || WinOs::new().resolve_pac(&q))
        .await
        .unwrap()
}

fn proxy(port: u16) -> Hop {
    Hop::Proxy(ProxyAddr::new("127.0.0.1", port))
}

#[tokio::test(flavor = "multi_thread")]
async fn winhttp_evaluates_a_local_pac_per_url_with_the_full_failover_list() {
    let _exclusive = exclusive();
    let pac = pac_server().await;
    assert_eq!(
        resolve(query(&pac, "https://github.com/")).await.unwrap(),
        vec![proxy(1), proxy(3128), Hop::Direct]
    );
    assert_eq!(
        resolve(query(&pac, "https://wiki.corp.test/"))
            .await
            .unwrap(),
        vec![Hop::Direct]
    );
    assert_eq!(
        resolve(query(&pac, "https://registry.npmjs.org/"))
            .await
            .unwrap(),
        vec![proxy(3128)]
    );
    assert_eq!(
        resolve(query(&pac, "http://deb.debian.org/"))
            .await
            .unwrap(),
        vec![proxy(8080)]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_pac_is_an_error_not_a_hang() {
    let _exclusive = exclusive();
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/proxy.pac", closed.local_addr().unwrap());
    drop(closed);
    let started = Instant::now();
    let err = resolve(query(&url, "https://github.com/"))
        .await
        .unwrap_err();
    eprintln!("unreachable PAC gave: {err}");
    assert!(matches!(
        err,
        PacError::Unavailable(_) | PacError::Timeout | PacError::Failed(_)
    ));
    // flake-ok: the query's own timeout is 20 s, so 25 s fails only a resolve that ignored it; a
    // closed port still took 2.04 to 2.06 s (3 runs on an idle Windows machine), a 12x margin. Windows only.
    assert!(started.elapsed() < Duration::from_secs(25));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pac_server_that_never_answers_times_out_at_the_query_timeout() {
    let _exclusive = exclusive();
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/proxy.pac", silent.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = silent.accept().await {
            held.push(stream);
        }
    });
    let mut q = query(&url, "https://github.com/");
    q.timeout = Duration::from_secs(2);
    let started = Instant::now();
    let err = resolve(q).await.unwrap_err();
    assert!(
        matches!(err, PacError::Timeout | PacError::Unavailable(_)),
        "{err}"
    );
    // flake-ok: the query timeout here is 2 s and the server never answers, so a resolve that
    // ignored it waits for WinHTTP's own (far longer) default; 20 s is 10x the limit, and it took
    // 2.01 to 2.02 s (3 runs on an idle Windows machine). Windows only.
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_pac_lookups_all_answer() {
    let _exclusive = exclusive();
    let pac = pac_server().await;
    let mut tasks = Vec::new();
    for i in 0..24 {
        let pac = pac.clone();
        tasks.push(tokio::spawn(async move {
            resolve(query(&pac, &format!("https://host{i}.example/"))).await
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap().unwrap(), vec![proxy(3128)]);
    }
}

#[test]
fn the_settings_call_works_on_any_machine_and_never_panics() {
    let settings = WinOs::new().config().unwrap();
    eprintln!(
        "auto_detect={} pac={} static={}",
        settings.auto_detect,
        settings.pac_url.is_some(),
        !settings.rules.is_empty()
    );
}

// --- registry tests: throwaway runners only ---------------------------------------------------

const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";

/// The tests share one registry key: one at a time.
static REGISTRY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The cross-process lock behind every test that edits the Internet Settings key or asks WinHTTP
/// for a PAC answer. nextest runs each test in its own process, in parallel, and these tests
/// disturb each other: one's values (or its clean-up deleting them) show up in another's reads,
/// every evaluated PAC script, in any process, makes WinHTTP call the proxy-change callback of
/// every registered watch (about every 50 ms for as long as lookups run, with no registry change
/// at all), which ends the epoch the debounce test is counting, and PAC lookups of two processes
/// at once have given wrong results (one that must fail answering, ones that must answer
/// failing). A file lock holds across processes and across threads alike. Held until the
/// returned file is dropped.
#[expect(
    clippy::disallowed_methods,
    reason = "the lock is shared by every test process, so it must sit at one fixed path"
)]
fn exclusive() -> std::fs::File {
    let path = std::env::temp_dir().join("puddle-test-hkcu-internet-settings.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .unwrap();
    lock.lock().unwrap();
    lock
}

/// The gate for every test that edits the Internet Settings key: `None` (skip) off a CI runner,
/// otherwise the exclusive lock, held until the test returns.
fn registry_test() -> Option<std::fs::File> {
    if std::env::var_os("GITHUB_ACTIONS").is_none() {
        // The tests rewrite the user's own Internet Settings, so they run only on a CI runner; the
        // Windows job sets PUDDLE_REQUIRE_REGISTRY_TESTS to fail if it ever stops qualifying.
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_REGISTRY_TESTS").is_none(),
            "PUDDLE_REQUIRE_REGISTRY_TESTS is set but this is not a CI runner (GITHUB_ACTIONS)"
        );
        eprintln!(
            "skipped: edits HKCU Internet Settings, only run on a CI runner (GITHUB_ACTIONS)"
        );
        return None;
    }
    Some(exclusive())
}

fn reg(args: &[&str]) -> bool {
    Command::new("reg")
        .args(args)
        .output()
        .unwrap()
        .status
        .success()
}

/// Puts one value in the Internet Settings key and puts the old state back on drop.
struct RegValue {
    name: &'static str,
    existed: bool,
}

impl RegValue {
    fn set(name: &'static str, kind: &str, data: &str) -> Self {
        let existed = reg(&["query", KEY, "/v", name]);
        assert!(
            reg(&["add", KEY, "/v", name, "/t", kind, "/d", data, "/f"]),
            "reg add {name}"
        );
        Self { name, existed }
    }
}

impl Drop for RegValue {
    fn drop(&mut self) {
        // A value that existed before is left as the test set it: runners are throwaway, and
        // restoring would need its old data and type.
        if !self.existed {
            let _ = reg(&["delete", KEY, "/v", self.name, "/f"]);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_and_discovery_follow_the_registry_end_to_end() {
    let Some(_registry) = registry_test() else {
        return;
    };
    let _registry = REGISTRY.lock().await;
    let pac = pac_server().await;
    let _url = RegValue::set("AutoConfigURL", "REG_SZ", &pac);
    let settings = WinOs::new().config().unwrap();
    assert_eq!(settings.pac_url.as_deref(), Some(pac.as_str()));

    let discovery = Discovery::new(
        Arc::new(EnvFallback::new(Arc::new(WinOs::new()), EnvOs::default())),
        Config::default(),
    );
    let decision = discovery
        .route(&Destination::new(Scheme::Https, "github.com", 443))
        .await;
    assert_eq!(decision.source, RouteSource::Pac);
    assert_eq!(
        decision.route.to_string(),
        "PROXY 127.0.0.1:1; PROXY 127.0.0.1:3128; DIRECT"
    );
    let direct = discovery
        .route(&Destination::new(Scheme::Https, "wiki.corp.test", 443))
        .await;
    assert!(direct.route.is_direct());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_health_report_names_what_windows_holds_and_hides_what_is_secret() {
    let Some(_registry) = registry_test() else {
        return;
    };
    let _registry = REGISTRY.lock().await;
    let discovery = || {
        Discovery::new(
            Arc::new(EnvFallback::new(Arc::new(WinOs::new()), EnvOs::default())),
            Config::default(),
        )
    };
    let pac = pac_server().await;
    let with_secrets = pac.replacen("http://", "http://svc:hunter2@", 1) + "?key=topsecret";
    let _url = RegValue::set("AutoConfigURL", "REG_SZ", &with_secrets);
    let health = discovery().health().await;
    assert_eq!(health.detected, Detected::Pac);
    assert_eq!(health.pac_url.as_deref(), Some(pac.as_str()));
    assert_eq!(health.mode, ModeKind::System);
    assert_eq!(health.settings_error, None);
    let shown = format!("{health:?}");
    assert!(
        !shown.contains("hunter2") && !shown.contains("topsecret"),
        "{shown}"
    );

    // The PAC address gone, a static proxy with a bypass list is what Windows holds.
    assert!(reg(&["delete", KEY, "/v", "AutoConfigURL", "/f"]));
    let _server = RegValue::set("ProxyServer", "REG_SZ", "static.corp:3128");
    let _bypass = RegValue::set("ProxyOverride", "REG_SZ", "*.corp.test;<local>");
    let _on = RegValue::set("ProxyEnable", "REG_DWORD", "1");
    let health = discovery().health().await;
    // Windows may have "detect settings automatically" on (a runner does): WPAD is then asked
    // first and the static proxy is what it falls back to. Both are reported.
    let expected = if health.auto_detect {
        Detected::Wpad
    } else {
        Detected::Static
    };
    assert_eq!(health.detected, expected);
    assert_eq!(
        health.https_proxy,
        Some(ProxyAddr::new("static.corp", 3128))
    );
    assert_eq!(health.bypass_entries, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_proxy_enable_switch_hides_a_static_proxy() {
    let Some(_registry) = registry_test() else {
        return;
    };
    let _registry = REGISTRY.lock().await;
    let _server = RegValue::set("ProxyServer", "REG_SZ", "static.corp:3128");
    let _bypass = RegValue::set("ProxyOverride", "REG_SZ", "*.corp.test;<local>");
    let _on = RegValue::set("ProxyEnable", "REG_DWORD", "1");
    let on = WinOs::new().config().unwrap();
    assert_eq!(
        on.rules.for_scheme(Scheme::Https),
        Some(&ProxyAddr::new("static.corp", 3128))
    );
    assert!(
        on.bypass
            .matches(&Destination::new(Scheme::Https, "wiki.corp.test", 443))
    );
    assert!(
        on.bypass
            .matches(&Destination::new(Scheme::Https, "intranet", 443))
    );
    assert!(reg(&[
        "add",
        KEY,
        "/v",
        "ProxyEnable",
        "/t",
        "REG_DWORD",
        "/d",
        "0",
        "/f"
    ]));
    let off = WinOs::new().config().unwrap();
    assert!(off.rules.is_empty(), "ProxyEnable=0 means no static proxy");
}

/// Writes a string value with one Win32 call: microseconds, where `reg add` is a process start
/// that a loaded runner can stall for seconds. Debounce tests need edits that are quick *by
/// construction*, not by luck of the scheduler.
fn set_in_process(name: &str, data: &str) {
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegOpenKeyExW, RegSetValueExW,
    };
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() };
    let subkey = wide(KEY.strip_prefix(r"HKCU\").unwrap());
    let (name, data) = (wide(name), wide(data));
    let mut key = std::ptr::null_mut();
    // SAFETY: `subkey`, `name` and `data` are NUL-terminated UTF-16 buffers that outlive the
    // calls, `bytes` is `data`'s length in bytes, and `key` is closed once after the write.
    unsafe {
        assert_eq!(
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                KEY_SET_VALUE,
                &raw mut key
            ),
            0
        );
        let bytes = u32::try_from(data.len() * 2).unwrap();
        let set = RegSetValueExW(key, name.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), bytes);
        RegCloseKey(key);
        assert_eq!(set, 0, "RegSetValueExW");
    }
}

/// The OS layer with a note of when each change notification reached discovery, so a test can
/// tell which epoch changes a notification of its own explains.
#[derive(Debug)]
struct Recording {
    inner: Arc<dyn OsProxy>,
    calls: Arc<Mutex<Vec<Instant>>>,
}

impl OsProxy for Recording {
    fn config(&self) -> Result<ProxyConfig, SettingsError> {
        self.inner.config()
    }

    fn resolve_pac(&self, query: &PacQuery) -> Result<Vec<Hop>, PacError> {
        self.inner.resolve_pac(query)
    }

    fn watch(
        &self,
        on_change: ChangeCallback,
        on_problem: ProblemCallback,
    ) -> Option<Box<dyn WatchGuard>> {
        let calls = Arc::clone(&self.calls);
        let noted: ChangeCallback = Arc::new(move || {
            calls.lock().unwrap().push(Instant::now());
            on_change();
        });
        self.inner.watch(noted, on_problem)
    }
}

/// Five edits in a burst give one epoch, `debounce` after the last one. The burst is written
/// in-process, so it is over in a few milliseconds whatever the runner's load; the test checks
/// that rather than assuming it, so a stalled runner reads as a stall and not as a debounce bug.
///
/// WinHTTP also calls the proxy-change callback for things that are not the test's edits (it
/// does so for every PAC lookup on the machine, and the system rewrites the `Connections` value
/// a moment after a lookup), and those can come after the burst's epoch. So the test does not
/// count epochs; it checks the contract per epoch against the notifications discovery was told
/// about: an epoch comes only when a notification is new since the last epoch, and never less
/// than `debounce` after the latest one. An epoch the notifications do not explain, or one that
/// cuts the quiet period short, is a debounce bug whatever else the machine does.
#[tokio::test(flavor = "multi_thread")]
async fn a_registry_change_ends_the_epoch_once_after_the_debounce() {
    let Some(_registry) = registry_test() else {
        return;
    };
    let _registry = REGISTRY.lock().await;
    let debounce = Duration::from_secs(1);
    let config = Config {
        debounce,
        ..Config::default()
    };
    let calls = Arc::new(Mutex::new(Vec::new()));
    let os = Recording {
        inner: Arc::new(EnvFallback::new(Arc::new(WinOs::new()), EnvOs::default())),
        calls: Arc::clone(&calls),
    };
    let discovery = Discovery::new(Arc::new(os), config);
    // Sets the value (and puts the old state back on drop) before the watch starts.
    let _value = RegValue::set("ProxyServer", "REG_SZ", "before.corp:3128");
    let mut epochs = discovery.subscribe();
    let watching = discovery
        .watch()
        .expect("the registry watch is available on every Windows");
    let started = Instant::now();
    for i in 0..5 {
        set_in_process("ProxyServer", &format!("flap{i}.corp:3128"));
    }
    let burst = started.elapsed();
    assert!(
        burst < debounce / 2,
        "the runner stalled for {burst:?} inside five registry writes; the debounce is {debounce:?}"
    );
    // The first epoch (30 s at most, not a delay), then whatever else comes within two debounce
    // periods of quiet.
    let mut ended = Vec::new();
    let mut wait = Duration::from_secs(30);
    while let Ok(changed) = tokio::time::timeout(wait, epochs.changed()).await {
        changed.unwrap();
        ended.push(Instant::now());
        wait = debounce * 2;
        assert!(
            ended.len() < 20,
            "the epoch keeps changing: {} changes",
            ended.len()
        );
    }
    assert!(!ended.is_empty(), "no epoch ended after the burst");
    let calls = calls.lock().unwrap().clone();
    let mut previous = started; // notifications at or before this one explained earlier epochs
    for (n, epoch) in ended.iter().enumerate() {
        // A notification that lands within a few milliseconds of an epoch may be counted by
        // either side of it.
        let slack = Duration::from_millis(100);
        let latest = calls
            .iter()
            .filter(|call| **call + slack > previous && *call <= epoch)
            .max()
            .unwrap_or_else(|| {
                panic!(
                    "epoch change {n} came with no new notification since the last one: \
                     the debounce produced an epoch of its own"
                )
            });
        assert!(
            *epoch - *latest >= debounce,
            "epoch change {n} came {:?} after the latest notification; the debounce is {debounce:?}",
            *epoch - *latest
        );
        previous = *epoch;
    }
    drop(watching);
}
