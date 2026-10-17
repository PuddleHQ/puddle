// SPDX-License-Identifier: GPL-3.0-or-later
//! Git over HTTPS end to end on real microVMs (K on Linux KVM, W on Windows WHP), through the
//! host's own wiring: the real Git injector over the host's store and secrets, the per-workspace
//! CA, the live decrypt set and the egress proxy, to a fake Git host (the real `git http-backend`
//! behind TLS, `puddle_e2e::lab::GitHost`) that the guest can only reach through a company proxy
//! that alone knows its name.
//!
//! What is proved here that the proxy-level tests cannot: the guest's own git, curl and TLS
//! stack against the real boot (CA in the trust store, proxy variables, author rules), a
//! credential that really reads (a stored token, no stub injector), no secret anywhere in the
//! guest, the CA's shape, two workspaces that do not share what one decrypts, and a guest whose
//! clock lies.
#![expect(
    clippy::unwrap_used,
    reason = "VM test: a failed step fails the test; measurements go to stderr"
)]

mod support;
mod vm_support;

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use puddle_certs::{CorporateRoots, SOURCES, StoreSnapshot};
use puddle_e2e::lab::{GitHost, GitHostOptions, NameProxy, Versions, clean_git};
use puddle_host::HostOptions;
use puddle_secrets::{HostName, Secret, SecretStore, SourceSpec, StoredId, TokenScope};
use puddle_store::{
    Actor, Author, Coverage, CredentialBinding, Effect, IdentityDraft, NewRule, Pattern, Scope,
};
use puddle_types::WorkspaceName;
use puddle_upstream::{Discovery, FakeOs, ProxyConfig, ProxyRules};
use serde_json::json;
use support::{Api, ended};
use vm_support::{HarnessPlatform, VmHost, guest, guest_within, start_vm_host_platform};

const NAME: &str = "bound.test";
const WORK: &str = "KCANARY-WORK-7d41c9e2b0a8";
const PERSONAL: &str = "KCANARY-PERSONAL-3f9a60c1d7e5";

/// The fake internet: the hosts, the company proxy that knows their names, and what the host
/// reports and uses to reach them.
struct Internet {
    proxy: NameProxy,
    options: HostOptions,
    platform: HarnessPlatform,
}

fn internet(hosts: &[(&str, &GitHost)]) -> Internet {
    let rt = tokio::runtime::Handle::current();
    let names: Vec<(&str, std::net::SocketAddr)> =
        hosts.iter().map(|(n, h)| (*n, h.addr())).collect();
    let proxy = tokio::task::block_in_place(|| rt.block_on(NameProxy::start(&names)));
    let os = FakeOs::new(ProxyConfig {
        rules: ProxyRules::parse(&format!("http://127.0.0.1:{}", proxy.port())),
        ..ProxyConfig::default()
    });
    let mut options = support::hermetic_options();
    options.discovery = Some(Discovery::new(os, puddle_upstream::Config::default()));
    let mut snapshot = StoreSnapshot::new();
    for (_, host) in hosts {
        snapshot.add(SOURCES[0], host.root().as_ref().to_vec());
    }
    let platform = HarnessPlatform {
        roots: CorporateRoots::select(&snapshot, SystemTime::now()),
    };
    let roots: BTreeSet<Vec<u8>> = hosts
        .iter()
        .map(|(_, h)| h.root().as_ref().to_vec())
        .collect();
    assert_eq!(
        platform.roots.certificates().len(),
        roots.len(),
        "the fake hosts' roots are reported as corporate roots"
    );
    Internet {
        proxy,
        options,
        platform,
    }
}

fn stored(host: &str, token: &str, vm: &VmHost, id: &str) -> CredentialBinding {
    let host = HostName::new(host).unwrap();
    let id = StoredId::new(id).unwrap();
    vm.secrets.set(&id, &Secret::new(token.to_owned())).unwrap();
    CredentialBinding::new(
        &host,
        SourceSpec::Stored {
            id,
            scope: TokenScope {
                host: host.clone(),
                org: None,
            },
        },
        Coverage::new(BTreeSet::new(), true).unwrap(),
    )
    .unwrap()
}

