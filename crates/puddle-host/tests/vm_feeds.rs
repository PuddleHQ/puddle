// SPDX-License-Identifier: AGPL-3.0-or-later
//! Private package feeds on a real microVM (K on Linux KVM): the real `npm`, `pip`, `dotnet`
//! (`NuGet`) and `mvn` install a package from a feed that answers `401` to a request without its
//! token, with the token held in the guest only as a secret's stand-in.
//!
//! The fake feeds ([`puddle_e2e::lab::private_feeds`]) are behind a company proxy and the host
//! decrypts them because a secret names each. For each tool the guest's own config file names the
//! stand-in (an environment reference where the tool expands one, the stand-in itself where it
//! does not), and the test says, per tool: did the install work, did the feed receive the real
//! token, did the stand-in reach the feed unswapped, and is the real token anywhere in the guest.
//! A second test does the same for the Azure Artifacts credential provider, whose
//! `ARTIFACTS_CREDENTIALPROVIDER_EXTERNAL_FEED_ENDPOINTS` (the variant that takes a password; the
//! plain `..._FEED_ENDPOINTS` takes a service principal's client id and certificate) carries the stand-in inside a JSON value.
//!
//! Tools and the `dotnet` SDK are installed in the guest from the live package mirrors, so the
//! test is named `vm_radar_*`: the per-pull-request jobs skip it.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::indexing_slicing,
    reason = "VM test: a failed step fails the test; measurements go to stderr"
)]

mod support;
mod vm_support;

use std::time::Duration;

use puddle_compute_msb::MsbRuntime;
use puddle_e2e::lab::{
    MAVEN_COORDINATES, NPM_PACKAGE, NUGET_PACKAGE, PIP_PROJECT, WebOptions, private_feeds,
};
use puddle_host::Host;
use puddle_store::{Actor, Effect, NewRule, Pattern, Scope, StartValue};
use puddle_types::WorkspaceId;
use serde_json::json;
use support::{Api, ended, named_id_options};
use vm_support::{VmHost, WebInternet, guest_within, start_vm_host_platform, web_internet};

const NPM: &str = "npm.feeds.invalid";
const NUGET: &str = "nuget.feeds.invalid";
const PIP: &str = "pip.feeds.invalid";
const MAVEN: &str = "maven.feeds.invalid";
const REAL: &str = "puddle-feed-real-5b7e0c91d2a64f38";

/// Live hosts the guest reaches to install the tools and (for Maven) its dependency plugin.
const MIRRORS: &[&str] = &[
    "deb.debian.org",
    "security.debian.org",
    "repo.maven.apache.org",
    "repo1.maven.org",
    "dot.net",
    "builds.dotnet.microsoft.com",
    "ci.dot.net",
    "dotnetcli.azureedge.net",
    "dotnetcli.blob.core.windows.net",
];

/// Where the credential provider is downloaded from (a pinned release) and the hosts that
/// download touches, on top of [`MIRRORS`].
const PROVIDER_URL: &str = "https://github.com/microsoft/artifacts-credprovider/releases/download/v2.0.6/Microsoft.Net8.NuGet.CredentialProvider.tar.gz";
const PROVIDER_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// Installs the four tools. `dotnet` is not in Debian: the SDK comes from Microsoft's script.
const INSTALL: &str = r#"
export DEBIAN_FRONTEND=noninteractive
{ apt-get update -qq && apt-get install -y -qq --no-install-recommends nodejs npm python3 python3-pip maven default-jdk-headless ca-certificates-java curl ca-certificates libicu-dev; } >/tmp/apt.log 2>&1
echo "apt=$?"
curl -fsSL https://dot.net/v1/dotnet-install.sh -o /tmp/dotnet-install.sh >/tmp/dotnet.log 2>&1 \
  && bash /tmp/dotnet-install.sh --channel 8.0 --install-dir /opt/dotnet >>/tmp/dotnet.log 2>&1
