// SPDX-License-Identifier: AGPL-3.0-or-later
//! The VM bar for the guest proxy config (tier K): with Puddle's boot-time proxy config (`puddle-guest-env`) in a tools
//! image, each tool by name reaches a local fixture through the proxy route.
//!
//! The guest's proxy listener is played by a fixture in the guest on the agent's address
//! (127.0.0.1:3128): it logs every request line and answers with a fixed response (or 502 to a
//! CONNECT). Every probe targets a name under `.invalid`, which never resolves, so a tool that
//! ignores the config fails instead of reaching the fixture another way. The VM keeps msb's
//! default network only for installing the tools first, once, from the real mirrors, into a host cache (see `SETUP_STEPS`).
//!
//! The config goes in the way the boot hook would apply it: the files written as root with their
//! modes, the env given to each exec. Until the boot hook runs in VMs, the files are
//! written by this test; the content is exactly `guest_proxy_config`'s.
//!
//! The `ssh` probes run the real `puddle-agent connect` as the drop-in's `ProxyCommand`: the fixture
//! answers an SSH client with the refusal the host proxy gives, and the probe checks that `ssh`
//! and `git` print it.
//!
//! Tools that still fail are listed in [`KNOWN_GAPS`] (capture gaps in the proxy): the test fails when
//! a tool outside the list fails, and also when a listed one starts working, so the list stays
//! true.
#![expect(
    clippy::unwrap_used,
    clippy::print_stderr,
    reason = "VM test: a failed step fails the test; the probe table goes to stderr"
)]

use std::time::{Duration, Instant};

use microsandbox::Sandbox;
use puddle_guest_env::{GuestProxyConfig, ProxySettings, guest_proxy_config};
use puddle_types::{ApplyKind, GuestFile, MergeSpec};
use puddle_vm_tests::{HarnessError, VmEnv, create_from_registry, within};

/// Node 24 (`NODE_USE_ENV_PROXY`), Debian 13 (Maven 3.9 for `MAVEN_ARGS`, `OpenJDK` 21).
/// Pinned by digest (the cache holds the packages this image's base needs, so the image must not move
/// under it); the registry's `Docker-Content-Digest` for the tag gives the next one.
const TOOLS_IMAGE: &str =
    "node:24-trixie-slim@sha256:0820eaa85e07d0490446dd01d14707e17e6108a36988cf374899cd057eb3812d";
const GRADLE_VERSION: &str = "8.14.3";
/// The SHA-256 services.gradle.org publishes for `gradle-8.14.3-bin.zip` (`<url>.sha256`).
const GRADLE_SHA256: &str = "bd71102213493060956ec229d946beee57158dbd89d0e62b91bca0fa2c5f3531";
const PNPM: &str = "pnpm@10.12.1";
const YARN_BERRY: &str = "yarn@4.9.2";
/// The SHA-512 corepack computes over each archive it downloads, i.e. the `+sha512.<hex>` suffix
/// `corepack use <tool>@<version>` writes into `packageManager`. The pnpm one equals the registry's
/// `dist.integrity` of `pnpm@10.12.1` (base64 of the same digest).
const PNPM_SHA512: &str = "f0dda8580f0ee9481c5c79a1d927b9164f2c478e90992ad268bbb2465a736984391d6333d2c327913578b2804af33474ca554ba29c04a8b13060a717675ae3ac";
const YARN_BERRY_SHA512: &str = "1fc009bc09d13cfd0e19efa44cbfc2b9cf6ca61482725eb35bbc5e257e093ebf4130db6dfe15d604ff4b79efd8e1e8e99b25fa7d0a6197c9f9826358d4d65c3c";

const CREATE_BUDGET: Duration = Duration::from_secs(300);
/// Per setup step; the guest enforces it with `timeout` (a bit less) and writes the step's output to a
/// file, so a hung step ends with its output instead of a bare host-side timeout.
const SETUP_STEP_BUDGET: Duration = Duration::from_secs(300);
const PROBE_BUDGET: Duration = Duration::from_secs(150);
const STEP_BUDGET: Duration = Duration::from_secs(60);

/// Probes expected to fail today, by name (see the module docs).
const KNOWN_GAPS: &[&str] = &[];

/// The packages the tools need (installed from Debian once, then from the cache).
const APT_PACKAGES: &str = "ca-certificates curl wget git openssh-client sudo unzip python3-pip \
                            openjdk-21-jdk-headless maven";