fn owned_stored(host: &str, owner: &str, token: &str, vm: &VmHost, id: &str) -> CredentialBinding {
    let binding = stored(host, token, vm, id);
    let host = HostName::new(host).unwrap();
    CredentialBinding::new(
        &host,
        binding.source.clone(),
        Coverage::new(
            BTreeSet::from([puddle_store::Owner::new(owner).unwrap()]),
            false,
        )
        .unwrap(),
    )
    .unwrap()
}

fn identity(label: &str, credentials: Vec<CredentialBinding>) -> IdentityDraft {
    IdentityDraft {
        label: label.to_owned(),
        author: Author::new(label, &format!("{}@example.org", label.to_lowercase())).unwrap(),
        credentials,
    }
}

fn allow(vm: &VmHost, sandbox: &WorkspaceName, hosts: &[&str]) {
    for host in hosts {
        vm.host
            .store()
            .add_rule(&NewRule {
                scope: Scope::Workspace(sandbox.clone()),
                pattern: Pattern::parse(host).unwrap(),
                effect: Effect::Allow,
                expires_at: None,
                created_by: Actor::Cli,
            })
            .unwrap();
    }
}

/// Creates the workspace `name` from `repo_url` and waits until it is up.
async fn create(vm: &VmHost, name: &str, repo_url: &str) {
    let api = Api::new(vm.host.url(), vm.host.token());
    let mut events = api.events().await;
    let name_static: &'static str = Box::leak(name.to_owned().into_boxed_str());
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "repo_url": repo_url, "memory_mib": 1024}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end.get("step"), Some(&json!("done")), "{end}");
}

/// A bare repository's branch, read on the fake host.
fn head_of(host: &GitHost, repo: &str, branch: &str) -> String {
    let mut command = clean_git(host.scratch());
    command
        .arg("--git-dir")
        .arg(host.repo_path(repo))
        .args(["rev-parse", &format!("refs/heads/{branch}")]);
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn basic(token: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"))
    )
}

/// `"first""second"`: the shell joins them, the script text never holds the value.
fn split(value: &str) -> String {
    let (head, tail) = value.split_at(value.len() / 2);
    format!("\"{head}\"\"{tail}\"")
}

/// The fake host with the repositories every test uses.
async fn git_host(name: &str) -> GitHost {
    let host = GitHost::start(name, Versions::Both).await;
    seed(&host);
    host
}

fn seed(host: &GitHost) {
    host.accept_token(WORK);
    host.accept_token(PERSONAL);
    host.add_repo("acme/web", true);
    host.add_repo("acme/unlisted", true);
    host.add_repo("acme/open", false);
    host.add_repo("someone/else", true);
}