echo "dotnet_install=$?"
tail -5 /tmp/apt.log /tmp/dotnet.log
node --version; npm --version; python3 -m pip --version; mvn -v | head -1; /opt/dotnet/dotnet --version
true
"#;

fn npm_script() -> String {
    format!(
        r#"rm -rf /tmp/np && mkdir -p /tmp/np && cd /tmp/np
printf 'registry=https://{NPM}/\n//{NPM}/:_authToken=${{NPM_FEED_TOKEN}}\n' > .npmrc
echo '{{"name":"consumer","version":"1.0.0"}}' > package.json
npm install {NPM_PACKAGE} --no-audit --no-fund --loglevel=http 2>&1 | tail -12
echo "tool=npm rc=${{PIPESTATUS[0]}} installed=$(test -f node_modules/{NPM_PACKAGE}/index.js && echo yes || echo no)"
"#
    )
}

/// npm with the token as a base64 `user:token` in `_auth` (the form some feeds' setup pages give)
/// instead of `_authToken`: the guest's own tool encodes the stand-in, and the proxy must decode,
/// swap and encode again.
fn npm_basic_script() -> String {
    format!(
        r#"rm -rf /tmp/npb && mkdir -p /tmp/npb && cd /tmp/npb
printf 'registry=https://{NPM}/\n//{NPM}/:_auth=%s\n' "$(printf 'feeduser:%s' "$NPM_FEED_TOKEN" | base64 -w0)" > .npmrc
echo '{{"name":"consumer","version":"1.0.0"}}' > package.json
npm install {NPM_PACKAGE} --no-audit --no-fund --loglevel=http 2>&1 | tail -12
echo "tool=npm-basic rc=${{PIPESTATUS[0]}} installed=$(test -f node_modules/{NPM_PACKAGE}/index.js && echo yes || echo no)"
"#
    )
}

/// The same installs with no credential at all: each must fail, or a pass above proves nothing.
fn control_script() -> String {
    format!(
        r#"rm -rf /tmp/nc /tmp/pc ~/.config/pip && mkdir -p /tmp/nc
cd /tmp/nc && printf 'registry=https://{NPM}/\n' > .npmrc && echo '{{"name":"c","version":"1.0.0"}}' > package.json
npm install {NPM_PACKAGE} --no-audit --no-fund 2>&1 | tail -3
echo "control=npm rc=${{PIPESTATUS[0]}}"
python3 -m pip download --no-deps --retries 0 -d /tmp/pc --index-url https://{PIP}/simple/ {PIP_PROJECT} 2>&1 | tail -3
echo "control=pip rc=${{PIPESTATUS[0]}}"
"#
    )
}

fn pip_script() -> String {
    // pip expands no variable in its config: the file holds the stand-in the shell put there.
    format!(
        r#"rm -rf /tmp/pipd ~/.config/pip && mkdir -p ~/.config/pip
cat > ~/.config/pip/pip.conf <<EOF
[global]
index-url = https://__token__:$PIP_FEED_TOKEN@{PIP}/simple/
retries = 0
EOF
python3 -m pip download --no-deps -d /tmp/pipd {PIP_PROJECT} 2>&1 | tail -12
echo "tool=pip rc=${{PIPESTATUS[0]}} installed=$(ls /tmp/pipd/*.whl >/dev/null 2>&1 && echo yes || echo no)"
"#
    )
}

fn nuget_script() -> String {
    let id = NUGET_PACKAGE.to_lowercase();
    format!(
        r#"rm -rf /tmp/nu ~/.nuget/packages/{id} && mkdir -p /tmp/nu && cd /tmp/nu
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1 DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1 PATH=/opt/dotnet:$PATH
cat > nuget.config <<'EOF'
<configuration>
  <packageSources><clear /><add key="private" value="https://{NUGET}/v3/index.json" /></packageSources>
  <packageSourceCredentials><private>
    <add key="Username" value="feeduser" />
    <add key="ClearTextPassword" value="%NUGET_FEED_TOKEN%" />
  </private></packageSourceCredentials>
</configuration>
EOF
cat > probe.csproj <<'EOF'
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup>
  <ItemGroup><PackageReference Include="{NUGET_PACKAGE}" Version="1.0.0" /></ItemGroup>
</Project>
EOF
dotnet restore 2>&1 | tail -12
echo "tool=nuget rc=${{PIPESTATUS[0]}} installed=$(test -d ~/.nuget/packages/{id}/1.0.0 && echo yes || echo no)"
"#
    )
}