/// The tool install, one named step at a time so the log shows which step is slow or hangs.
///
/// Each step is `(name, third-party host it needs on a cold cache, script)`. The tools are the real
/// JDK, Maven, Gradle, pnpm and Yarn that the proxy config is written for (a mirror serving
/// stand-ins would test nothing), but they are downloaded once: a step that finds its files in
/// `/vmcache` (the host's [`tools_cache`], keyed by [`TOOLS_IMAGE`]'s digest and the versions here)
/// installs from there and uses no network, so a run on a warm cache depends on no third-party
/// host. A cold cache is filled by the same step, which then names the host in the failure message.
/// Nothing after the setup reaches the internet (every probe targets a `.invalid` name).
const SETUP_STEPS: &[(&str, &str, &str)] = &[
    (
        // Names and routes the install needs, so a stall can be told from a dead network. The
        // lookup is timed on its own. Skipped on a warm cache: nothing downloads then.
        "name lookup",
        "deb.debian.org (cold cache only)",
        "[ -f /vmcache/apt/ok ] && { echo 'warm cache: no download'; exit 0; }; \
         t=$(date +%s%N); getent ahosts deb.debian.org | head -4; \
         echo \"lookup ms=$((($(date +%s%N) - t) / 1000000))\"",
    ),
    (
        // Node ends the process itself: after the response it would otherwise wait for the idle
        // keep-alive connection to close, which took about 8 s of every probe.
        "network probe",
        "deb.debian.org (cold cache only)",
        "[ -f /vmcache/apt/ok ] && { echo 'warm cache: no download'; exit 0; }; \
         node -e \"const t = Date.now(); \\
         fetch('http://deb.debian.org/debian/dists/trixie/InRelease', {signal: AbortSignal.timeout(20000)}) \\
         .then((r) => { console.log('debian http=' + r.status + ' ms=' + (Date.now() - t)); process.exit(0); }) \\
         .catch((e) => { console.log('debian failed ms=' + (Date.now() - t) + ' ' + e); process.exit(1); })\"",
    ),
    (
        // Cold: update, download the packages (docker-clean deletes archives after an install, so
        // `--download-only` first), keep the lists and archives in the cache, then install. Warm:
        // put them back and install with `--no-download`, which fails instead of reaching a mirror.
        // Output kept (no -qq): a stall shows the last package unpacked.
        "apt install (JDK 21, maven, tools)",
        "deb.debian.org (cold cache only)",
        "C=/vmcache/apt; \
         opts='-y -o Dpkg::Use-Pty=0 --no-install-recommends'; \
         if [ -f $C/ok ]; then \
           mkdir -p /var/lib/apt/lists /var/cache/apt/archives && \
           cp -a $C/lists/. /var/lib/apt/lists/ && cp -a $C/archives/. /var/cache/apt/archives/ && \
           apt-get install $opts --no-download -o Acquire::Check-Valid-Until=false $APT_PACKAGES; \
         else \
           apt-get update -qq && apt-get install $opts --download-only $APT_PACKAGES && \
           rm -rf $C.tmp && mkdir -p $C.tmp/lists $C.tmp/archives && \
           cp -a /var/lib/apt/lists/. $C.tmp/lists/ && rm -f $C.tmp/lists/lock && \
           cp /var/cache/apt/archives/*.deb $C.tmp/archives/ && touch $C.tmp/ok && \
           rm -rf $C && mv $C.tmp $C && \
           apt-get install $opts $APT_PACKAGES; \
         fi",
    ),
    (
        // Verified against the pinned hash whether it comes from the cache or the download; the
        // download retries over a window sized for a CDN outage (a 504 from services.gradle.org
        // lasted longer than three tries 5 s apart).
        "gradle (cache, else download from services.gradle.org)",
        "services.gradle.org (cold cache only)",
        "Z=/vmcache/gradle-$GRADLE_VERSION-bin.zip; \
         if ! { [ -f $Z ] && echo \"$GRADLE_SHA256  $Z\" | sha256sum -c -; }; then \
           n=1; pause=5; \
           until curl -fsSL -w 'http=%{http_code} bytes=%{size_download} total=%{time_total}s\\n' -o $Z.part \
             \"https://services.gradle.org/distributions/gradle-$GRADLE_VERSION-bin.zip\"; do \
             [ $n -lt 6 ] || { echo 'gradle download failed 6 times'; exit 1; }; \
             sleep $pause; pause=$((pause * 2)); n=$((n + 1)); \
           done; \
           echo \"$GRADLE_SHA256  $Z.part\" | sha256sum -c - && mv $Z.part $Z; \
         fi; \
         unzip -oq $Z -d /opt && ln -sf \"/opt/gradle-$GRADLE_VERSION/bin/gradle\" /usr/local/bin/gradle",
    ),
    (
        // The corepack home holds both tools; on a cold cache it is copied out once both are there.
        // Each archive is fetched with its `+sha512` pin, so a wrong hash or a swapped archive fails
        // the step here, naming the tool, instead of passing on corepack's registry signature check.
        "corepack pnpm and yarn berry",
        "registry.npmjs.org, repo.yarnpkg.com (cold cache only)",
        "export COREPACK_ENABLE_DOWNLOAD_PROMPT=0; H=\"$HOME/.cache/node/corepack\"; C=/vmcache/corepack; \
         if [ -f $C/ok ]; then mkdir -p \"$H\" && cp -a $C/home/. \"$H\"/; fi; \
         corepack enable pnpm && \
         { corepack install -g \"$PNPM+sha512.$PNPM_SHA512\" || { echo \"pnpm archive check or download failed: $PNPM+sha512.$PNPM_SHA512\"; exit 1; }; } && \
         { corepack install -g \"$YARN_BERRY+sha512.$YARN_BERRY_SHA512\" || { echo \"yarn berry archive check or download failed: $YARN_BERRY+sha512.$YARN_BERRY_SHA512\"; exit 1; }; } && \
         mkdir -p /tmp/warm && cd /tmp/warm && corepack \"$YARN_BERRY\" --version && \
         if [ ! -f $C/ok ]; then \
           rm -rf $C.tmp && mkdir -p $C.tmp/home && cp -a \"$H\"/. $C.tmp/home/ && touch $C.tmp/ok && \
           rm -rf $C && mv $C.tmp $C; \
         fi",
    ),
    (
        "tool versions",
        "none (local step)",
        "mvn -v | head -1; gradle --version -q | grep Gradle; node -v; pnpm -v; yarn -v; java -version 2>&1 | head -1",
    ),
];