/// Git through the real wiring, in the guest's own git: a workspace is created by cloning a
/// private repository with a credential that is read from the host's secret store (the default
/// injector, no stub), two identities share one host and each owner's repositories get their
/// own, a push to a listed repository lands, a push to an unlisted one is refused in git's own
/// words, a path that names a repository two ways is a `400` and never goes upstream, and the
/// guest holds no token: not in any process's environment or command line, not in any file of
/// the disk, not in git's trace.
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_git_clones_and_pushes_with_two_identities_and_the_guest_never_holds_a_token() {
    let fake = git_host(NAME).await;
    let net = internet(&[(NAME, &fake)]);
    let vm = start_vm_host_platform("gitk", net.options, &net.platform, |_| {}).await;
    let name = format!("{}-gitk", vm.settings.prefix.as_str());
    let sandbox = WorkspaceName::new(&name).unwrap();
    allow(&vm, &sandbox, &[NAME]);
    // Work covers the `acme` organisation; Personal covers the rest of the host. The tokens are
    // in the host's secret store and the sources are read at the moment of use.
    let work = vm
        .host
        .store()
        .create_identity(identity(
            "Work",
            vec![owned_stored(NAME, "acme", WORK, &vm, "kwork")],
        ))
        .unwrap();
    let personal = vm
        .host
        .store()
        .create_identity(identity(
            "Personal",
            vec![stored(NAME, PERSONAL, &vm, "kpersonal")],
        ))
        .unwrap();
    let _ = (work.id, personal.id);

    // Creating the workspace clones a private repository: the first request on the host already
    // carries the credential, from a source that really reads.
    create(&vm, &name, &format!("https://{NAME}/acme/web.git")).await;
    // Personal joins the running workspace; the workspace's own repository is already in the
    // table with pull and push on (the create put it there).
    vm.host
        .store()
        .attach_identity(&sandbox, personal.id, None)
        .unwrap();
    let seen = fake.seen();
    assert!(
        !seen.is_empty(),
        "the workspace's own clone reached the fake host"
    );
    for request in &seen {
        assert_eq!(
            request.headers_named("authorization"),
            [basic(WORK)],
            "{} {}: the clone carries Work's token, added by the proxy",
            request.method,
            request.target
        );
    }
    let first = seen.len();

    // The guest's git: clone with a trace (to a file in the guest, searched below), then push.
    let script = "set -eu; rm -rf /tmp/k; mkdir /tmp/k; cd /tmp/k; \
         GIT_TRACE_CURL=1 GIT_TRACE=1 git clone -q https://bound.test/acme/web.git web 2>/tmp/clone-trace.txt; \
         cd web; echo hi > pushed.txt; git add pushed.txt; git commit -q -m guest; \
         git push -q origin HEAD:main; git log -1 --format=%ae; git rev-parse HEAD";
    let out = guest(&vm.runtime, &sandbox, script).await;
    let mut lines = out.lines();
    let author = lines.next().unwrap();
    let pushed = lines.next().unwrap();
    assert_eq!(
        author, "work@example.org",
        "the author follows the owner: {out}"
    );
    assert_eq!(
        head_of(&fake, "acme/web", "main"),
        pushed,
        "the push reached the host's repository"
    );
    // The other owner gets the other identity, from the same host: its credential for the
    // clone, its author for the commit (the author rules follow the attach in the running guest).
    let mut author = String::new();
    for _ in 0..60 {
        author = guest(
            &vm.runtime,
            &sandbox,
            "set -eu; cd /tmp/k; rm -rf else; git clone -q https://bound.test/someone/else.git else; \
             git -C else config user.email",
        )
        .await;
        if author.trim() == "personal@example.org" {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(author.trim(), "personal@example.org", "{author}");
    let seen = fake.seen();
    for request in seen.iter().skip(first) {
        let wanted = if request.target.starts_with("/someone/") {
            basic(PERSONAL)
        } else {
            basic(WORK)
        };
        assert_eq!(
            request.headers_named("authorization"),
            [wanted],
            "{}",
            request.target
        );
    }
    assert!(
        seen.iter().any(|r| r.target.starts_with("/someone/")),
        "the second identity's repository was read"
    );
    assert!(
        seen.iter().any(|r| r.version == "HTTP/2"),
        "the upstream leg speaks HTTP/2 to a host that offers it: {:?}",
        seen.iter().map(|r| r.version).collect::<Vec<_>>()
    );

    // A push to a repository that is not listed: refused in git's own output, not sent.
    let before = fake.seen().len();
    let out = guest(
        &vm.runtime,
        &sandbox,
        "cd /tmp/k; git clone -q https://bound.test/acme/unlisted.git unlisted; cd unlisted; \
         echo x > x.txt; git add x.txt; git commit -q -m x; \
         git push origin HEAD:main 2>&1 || true",
    )
    .await;
    assert!(out.contains("not on this workspace's push list"), "{out}");
    assert!(
        fake.seen()
            .iter()
            .skip(before)
            .all(|r| !r.target.contains("receive-pack")),
        "no push request reached the host"
    );

    // HG-18: a path that names a repository two ways is a 400, and the host never hears of it.
    let before = fake.seen().len();
    let codes = guest(
        &vm.runtime,
        &sandbox,
        "for p in \
           '/acme/web.git/../unlisted.git/info/refs?service=git-receive-pack' \
           '/acme/./unlisted.git/info/refs?service=git-receive-pack' \
           '/acme%2Funlisted.git/info/refs?service=git-receive-pack' \
           '/acme//unlisted.git/info/refs?service=git-receive-pack' \
           '/acme/%75nlisted.git/info/refs?service=git-receive-pack' \
           '/acme/unlisted.git%00/info/refs?service=git-receive-pack' \
           '/ACME/Unlisted.GIT/info/refs?service=git-receive-pack'; do \
           curl -s -o /dev/null -w '%{http_code}\\n' --path-as-is --max-time 30 \"https://bound.test$p\"; \
         done",
    )
    .await;
    let codes: Vec<&str> = codes.lines().collect();
    assert_eq!(
        codes,
        ["400", "400", "400", "400", "400", "400", "403"],
        "ambiguous spellings are a 400 and the case variant is the plain repository's refusal"
    );
    assert_eq!(fake.seen().len(), before, "none of them reached the host");

    // HG-14: the guest holds no token. Every place a secret could rest: the environment and
    // command line of every process, every file on the disk (binary files too), the trace of
    // the clone. The script holds the tokens in halves, so the search never finds itself.
    let patterns = format!(
        "w={work}; p={personal}; \
         printf '%s\\n' \"$w\" \"$p\" \
           \"$(printf 'x-access-token:%s' \"$w\" | base64 -w0)\" \
           \"$(printf 'x-access-token:%s' \"$p\" | base64 -w0)\" \
           \"$(printf '%s' \"$w\" | base64 -w0)\" \"$(printf '%s' \"$p\" | base64 -w0)\" > /dev/shm/.p; \
         ",
        work = split(WORK),
        personal = split(PERSONAL)
    );
    let search = format!(
        "{patterns} \
         echo procs=$(cat /proc/[0-9]*/environ /proc/[0-9]*/cmdline 2>/dev/null | tr '\\0' '\\n' | grep -cFf /dev/shm/.p); \
         echo files=$(grep -rlaFf /dev/shm/.p --exclude-dir=proc --exclude-dir=sys --exclude-dir=dev --exclude=.p / 2>/dev/null | grep -v '^/dev/shm/.p$' | tr '\\n' ' '); \
         rm -f /dev/shm/.p; echo done"
    );
    // The control first: a token planted in a file and in a process's environment must be found,
    // or the search proves nothing.
    let planted = format!(
        "w={work}; printf '%s\\n' \"$w\" > /root/planted.txt; \
         env PLANTED=\"$w\" sleep 120 & pid=$!; sleep 1; {search}; kill $pid; rm -f /root/planted.txt",
        work = split(WORK),
    );
    let control = guest_within(&vm.runtime, &sandbox, &planted, Duration::from_secs(600)).await;
    assert!(
        !control.contains("procs=0") && control.contains("/root/planted.txt"),
        "the search must find a planted token in a process and in a file: {control}"
    );
    let found = guest_within(&vm.runtime, &sandbox, &search, Duration::from_secs(600)).await;
    assert!(
        found.contains("procs=0"),
        "a token is in a process: {found}"
    );
    assert!(
        found.contains("files= done") || found.contains("files=\ndone"),
        "a token is on the guest's disk: {found}"
    );
    // The trace holds the traffic (and the redaction of any header git sent): none is ours.
    let trace = guest(
        &vm.runtime,
        &sandbox,
        "grep -ci 'authorization' /tmp/clone-trace.txt || true",
    )
    .await;
    assert_eq!(trace.trim(), "0", "git sent an Authorization of its own");

    // The audit log holds no token either; its rows say the connections were injected.
    let api = Api::new(vm.host.url(), vm.host.token());
    let audit = api
        .get(&format!(
            "/api/audit?workspace={name}&type=connection&limit=200"
        ))
        .await;
    assert!(
        !audit.body.contains(WORK) && !audit.body.contains(PERSONAL),
        "a token reached the audit log"
    );
    assert!(audit.body.contains("\"injected\":true"), "{}", audit.body);
    let _ = net.proxy.connects();
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// The CA of a workspace and what two workspaces share: no private key anywhere on the guest's
/// disk matches the CA's key, the CA says `pathLen 0` and names no constraint, a host nobody
/// bound shows its real issuer while a bound one shows the workspace's CA, two workspaces have
/// two CAs, the one with no identity gets no credential on the same host the other does, and
/// each connection is attributed to its own workspace. The guest's tools (curl, OpenSSL's
/// client and, where the image has it, Python's `requests`-free `urllib`) trust the leaf.
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_git_the_ca_holds_no_key_and_two_workspaces_share_neither_ca_nor_credential() {
    let fake =
        GitHost::start_with(&GitHostOptions::new(NAME, Versions::Both).also_for("plain.test"))
            .await;
    seed(&fake);
    let net = internet(&[(NAME, &fake), ("plain.test", &fake)]);
    let vm = start_vm_host_platform("gitca", net.options, &net.platform, |_| {}).await;
    let prefix = vm.settings.prefix.as_str().to_owned();
    let (a_name, b_name) = (format!("{prefix}-gita"), format!("{prefix}-gitb"));
    let (a, b) = (
        WorkspaceName::new(&a_name).unwrap(),
        WorkspaceName::new(&b_name).unwrap(),
    );
    allow(&vm, &a, &[NAME, "plain.test"]);
    allow(&vm, &b, &[NAME, "plain.test"]);
    // B is made while no identity exists, so it has none; A is made after Work exists and
    // takes it as its default.
    create(&vm, &b_name, &format!("https://{NAME}/acme/open.git")).await;
    vm.host
        .store()
        .create_identity(identity("Work", vec![stored(NAME, WORK, &vm, "kwork")]))
        .unwrap();
    create(&vm, &a_name, &format!("https://{NAME}/acme/web.git")).await;
    let ca_a = format!("Puddle CA for {a_name}");
    let ca_b = format!("Puddle CA for {b_name}");
    let root = "Puddle fake git host root";
    let issuer = |host: &str| {
        format!("curl -sv -o /dev/null --max-time 60 https://{host}/ 2>&1 | grep -i 'issuer:'")
    };

    // A decrypts bound.test (its CA signs the leaf); plain.test is nobody's: its real issuer.
    let out = guest(&vm.runtime, &a, &issuer(NAME)).await;
    assert!(out.contains(&ca_a), "{out}");
    let out = guest(&vm.runtime, &a, &issuer("plain.test")).await;
    assert!(out.contains(root) && !out.contains("Puddle CA"), "{out}");
    // B has no identity: bound.test is not decrypted for it, whatever A decrypts.
    let out = guest(&vm.runtime, &b, &issuer(NAME)).await;
    assert!(out.contains(root) && !out.contains(&ca_a), "{out}");

    // Two CAs.
    let fingerprint = |ws: &WorkspaceName| {
        let (runtime, ws) = (vm.runtime.clone(), ws.clone());
        async move {
            guest(
                &runtime,
                &ws,
                "openssl x509 -in /usr/local/share/ca-certificates/puddle/puddle-ca-1.crt -noout -fingerprint -sha256",
            )
            .await
        }
    };
    let (fa, fb) = (fingerprint(&a).await, fingerprint(&b).await);
    assert!(
        fa.to_lowercase().contains("sha256 fingerprint=")
            && fb.to_lowercase().contains("sha256 fingerprint="),
        "{fa} {fb}"
    );
    assert_ne!(fa.trim(), fb.trim(), "each workspace has its own CA");
    assert_ne!(ca_a, ca_b);

    // The same request on the same host: A gets the credential, B does not (the host's own 401).
    let path = "/acme/web.git/info/refs?service=git-upload-pack";
    let get = |host: &str| {
        format!("curl -s -o /dev/null -w '%{{http_code}}' --max-time 60 https://{host}{path}")
    };
    let before = fake.seen().len();
    let code_a = guest(&vm.runtime, &a, &get(NAME)).await;
    let mid = fake.seen().len();
    let code_b = guest(&vm.runtime, &b, &get(NAME)).await;
    let after = fake.seen();
    assert_eq!((code_a.trim(), code_b.trim()), ("200", "401"));
    assert_eq!(after.len(), mid + 1);
    assert_eq!(
        after
            .iter()
            .skip(before)
            .take(mid - before)
            .map(|r| r.headers_named("authorization"))
            .collect::<Vec<_>>(),
        [[basic(WORK)]],
        "A's request carries Work's credential"
    );
    assert!(
        after
            .last()
            .unwrap()
            .headers_named("authorization")
            .is_empty(),
        "B's carries none: {:?}",
        after.last()
    );

    // HG-21: the CA names no constraint and is pathLen 0, and no private key on A's disk is the
    // CA's. (Other keys exist in a distribution image: the check is by public key, not by
    // "no key at all".)
    let shape = guest(
        &vm.runtime,
        &a,
        "openssl x509 -in /usr/local/share/ca-certificates/puddle/puddle-ca-1.crt -noout -text",
    )
    .await;
    assert!(shape.contains("pathlen:0"), "{shape}");
    assert!(!shape.contains("Name Constraints"), "{shape}");
    let keys = guest_within(
        &vm.runtime,
        &a,
        "ca=$(openssl x509 -in /usr/local/share/ca-certificates/puddle/puddle-ca-1.crt -pubkey -noout | sha256sum); \
         n=0; for f in $(grep -rlI -e 'PRIVATE KEY' --exclude-dir=proc --exclude-dir=sys --exclude-dir=dev / 2>/dev/null); do \
           n=$((n+1)); \
           mine=$(openssl pkey -in \"$f\" -pubout 2>/dev/null | sha256sum); \
           [ \"$mine\" = \"$ca\" ] && echo \"CA KEY: $f\"; \
         done; echo checked=$n",
        Duration::from_secs(600),
    )
    .await;
    assert!(!keys.contains("CA KEY"), "{keys}");
    eprintln!("private-key files on the guest's disk, none of them the CA's: {keys}");

    // The tools in the stock image trust the leaf: OpenSSL's client with its own verification,
    // and Python's urllib when the image has Python. Each prints a line a failure cannot fake.
    let tools = guest(
        &vm.runtime,
        &a,
        "proxy=${HTTPS_PROXY#http://}; proxy=${proxy%/}; \
         echo openssl=$(echo | openssl s_client -proxy \"$proxy\" -connect bound.test:443 -servername bound.test \
             -verify_return_error 2>&1 | grep -c 'Verify return code: 0'); \
         if command -v python3 >/dev/null; then \
           echo python=$(python3 -c \"import urllib.request as u; print(u.urlopen('https://bound.test/acme/open.git/info/refs?service=git-upload-pack', timeout=30).status)\" 2>&1 | tail -1); \
         else echo python=absent; fi",
    )
    .await;
    assert!(tools.contains("openssl=1"), "{tools}");
    assert!(
        tools.contains("python=200") || tools.contains("python=absent"),
        "{tools}"
    );

    // Rows are attributed to the workspace they belong to.
    let api = Api::new(vm.host.url(), vm.host.token());
    for (ws, injected) in [(&a_name, true), (&b_name, false)] {
        let page = api
            .get(&format!(
                "/api/audit?workspace={ws}&type=connection&host_contains=bound.test&limit=100"
            ))
            .await
            .json();
        let entries = page["entries"].as_array().unwrap();
        assert!(!entries.is_empty(), "{ws}");
        for entry in entries {
            assert_eq!(
                entry["record"]["workspace_id"],
                json!(ws).as_str().map(|_| ws.as_str()).unwrap(),
                "{entry}"
            );
        }
        assert_eq!(
            entries.iter().any(|e| e["record"]["injected"] == true),
            injected,
            "{ws}: {entries:#?}"
        );
    }
    let _ = net.proxy.connects();
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// The guest sets its clock to 2010 and 2040: the upstream certificate check still uses the
/// host's clock alone. A certificate that is valid only in 2008 to 2012 (so valid to a guest at
/// 2010) is refused, one valid only in 2038 to 2042 (valid to a guest at 2040) is refused, and one
/// valid from 2020 to 2090 (not yet valid to a guest at 2010) is accepted. `curl -k` takes the
/// guest's own check of the proxy's leaf out of it; what is left is the proxy's upstream leg.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_git_a_guest_clock_at_2010_or_2040_does_not_change_the_upstream_check() {
    let valid = git_host(NAME).await;
    let old = GitHost::start_with(
        &GitHostOptions::new("old.test", Versions::Http1).valid_between(2008, 2012),
    )
    .await;
    old.add_repo("acme/open", false);
    let new = GitHost::start_with(
        &GitHostOptions::new("new.test", Versions::Http1).valid_between(2038, 2042),
    )
    .await;
    new.add_repo("acme/open", false);
    let net = internet(&[(NAME, &valid), ("old.test", &old), ("new.test", &new)]);
    let vm = start_vm_host_platform("gitclock", net.options, &net.platform, |_| {}).await;
    let name = format!("{}-gitclock", vm.settings.prefix.as_str());
    let sandbox = WorkspaceName::new(&name).unwrap();
    allow(&vm, &sandbox, &[NAME, "old.test", "new.test"]);
    // One identity names all three hosts, so all three are decrypted and the proxy checks each
    // upstream certificate itself.
    vm.host
        .store()
        .create_identity(identity(
            "Work",
            vec![
                stored(NAME, WORK, &vm, "kwork"),
                stored("old.test", WORK, &vm, "kold"),
                stored("new.test", WORK, &vm, "knew"),
            ],
        ))
        .unwrap();
    create(&vm, &name, &format!("https://{NAME}/acme/open.git")).await;

    let script = |date: &str| {
        format!(
            "now=$(date +%s); date -s '{date}' >/dev/null || echo SETFAIL; echo year=$(date +%Y); \
             code() {{ curl -sk -o /dev/null -w '%{{http_code}}' --max-time 30 \
               \"https://$1/acme/open.git/info/refs?service=git-upload-pack\"; }}; \
             echo old=$(code old.test) bound=$(code bound.test) new=$(code new.test); \
             date -s @$now >/dev/null"
        )
    };
    // The host's clock first: the proxy accepts the valid host and refuses the other two.
    let out = guest(&vm.runtime, &sandbox, &script("@$(date +%s)")).await;
    assert!(
        out.contains("old=502") && out.contains("bound=200") && out.contains("new=502"),
        "{out}"
    );
    for (date, year) in [
        ("2010-06-01 12:00:00", "2010"),
        ("2040-06-01 12:00:00", "2040"),
    ] {
        let out = guest(&vm.runtime, &sandbox, &script(date)).await;
        assert!(
            out.contains(&format!("year={year}")),
            "the guest's clock really moved (else the test proves nothing): {out}"
        );
        assert!(
            out.contains("old=502") && out.contains("bound=200") && out.contains("new=502"),
            "at {date} in the guest the upstream check must be what the host's clock says: {out}"
        );
    }
    // The tokens never reached the two refused hosts.
    assert!(old.seen().is_empty() && new.seen().is_empty());
    let _ = net.proxy.connects();
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