/// `dotnet restore` with the Azure Artifacts credential provider and no credential in
/// `nuget.config`: the provider answers `NuGet`'s `401` from `ARTIFACTS_CREDENTIALPROVIDER_EXTERNAL_FEED_ENDPOINTS`,
/// a JSON document the shell builds around the value of the shell variable `credential_var`. `tag` names the folder and the report line.
fn provider_script(tag: &str, credential_var: &str) -> String {
    let id = NUGET_PACKAGE.to_lowercase();
    format!(
        r#"rm -rf /tmp/{tag} ~/.nuget/packages/{id} && mkdir -p /tmp/{tag} && cd /tmp/{tag}
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1 DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1 PATH=/opt/dotnet:$PATH
WRONG_FEED_TOKEN=$(printf 'not-the-%s' feed-token)
export NUGET_PLUGIN_PATHS=/root/.nuget/plugins/netcore/CredentialProvider.Microsoft/CredentialProvider.Microsoft.dll
export ARTIFACTS_CREDENTIALPROVIDER_EXTERNAL_FEED_ENDPOINTS='{{"endpointCredentials":[{{"endpoint":"https://{NUGET}/v3/index.json","username":"feeduser","password":"'"${credential_var}"'"}}]}}'
echo "endpoints-json-is-valid=$(printf %s "$ARTIFACTS_CREDENTIALPROVIDER_EXTERNAL_FEED_ENDPOINTS" | python3 -c 'import json,sys; json.load(sys.stdin); print("yes")' 2>&1)"
cat > nuget.config <<'EOF'
<configuration>
  <packageSources><clear /><add key="private" value="https://{NUGET}/v3/index.json" /></packageSources>
</configuration>
EOF
cat > probe.csproj <<'EOF'
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup>
  <ItemGroup><PackageReference Include="{NUGET_PACKAGE}" Version="1.0.0" /></ItemGroup>
</Project>
EOF
timeout 240 dotnet restore --no-cache 2>&1 | tail -12
echo "tool={tag} rc=${{PIPESTATUS[0]}} installed=$(test -d ~/.nuget/packages/{id}/1.0.0 && echo yes || echo no)"
"#
    )
}

/// Installs `dotnet` and the credential provider, not the other tools.
const PROVIDER_INSTALL: &str = r#"
export DEBIAN_FRONTEND=noninteractive
{ apt-get update -qq && apt-get install -y -qq --no-install-recommends python3 curl ca-certificates libicu-dev; } >/tmp/apt.log 2>&1
echo "apt=$?"
curl -fsSL https://dot.net/v1/dotnet-install.sh -o /tmp/dotnet-install.sh >/tmp/dotnet.log 2>&1 \
  && bash /tmp/dotnet-install.sh --channel 8.0 --install-dir /opt/dotnet >>/tmp/dotnet.log 2>&1
echo "dotnet_install=$?"
mkdir -p /root/.nuget && curl -fsSL PROVIDER_URL -o /tmp/provider.tar.gz >/tmp/provider.log 2>&1 \
  && tar -xzf /tmp/provider.tar.gz -C /root/.nuget >>/tmp/provider.log 2>&1
echo "provider_install=$?"
for f in /tmp/apt.log /tmp/dotnet.log /tmp/provider.log; do tail -n 5 $f; done
ls /root/.nuget/plugins/netcore/CredentialProvider.Microsoft/CredentialProvider.Microsoft.dll
true
"#;