/// The host folder mounted at `/vmcache`: `PUDDLE_VM_TOOLS_CACHE` (CI restores it between runs) or
/// `~/.cache/puddle-vm-tools`, then one folder for everything the setup downloads, named by a hash of
/// what it holds (image, versions, install steps) so a change starts a cold one instead of mixing.
fn tools_cache() -> std::path::PathBuf {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let root = std::env::var_os("PUDDLE_VM_TOOLS_CACHE")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::Path::new(&h).join(".cache/puddle-vm-tools"))
        })
        .unwrap();
    let mut hasher = DefaultHasher::new();
    (
        TOOLS_IMAGE,
        APT_PACKAGES,
        GRADLE_SHA256,
        PNPM,
        PNPM_SHA512,
        YARN_BERRY,
        YARN_BERRY_SHA512,
        SETUP_STEPS,
    )
        .hash(&mut hasher);
    let dir = root.join(format!("guestenv-{:016x}", hasher.finish()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The guest side of the route: logs `<request line> host=<Host>` per request.
const FIXTURE: &str = r"
const net = require('net'), fs = require('fs');
const log = fs.openSync('/tmp/fixture.log', 'a');
net.createServer((s) => {
  let buf = '';
  s.on('error', () => {});
  s.on('data', (d) => {
    buf += d.toString('latin1');
    const end = buf.indexOf('\r\n\r\n');
    if (end < 0) return;
    s.removeAllListeners('data');
    const lines = buf.slice(0, end).split('\r\n');
    const host = (lines.find((l) => /^host:/i.test(l)) || 'host:').slice(5).trim();
    const hint = (lines.find((l) => /^x-puddle-protocol:/i.test(l)) || ':').split(':').slice(1).join(':').trim();
    fs.writeSync(log, lines[0] + ' host=' + host + (hint ? ' protocol=' + hint : '') + '\n');
    if (hint === 'ssh') {
      const body = 'puddle: SSH is not supported yet\n';
      s.end('HTTP/1.1 403 Forbidden\r\nContent-Length: ' + body.length + '\r\nConnection: close\r\n\r\n' + body);
    } else if (lines[0].startsWith('CONNECT ')) {
      s.end('HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n');
    } else {
      s.end('HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 15\r\nConnection: close\r\n\r\npuddle-fixture\n');
    }
  });
}).listen(3128, '127.0.0.1');
";

const JAVA_FETCH: &str = r#"
public class Fetch {
  public static void main(String[] a) throws Exception {
    var uri = java.net.URI.create(a[0]);
    if (a[1].equals("url")) {
      var c = (java.net.HttpURLConnection) uri.toURL().openConnection();
      c.setConnectTimeout(20000);
      System.out.println(c.getResponseCode());
    } else {
      var c = java.net.http.HttpClient.newHttpClient();
      var r = c.send(java.net.http.HttpRequest.newBuilder(uri).build(),
          java.net.http.HttpResponse.BodyHandlers.ofString());
      System.out.println(r.statusCode());
    }
  }
}
"#;

/// One tool reaching the fixture: passes when a request for one of `hosts` reaches the fixture
/// while `script` runs.
struct Probe {
    name: &'static str,
    hosts: &'static [&'static str],
    script: String,
    /// Text the tool's own output must contain as well.
    prints: Option<&'static str>,
}

fn probe(name: &'static str, hosts: &'static [&'static str], script: &str) -> Probe {
    Probe {
        name,
        hosts,
        script: script.to_owned(),
        prints: None,
    }
}

/// A probe that also passes only when the tool prints `prints`.
fn probe_printing(
    name: &'static str,
    hosts: &'static [&'static str],
    script: &str,
    prints: &'static str,
) -> Probe {
    Probe {
        prints: Some(prints),
        ..probe(name, hosts, script)
    }
}

fn node_project(dir: &str, extra: &str) -> String {
    format!(
        "mkdir -p {dir} && cd {dir} && printf '%s' \
         '{{\"name\":\"p\",\"version\":\"1.0.0\",{extra}\"dependencies\":{{\"left-pad\":\"1.3.0\"}}}}' \
         > package.json"
    )
}

fn maven_project(dir: &str, repo: &str) -> String {
    format!(
        "mkdir -p {dir} && cd {dir} && cat > pom.xml <<'EOF'
<project xmlns=\"http://maven.apache.org/POM/4.0.0\">
  <modelVersion>4.0.0</modelVersion>
  <groupId>t</groupId><artifactId>p</artifactId><version>1</version>
  <repositories><repository><id>fx</id><url>{repo}</url></repository></repositories>
  <dependencies><dependency><groupId>org.example</groupId><artifactId>thing</artifactId><version>1.0</version></dependency></dependencies>
</project>
EOF
mvn -B -U -Dmaven.repo.local={dir}/m2 compile"
    )
}

fn gradle_project(dir: &str, repo: &str) -> String {
    format!(
        "mkdir -p {dir} && cd {dir} && : > settings.gradle && cat > build.gradle <<'EOF'
repositories {{ maven {{ url = '{repo}' }} }}
configurations {{ fetch }}
dependencies {{ fetch 'org.example:thing:1.0' }}
tasks.register('fetch') {{ doLast {{ configurations.fetch.resolve() }} }}
EOF
gradle --no-daemon --refresh-dependencies -q fetch"
    )
}

fn apt_update(list: &str) -> String {
    format!(
        "mkdir -p /tmp/apt-empty && echo '{list}' > /tmp/apt-probe.list && \
         env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin apt-get -o Dir::Etc::SourceList=/tmp/apt-probe.list \
         -o Dir::Etc::SourceParts=/tmp/apt-empty -o Acquire::Retries=0 update"
    )
}

#[expect(clippy::too_many_lines, reason = "one flat list of probes reads best")]
fn probes() -> Vec<Probe> {
    let node_fetch = |url: &str| {
        format!(
            "node -e \"fetch('{url}').then(r => r.text()).then(console.log)\
             .catch(e => {{ console.error(e); process.exit(1); }})\""
        )
    };
    vec![
        probe(
            "curl http",
            &["curl.fixture.invalid"],
            "curl -sS -m 20 http://curl.fixture.invalid/a",
        ),
        probe(
            "curl https",
            &["curl-tls.fixture.invalid"],
            "curl -sS -m 20 https://curl-tls.fixture.invalid/a",
        ),
        probe(
            "wget http",
            &["wget.fixture.invalid"],
            "wget -q -O- -T 20 -t 1 http://wget.fixture.invalid/a",
        ),
        probe(
            "wget https",
            &["wget-tls.fixture.invalid"],
            "wget -q -O- -T 20 -t 1 https://wget-tls.fixture.invalid/a",
        ),
        probe(
            "git http",
            &["git.fixture.invalid"],
            "git ls-remote http://git.fixture.invalid/r.git",
        ),
        probe(
            "git https",
            &["git-tls.fixture.invalid"],
            "git ls-remote https://git-tls.fixture.invalid/r.git",
        ),
        probe(
            "apt http (file only)",
            &["apt.fixture.invalid"],
            &apt_update("deb http://apt.fixture.invalid/debian trixie main"),
        ),
        probe(
            "apt https (file only)",
            &["apt-tls.fixture.invalid"],
            &apt_update("deb https://apt-tls.fixture.invalid/debian trixie main"),
        ),
        probe(
            "npm http",
            &["npm.fixture.invalid"],
            "npm view left-pad --registry=http://npm.fixture.invalid/ --fetch-retries=0",
        ),
        probe(
            "npm https",
            &["npm-tls.fixture.invalid"],
            "npm view left-pad --registry=https://npm-tls.fixture.invalid/ --fetch-retries=0",
        ),
        probe(
            "pnpm http",
            &["pnpm.fixture.invalid"],
            &format!(
                "{} && pnpm install --registry=http://pnpm.fixture.invalid/ --fetch-retries=0",
                node_project("/tmp/pnpm-a", "")
            ),
        ),
        probe(
            "pnpm https",
            &["pnpm-tls.fixture.invalid"],
            &format!(
                "{} && pnpm install --registry=https://pnpm-tls.fixture.invalid/ --fetch-retries=0",
                node_project("/tmp/pnpm-b", "")
            ),
        ),
        probe(
            "yarn 1 http",
            &["yarn.fixture.invalid"],
            "cd /tmp && yarn info left-pad --registry http://yarn.fixture.invalid/ --non-interactive",
        ),
        probe(
            "yarn 1 https",
            &["yarn-tls.fixture.invalid"],
            "cd /tmp && yarn info left-pad --registry https://yarn-tls.fixture.invalid/ --non-interactive",
        ),
        probe(
            "yarn berry http",
            &["berry.fixture.invalid"],
            &format!(
                "{} && touch yarn.lock && YARN_NPM_REGISTRY_SERVER=http://berry.fixture.invalid \
             YARN_UNSAFE_HTTP_WHITELIST=berry.fixture.invalid YARN_ENABLE_IMMUTABLE_INSTALLS=false \
             COREPACK_ENABLE_DOWNLOAD_PROMPT=0 corepack yarn install",
                node_project(
                    "/tmp/berry-a",
                    &format!("\"packageManager\":\"{YARN_BERRY}\",")
                )
            ),
        ),
        probe(
            "yarn berry https",
            &["berry-tls.fixture.invalid"],
            &format!(
                "{} && touch yarn.lock && YARN_NPM_REGISTRY_SERVER=https://berry-tls.fixture.invalid \
             YARN_ENABLE_IMMUTABLE_INSTALLS=false COREPACK_ENABLE_DOWNLOAD_PROMPT=0 corepack yarn install",
                node_project(
                    "/tmp/berry-b",
                    &format!("\"packageManager\":\"{YARN_BERRY}\",")
                )
            ),
        ),
        probe(
            "pip http",
            &["pip.fixture.invalid"],
            "python3 -m pip download --no-deps --retries 0 --timeout 20 -d /tmp/pipd \
            --index-url http://pip.fixture.invalid/simple --trusted-host pip.fixture.invalid left-pad",
        ),
        probe(
            "pip https",
            &["pip-tls.fixture.invalid"],
            "python3 -m pip download --no-deps --retries 0 --timeout 20 -d /tmp/pipd \
            --index-url https://pip-tls.fixture.invalid/simple left-pad",
        ),
        probe(
            "maven",
            &["repo.maven.apache.org", "mvn-tls.fixture.invalid"],
            &maven_project("/tmp/mvn-a", "https://mvn-tls.fixture.invalid/m2"),
        ),
        probe(
            "maven (settings file only)",
            &["repo.maven.apache.org", "mvn2-tls.fixture.invalid"],
            &format!(
                "unset JAVA_TOOL_OPTIONS && {}",
                maven_project("/tmp/mvn-b", "https://mvn2-tls.fixture.invalid/m2")
            ),
        ),
        probe(
            "gradle",
            &["gradle-tls.fixture.invalid"],
            &gradle_project("/tmp/gradle-a", "https://gradle-tls.fixture.invalid/m2"),
        ),
        probe(
            "gradle (init script only)",
            &["gradle2-tls.fixture.invalid"],
            &format!(
                "unset JAVA_TOOL_OPTIONS && {}",
                gradle_project("/tmp/gradle-b", "https://gradle2-tls.fixture.invalid/m2")
            ),
        ),
        probe(
            "node fetch http",
            &["node.fixture.invalid"],
            &node_fetch("http://node.fixture.invalid/a"),
        ),
        probe(
            "node fetch https",
            &["node-tls.fixture.invalid"],
            &node_fetch("https://node-tls.fixture.invalid/a"),
        ),
        probe(
            "java HttpURLConnection http",
            &["java.fixture.invalid"],
            "java /tmp/Fetch.java http://java.fixture.invalid/a url",
        ),
        probe(
            "java HttpURLConnection https",
            &["java-tls.fixture.invalid"],
            "java /tmp/Fetch.java https://java-tls.fixture.invalid/a url",
        ),
        probe(
            "java HttpClient https",
            &["javahc-tls.fixture.invalid"],
            "java /tmp/Fetch.java https://javahc-tls.fixture.invalid/a client",
        ),
        // SSH is not supported yet: `ssh` runs the agent as its ProxyCommand (the drop-in), which
        // announces the SSH banner to the proxy and prints the refusal it gets back.
        probe_printing(
            "ssh refused with a message",
            &["ssh.fixture.invalid:22 HTTP/1.1 host=ssh.fixture.invalid:22 protocol=ssh"],
            "ssh -T -o StrictHostKeyChecking=no -o BatchMode=yes git@ssh.fixture.invalid true",
            "puddle: SSH is not supported yet",
        ),
        probe_printing(
            "git over ssh refused with a message",
            &["ssh-git.fixture.invalid:22 HTTP/1.1 host=ssh-git.fixture.invalid:22 protocol=ssh"],
            "git ls-remote git@ssh-git.fixture.invalid:owner/repo.git",
            "puddle: SSH is not supported yet",
        ),
        probe(
            "sudo keeps the vars (curl)",
            &["sudo.fixture.invalid"],
            "sudo -u node curl -sS -m 20 http://sudo.fixture.invalid/a",
        ),
        probe(
            "sudo keeps the vars (node fetch)",
            &["sudo-node.fixture.invalid"],
            &format!(
                "sudo -u node {}",
                node_fetch("https://sudo-node.fixture.invalid/a")
            ),
        ),
    ]
}

/// Runs `script` as root and returns its exit code and output.
async fn root_shell(sb: &Sandbox, script: &str) -> (i32, String) {
    let out = within(
        "shell",
        STEP_BUDGET,
        sb.shell_with(script, |e| e.user("root")),
    )
    .await
    .unwrap()
    .unwrap();
    (out.status().code, out.stdout().unwrap_or_default())
}

/// Where the spec of the merged file at `guest_path` is kept.
fn merge_spec_path(guest_path: &str) -> String {
    format!("/tmp/merge{}.spec", guest_path.replace('/', "_"))
}

/// Applies a merged file as the boot hook does: through `puddle-agent merge-file`.
async fn merge_file(sb: &Sandbox, file: &GuestFile, spec: &MergeSpec) {
    let spec_file = merge_spec_path(file.path().as_str());
    write_file(sb, &spec_file, 0o600, &serde_json::to_vec(spec).unwrap()).await;
    let path = sh_quote(file.path().as_str());
    let (code, out) = root_shell(
        sb,
        &format!(
            "/puddle/puddle-agent merge-file apply /var/lib/puddle/merge {path} {path} {:o} \
             <{spec_file} 2>&1",
            file.mode()
        ),
    )
    .await;
    assert_eq!(code, 0, "merging {path}: {out}");
    assert!(
        out.trim() == "changed" || out.trim() == "unchanged",
        "merging {path}: {out}"
    );
}

/// Takes the `Include` lines out of the image's `ssh_config`; returns the file as it is then. Checks
/// that `ssh` does not see the drop-in's `ProxyCommand` (nothing is written yet).
async fn ssh_config_without_include(sb: &Sandbox) -> String {
    let (code, out) = root_shell(
        sb,
        "sed -i '/^[[:space:]]*[Ii]nclude/d' /etc/ssh/ssh_config && cat /etc/ssh/ssh_config",
    )
    .await;
    assert_eq!(code, 0, "{out}");
    assert!(!out.to_lowercase().contains("include"), "{out}");
    out
}

/// After the merge: the file is the stripped original behind the line Puddle adds, a second merge
/// changes nothing, and `ssh` now reads the drop-in.
async fn check_ssh_config_include(sb: &Sandbox, before: &str) {
    let (_, after) = root_shell(sb, "cat /etc/ssh/ssh_config").await;
    let added = after.strip_suffix(before);
    assert!(
        added.is_some(),
        "the original file is not kept byte for byte:\n{after}"
    );
    let added = added.unwrap_or_default();
    assert!(
        added.ends_with("\nInclude /etc/ssh/ssh_config.d/*.conf\n")
            && added.starts_with("# Added by Puddle"),
        "{added:?}"
    );
    let spec = merge_spec_path("/etc/ssh/ssh_config");
    let (_, again) = root_shell(
        sb,
        &format!("cp /etc/ssh/ssh_config /tmp/ssh_config.merged; \
         /puddle/puddle-agent merge-file apply /var/lib/puddle/merge /etc/ssh/ssh_config /etc/ssh/ssh_config 644 <{spec} 2>&1; \
         cmp /etc/ssh/ssh_config /tmp/ssh_config.merged && echo same"),
    )
    .await;
    assert_eq!(again.trim(), "unchanged\nsame", "{again}");
    let (_, proxy) = root_shell(
        sb,
        "ssh -G git@ssh.fixture.invalid | grep -i '^proxycommand'",
    )
    .await;
    assert!(
        proxy.contains("connect"),
        "ssh does not read the drop-in: {proxy:?}"
    );
}

/// Runs `script` as root with `config`'s env; returns exit code and the output's tail.
async fn run(sb: &Sandbox, config: &GuestProxyConfig, script: &str) -> (i32, String) {
    let env: Vec<(String, String)> = config
        .env
        .iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    let script = format!("exec 2>&1; timeout 120 sh -c {}", sh_quote(script));
    let out = within(
        "probe",
        PROBE_BUDGET,
        sb.shell_with(script, |e| e.user("root").envs(env).timeout(PROBE_BUDGET)),
    )
    .await
    .unwrap()
    .unwrap();
    let text = out.stdout().unwrap_or_default();
    let tail: String = text
        .lines()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ");
    (out.status().code, tail)
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Writes `contents` to `path` as root with `mode`, creating the directory.
async fn write_file(sb: &Sandbox, path: &str, mode: u32, contents: &[u8]) {
    let out = within(
        "write file",
        STEP_BUDGET,
        sb.exec_with("/bin/sh", |e| {
            e.args([
                "-c",
                "mkdir -p \"$(dirname \"$1\")\" && cat > \"$1\" && chmod \"$2\" \"$1\"",
                "sh",
                path,
                &format!("{mode:o}"),
            ])
            .user("root")
            .stdin_bytes(contents.to_vec())
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(out.status().code, 0, "writing {path}: {:?}", out.stderr());
}

async fn fixture_log(sb: &Sandbox) -> Vec<String> {
    let out = within(
        "read log",
        STEP_BUDGET,
        sb.shell("cat /tmp/fixture.log 2>/dev/null || true"),
    )
    .await
    .unwrap()
    .unwrap();
    out.stdout().unwrap().lines().map(str::to_owned).collect()
}

/// Runs one setup step as root and prints its time and output tail; panics, naming the step, when it
/// fails or exceeds [`SETUP_STEP_BUDGET`].
async fn setup_step(sb: &Sandbox, name: &str, host: &str, script: &str) {
    let guest_limit = SETUP_STEP_BUDGET.as_secs() - 10;
    // Every step downloads from a public registry or mirror through msb's default network, where one
    // request can fail on its own (a corepack fetch of the pnpm manifest died after 3.8 s while the
    // steps before it, on the same network, passed). Each step is idempotent, so a failed one is run
    // again, up to three times inside the one guest time limit; the log keeps every attempt.
    let wrapped = format!(
        "export DEBIAN_FRONTEND=noninteractive; \
         timeout -k 5 {guest_limit} sh -c 'for n in 1 2 3; do \
           echo \"-- attempt $n\"; sh -c \"$1\" && exit 0; sleep 5; done; exit 1' sh {} \
         >/tmp/setup-step.log 2>&1; rc=$?; \
         tail -n 40 /tmp/setup-step.log; exit $rc",
        sh_quote(&format!("set -eu; {script}"))
    );
    let started = Instant::now();
    let out = within(
        "setup step",
        SETUP_STEP_BUDGET,
        sb.shell_with(wrapped, |e| {
            e.user("root")
                .env("GRADLE_VERSION", GRADLE_VERSION)
                .env("GRADLE_SHA256", GRADLE_SHA256)
                .env("APT_PACKAGES", APT_PACKAGES)
                .env("PNPM", PNPM)
                .env("YARN_BERRY", YARN_BERRY)
                .env("PNPM_SHA512", PNPM_SHA512)
                .env("YARN_BERRY_SHA512", YARN_BERRY_SHA512)
                .timeout(SETUP_STEP_BUDGET)
        }),
    )
    .await;
    let secs = started.elapsed().as_secs_f32();
    assert!(
        matches!(out, Ok(Ok(_))),
        "setup step '{name}' (third-party host: {host}) did not finish in {secs:.1} s: {out:?}",
    );
    let out = out.unwrap().unwrap();
    let text = out.stdout().unwrap_or_default();
    eprintln!(
        "setup step '{name}': {secs:.1} s, exit {}\n{}",
        out.status().code,
        tail(&text, 8)
    );
    assert_eq!(
        out.status().code,
        0,
        "setup step '{name}' (third-party host: {host}) failed:\n{text}"
    );
}

/// The last `n` lines of `text`.
fn tail(text: &str, n: usize) -> String {
    let skip = text.lines().count().saturating_sub(n);
    text.lines().skip(skip).collect::<Vec<_>>().join("\n")
}

struct Outcome {
    name: &'static str,
    reached: bool,
    code: i32,
    seen: Vec<String>,
    tail: String,
    secs: f32,
}

async fn tools_reach_the_fixture(env: &VmEnv) -> Result<Vec<Outcome>, HarnessError> {
    let config = guest_proxy_config(&ProxySettings::default(), &[]).unwrap();
    let cache = tools_cache();
    eprintln!("tools cache: {}", cache.display());
    let builder = env
        .sandbox("guestenv", TOOLS_IMAGE)?
        .cpus(2)
        .memory(3072u32)
        .volume("/vmcache", |m| m.bind(&cache));
    env.scope(Box::pin(async {
        let started = Instant::now();
        let sb = create_from_registry(TOOLS_IMAGE, CREATE_BUDGET, builder.create()).await?;
        eprintln!("create (pull + boot) {} s", started.elapsed().as_secs());

        let started = Instant::now();
        for (name, host, script) in SETUP_STEPS {
            setup_step(&sb, name, host, script).await;
        }
        eprintln!("setup total {} s", started.elapsed().as_secs());

        // The agent binary the ssh drop-in runs as its ProxyCommand and the boot hook's merge tool
        // (the static build, as the boot hook's VM tests use).
        let agent_bin = std::env::var_os("PUDDLE_AGENT_BIN");
        assert!(
            agent_bin.is_some(),
            "PUDDLE_AGENT_BIN: the static puddle-agent (ci/build-agent.sh)"
        );
        let agent = std::fs::read(agent_bin.unwrap()).unwrap();
        write_file(&sb, "/puddle/puddle-agent", 0o755, &agent).await;
        // An image whose ssh_config has no Include line (Debian's has one): the drop-in is not
        // read until the merge below adds the line.
        let before = ssh_config_without_include(&sb).await;
        for f in &config.files {
            match f.apply() {
                ApplyKind::Merge(spec) => merge_file(&sb, f, spec).await,
                _ => write_file(&sb, f.path().as_str(), f.mode(), f.contents()).await,
            }
        }
        check_ssh_config_include(&sb, &before).await;
        write_file(&sb, "/tmp/fixture.js", 0o644, FIXTURE.as_bytes()).await;
        write_file(&sb, "/tmp/Fetch.java", 0o644, JAVA_FETCH.as_bytes()).await;
        let up = within(
            "fixture",
            STEP_BUDGET,
            sb.shell(
                "setsid node /tmp/fixture.js >/tmp/fixture.out 2>&1 </dev/null & \
                 for i in $(seq 50); do \
                   curl -s --noproxy '*' -o /dev/null http://127.0.0.1:3128/ready && exit 0; sleep 0.2; \
                 done; cat /tmp/fixture.out; exit 1",
            ),
        )
        .await??;
        assert_eq!(up.status().code, 0, "fixture: {:?}", up.stdout());

        let mut outcomes = Vec::new();
        for p in probes() {
            let before = fixture_log(&sb).await.len();
            let started = Instant::now();
            let (code, tail) = run(&sb, &config, &p.script).await;
            let secs = started.elapsed().as_secs_f32();
            let seen: Vec<String> = fixture_log(&sb).await.split_off(before);
            let reached = seen.iter().any(|l| p.hosts.iter().any(|h| l.contains(h)))
                && p.prints.is_none_or(|text| tail.contains(text));
            outcomes.push(Outcome { name: p.name, reached, code, seen, tail, secs });
        }

        within("stop", STEP_BUDGET, sb.stop()).await??;
        within("remove", STEP_BUDGET, Sandbox::remove(sb.name())).await??;
        Ok(outcomes)
    }))
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_guest_env_tools_reach_the_proxy() {
    let env = VmEnv::from_env().await.expect("VM test environment");
    let result = Box::pin(tools_reach_the_fixture(&env)).await;
    let cleaned = Box::pin(env.cleanup()).await;
    let outcomes = result.expect("probes");

    eprintln!(
        "\n| probe | reached the proxy | exit | time | fixture saw | output tail |\n|---|---|---|---|---|---|"
    );
    for o in &outcomes {
        eprintln!(
            "| {} | {} | {} | {:.1} s | {} | {} |",
            o.name,
            if o.reached { "yes" } else { "**no**" },
            o.code,
            o.secs,
            o.seen.join("; "),
            o.tail.replace('|', "/")
        );
    }
    let failing: Vec<&str> = outcomes
        .iter()
        .filter(|o| !o.reached)
        .map(|o| o.name)
        .collect();
    let unexpected: Vec<&&str> = failing.iter().filter(|n| !KNOWN_GAPS.contains(n)).collect();
    let fixed: Vec<&&str> = KNOWN_GAPS.iter().filter(|n| !failing.contains(n)).collect();
    assert!(
        unexpected.is_empty(),
        "tools that did not reach the proxy: {unexpected:?}"
    );
    assert!(
        fixed.is_empty(),
        "known gaps that now work (remove them from KNOWN_GAPS): {fixed:?}"
    );

    cleaned.expect("cleanup");
    env.remove_home().await.expect("remove the private home");
}