fn maven_script() -> String {
    format!(
        r#"rm -rf /tmp/mv /tmp/m2 && mkdir -p /tmp/mv && cd /tmp/mv
cat > settings.xml <<'EOF'
<settings><servers><server><id>feed</id><username>feeduser</username><password>${{env.MAVEN_FEED_TOKEN}}</password></server></servers></settings>
EOF
mvn -B -q -s settings.xml -Dmaven.repo.local=/tmp/m2 org.apache.maven.plugins:maven-dependency-plugin:3.8.1:get \
  -Dartifact={MAVEN_COORDINATES} -DremoteRepositories=feed::default::https://{MAVEN}/m2 2>&1 | tail -12
echo "tool=maven rc=${{PIPESTATUS[0]}} installed=$(test -f /tmp/m2/com/example/probe-private/1.0/probe-private-1.0.jar && echo yes || echo no)"
"#
    )
}

/// `"first""second"`: the shell joins them, so the script text never holds the value.
fn split(value: &str) -> String {
    let (head, tail) = value.split_at(value.len() / 2);
    format!("\"{head}\"\"{tail}\"")
}

fn stand_in_of(host: &Host<MsbRuntime>, workspace: &WorkspaceId, var: &str) -> String {
    host.store()
        .env_for_start(workspace, &mut |_| Err("already made".into()))
        .unwrap()
        .into_iter()
        .find(|v| v.name.as_str() == var)
        .and_then(|v| match v.value {
            StartValue::Secret { stand_in, .. } => Some(stand_in),
            StartValue::Plain(_) => None,
        })
        .expect("the secret is among the start values")
}

async fn run(vm: &VmHost, workspace: &WorkspaceId, script: &str, minutes: u64) -> String {
    let wrapped = format!(
        "cat > /tmp/feed.sh <<'PUDDLE_EOF'\n{script}\nPUDDLE_EOF\nbash /tmp/feed.sh </dev/null 2>&1"
    );
    let out = guest_within(
        &vm.runtime,
        workspace,
        &wrapped,
        Duration::from_secs(minutes * 60),
    )
    .await;
    eprintln!("{out}");
    out
}

/// What the feed's log says of one tool: requests answered `200`, requests whose credential was
/// the real token, requests that carried a stand-in, requests refused with `401`.
struct Reach {
    ok: usize,
    real: usize,
    stand_in: usize,
    refused: usize,
}

fn reach(web: &puddle_e2e::lab::WebHost, host: &str, from: usize) -> Reach {
    let mut r = Reach {
        ok: 0,
        real: 0,
        stand_in: 0,
        refused: 0,
    };
    for seen in web.seen().iter().skip(from).filter(|s| {
        s.headers_named("host")
            .first()
            .is_some_and(|h| h.split(':').next() == Some(host))
    }) {
        let auth = seen.headers_named("authorization").join(" ");
        let decoded = auth
            .strip_prefix("Basic ")
            .and_then(|b| {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.decode(b).ok()
            })
            .map_or_else(
                || auth.clone(),
                |b| String::from_utf8_lossy(&b).into_owned(),
            );
        r.ok += usize::from(seen.status == 200);
        r.refused += usize::from(seen.status == 401);
        r.real += usize::from(decoded.contains(REAL));
        r.stand_in += usize::from(decoded.contains("puddle-secret-"));
    }
    r
}

/// A fake internet with the four private feeds, a workspace that may reach them and `extra`
/// sites, one secret per `(variable, feed host)` holding [`REAL`], and the workspace started.
async fn boot(secrets: &[(&str, &str)], extra: &[&str]) -> (WebInternet, VmHost, WorkspaceId) {
    let net = web_internet(
        &[NPM, NUGET, PIP, MAVEN],
        WebOptions {
            responder: Some(private_feeds(NPM, NUGET, PIP, MAVEN, REAL)),
            ..WebOptions::default()
        },
        named_id_options(),
    )
    .await;
    let vm = start_vm_host_platform("feed", net.options.clone(), &net.platform, |_| {}).await;
    let name = format!("{}-feed", vm.settings.prefix.as_str());
    let workspace = WorkspaceId::new(&name).unwrap();
    for site in [NPM, NUGET, PIP, MAVEN].iter().chain(extra) {
        vm.host
            .store()
            .as_user(puddle_store::UserId::OWNER)
            .add_rule(&NewRule {
                scope: Scope::Workspace(workspace.clone()),
                pattern: Pattern::parse(site).unwrap(),
                effect: Effect::Allow,
                expires_at: None,
                created_by: Actor::Cli,
            })
            .unwrap();
    }
    let api = Api::new(vm.host.url(), vm.host.token());
    for (var, host) in secrets {
        let reply = api
            .put(
                &format!("/api/env/{var}"),
                &json!({"kind": "secret", "value": REAL, "hosts": [host]}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "a feed secret was refused");
    }
    let mut events = api.events().await;
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "memory_mib": 3072}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let name_static: &'static str = Box::leak(name.into_boxed_str());
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    (net, vm, workspace)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_private_feeds_install_with_the_real_tools_and_a_secret_stand_in() {
    let (net, vm, workspace) = boot(
        &[
            ("NPM_FEED_TOKEN", NPM),
            ("NUGET_FEED_TOKEN", NUGET),
            ("PIP_FEED_TOKEN", PIP),
            ("MAVEN_FEED_TOKEN", MAVEN),
        ],
        MIRRORS,
    )
    .await;

    let installed = run(&vm, &workspace, INSTALL, 25).await;
    assert!(
        installed.contains("apt=0") && installed.contains("dotnet_install=0"),
        "the tools could not be installed (live third parties: the Debian mirrors, Microsoft's dotnet-install; an outage there fails this test): {installed}"
    );

    let mut report = Vec::new();
    for (tool, host, script) in [
        ("npm", NPM, npm_script()),
        ("npm-basic", NPM, npm_basic_script()),
        ("pip", PIP, pip_script()),
        ("nuget", NUGET, nuget_script()),
        ("maven", MAVEN, maven_script()),
    ] {
        let from = net.web.seen().len();
        let out = run(&vm, &workspace, &script, 15).await;
        let worked = out.contains(&format!("tool={tool} rc=0 installed=yes"));
        let r = reach(&net.web, host, from);
        report.push((tool, worked, r));
    }

    let control = run(&vm, &workspace, &control_script(), 5).await;
    for tool in ["npm", "pip"] {
        assert!(
            control.contains(&format!("control={tool} rc="))
                && !control.contains(&format!("control={tool} rc=0")),
            "{tool} installed from the feed with no credential, so a pass above proves nothing: {control}"
        );
    }

    // The real token is nowhere in the guest: no file, no process environment. The tools' own
    // config and caches are included (they live under /root and /tmp).
    let real = split(REAL);
    let search = run(
        &vm,
        &workspace,
        &format!(
            "files=$(grep -rla -e {real} / --exclude-dir=proc --exclude-dir=sys --exclude-dir=dev 2>/dev/null | grep -v '^/tmp/feed.sh$'); \
             live=$(cat /proc/[0-9]*/environ 2>/dev/null | tr '\\0' '\\n' | grep -c -e {real}); \
             echo \"files=[$files] processes=[$live]\""
        ),
        10,
    )
    .await;
    let stand_ins: Vec<String> = ["NPM", "NUGET", "PIP", "MAVEN"]
        .iter()
        .map(|t| stand_in_of(&vm.host, &workspace, &format!("{t}_FEED_TOKEN")))
        .collect();
    let on_disk = run(
        &vm,
        &workspace,
        "grep -rl 'puddle-secret-' /root/.npmrc /root/.config /root/.nuget /root/.m2 /tmp/np /tmp/nu /tmp/mv 2>/dev/null; true",
        2,
    )
    .await;

    eprintln!("tool    installed  feed-200  real-token  unswapped-stand-in  refused");
    for (tool, worked, r) in &report {
        eprintln!(
            "{tool:<7} {worked:<10} {:<9} {:<11} {:<19} {}",
            r.ok, r.real, r.stand_in, r.refused
        );
    }
    eprintln!("stand-ins held by the guest: {stand_ins:?}");
    eprintln!("files with a stand-in (config and caches): {on_disk}");
    eprintln!("search for the real token: {search}");

    let mut failures = Vec::new();
    for (tool, worked, r) in &report {
        if !worked {
            failures.push(format!("{tool}: the install did not work"));
        }
        if r.real == 0 {
            failures.push(format!("{tool}: the feed never received the real token"));
        }
        if r.stand_in > 0 {
            failures.push(format!(
                "{tool}: {} request(s) reached the feed with the stand-in",
                r.stand_in
            ));
        }
    }
    if search.trim() != "files=[] processes=[0]" {
        failures.push(format!("the real token is in the guest: {search}"));
    }
    assert!(failures.is_empty(), "private feeds: {failures:#?}");

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// The Azure Artifacts credential provider reads `ARTIFACTS_CREDENTIALPROVIDER_EXTERNAL_FEED_ENDPOINTS`, a
/// JSON document, and hands the credential in it to `NuGet`. The guest's variable holds the
/// secret's stand-in inside a JSON string; the test says whether the provider passes it on
/// byte for byte, so the proxy's swap applies, and that a wrong password in the same JSON fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_artifacts_credential_provider_hands_the_stand_in_to_nuget_unchanged() {
    let sites: Vec<&str> = MIRRORS.iter().chain(PROVIDER_HOSTS).copied().collect();
    let (net, vm, workspace) = boot(&[("NUGET_FEED_TOKEN", NUGET)], &sites).await;

    let installed = run(
        &vm,
        &workspace,
        &PROVIDER_INSTALL.replace("PROVIDER_URL", PROVIDER_URL),
        25,
    )
    .await;
    assert!(
        installed.contains("apt=0")
            && installed.contains("dotnet_install=0")
            && installed.contains("provider_install=0"),
        "the tools could not be installed (live third parties: the Debian mirrors, Microsoft's dotnet-install, GitHub releases): {installed}"
    );

    let from = net.web.seen().len();
    let out = run(
        &vm,
        &workspace,
        &provider_script("provider", "NUGET_FEED_TOKEN"),
        15,
    )
    .await;
    let worked = out.contains("tool=provider rc=0 installed=yes");
    let valid_json = out.contains("endpoints-json-is-valid=yes");
    let r = reach(&net.web, NUGET, from);

    let from = net.web.seen().len();
    let control = run(
        &vm,
        &workspace,
        &provider_script("provider-control", "WRONG_FEED_TOKEN"),
        5,
    )
    .await;
    let control_reach = reach(&net.web, NUGET, from);

    let stand_in = stand_in_of(&vm.host, &workspace, "NUGET_FEED_TOKEN");
    eprintln!(
        "provider: installed={worked} json={valid_json} feed-200={} real-token={} unswapped-stand-in={} refused={}",
        r.ok, r.real, r.stand_in, r.refused
    );
    eprintln!(
        "control (wrong password): feed-200={} refused={} stand-in held by the guest: {stand_in}",
        control_reach.ok, control_reach.refused
    );

    let mut failures = Vec::new();
    if !valid_json {
        failures.push("the guest's endpoints variable is not valid JSON".to_owned());
    }
    if !worked {
        failures.push("the restore through the credential provider did not work".to_owned());
    }
    if r.real == 0 {
        failures.push("the feed never received the real token".to_owned());
    }
    if r.stand_in > 0 {
        failures.push(format!(
            "{} request(s) reached the feed with the stand-in",
            r.stand_in
        ));
    }
    if control.contains("tool=provider-control rc=0") || control_reach.real > 0 {
        failures.push(format!(
            "a wrong password in the same JSON still installed, so a pass proves nothing: {control}"
        ));
    }
    assert!(
        failures.is_empty(),
        "azure artifacts credential provider: {failures:#?}"
    );

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
