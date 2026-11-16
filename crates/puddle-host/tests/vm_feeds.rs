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
//! A publish test (`vm_radar_publish_*`) has `npm publish` and `mvn deploy` upload a package to a
//! feed that answers `401` to an upload without its token: the stand-in is in the header and the
//! body is a request body upload the proxy forwards untouched.
//!
//! A further test (`vm_radar_token_placements_*`) covers clients that put the token somewhere other
//! than a header: the query string, the URL path, `.netrc`, a `cargo` registry token, and `pnpm`,
//! `yarn`, Gradle and `go`.
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
    CARGO_CRATE, GO_MODULE, MAVEN_COORDINATES, MAVEN_PUBLISH_DIR, NPM_PACKAGE, NPM_PUBLISH_PACKAGE,
    NUGET_PACKAGE, PIP_PROJECT, PlacementHosts, WebOptions, placement_feeds, private_feeds,
    wheel_file,
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
const PATH_HOST: &str = "path.feeds.invalid";
const QUERY_HOST: &str = "query.feeds.invalid";
const CARGO_HOST: &str = "cargo.feeds.invalid";
const GO_HOST: &str = "go.feeds.invalid";
const PLACEMENT_HOSTS: PlacementHosts = PlacementHosts {
    path: PATH_HOST,
    query: QUERY_HOST,
    cargo: CARGO_HOST,
    go: GO_HOST,
};
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
    let id = NUGET_PACKAGE.to_lowercase();
    let nuget_control = format!(
        r#"rm -rf /tmp/nuc ~/.nuget/packages/{id} && mkdir -p /tmp/nuc && cd /tmp/nuc
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1 DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1 PATH=/opt/dotnet:$PATH
cat > nuget.config <<'EOF'
<configuration><packageSources><clear /><add key="private" value="https://{NUGET}/v3/index.json" /></packageSources></configuration>
EOF
cat > probe.csproj <<'EOF'
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup>
  <ItemGroup><PackageReference Include="{NUGET_PACKAGE}" Version="1.0.0" /></ItemGroup>
</Project>
EOF
# NuGet keeps what the authenticated installs fetched in its http cache: without these the control restores from it.
rm -rf ~/.local/share/NuGet/http-cache
dotnet restore --no-cache 2>&1 | tail -3
echo "control=nuget rc=${{PIPESTATUS[0]}}"
"#
    );
    let maven_control = format!(
        r#"rm -rf /tmp/mvc /tmp/m2c && mkdir -p /tmp/mvc && cd /tmp/mvc
mvn -B -q -Dmaven.repo.local=/tmp/m2c org.apache.maven.plugins:maven-dependency-plugin:3.8.1:get \
  -Dartifact={MAVEN_COORDINATES} -DremoteRepositories=feed::default::https://{MAVEN}/m2 2>&1 | tail -3
echo "control=maven rc=${{PIPESTATUS[0]}}"
"#
    );
    format!(
        r#"rm -rf /tmp/nc /tmp/pc ~/.config/pip && mkdir -p /tmp/nc
cd /tmp/nc && printf 'registry=https://{NPM}/\n' > .npmrc && echo '{{"name":"c","version":"1.0.0"}}' > package.json
npm install {NPM_PACKAGE} --no-audit --no-fund 2>&1 | tail -3
echo "control=npm rc=${{PIPESTATUS[0]}}"
python3 -m pip download --no-deps --retries 0 -d /tmp/pc --index-url https://{PIP}/simple/ {PIP_PROJECT} 2>&1 | tail -3
echo "control=pip rc=${{PIPESTATUS[0]}}"
{nuget_control}{maven_control}"#
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

/// `files=[..] processes=[n]`: the guest files, and process environments, that hold [`REAL`].
async fn search_for_real_token(vm: &VmHost, workspace: &WorkspaceId) -> String {
    let real = split(REAL);
    run(
        vm,
        workspace,
        &format!(
            "files=$(grep -rla -e {real} / --exclude-dir=proc --exclude-dir=sys --exclude-dir=dev 2>/dev/null | grep -v '^/tmp/feed.sh$'); \
             live=$(cat /proc/[0-9]*/environ 2>/dev/null | tr '\\0' '\\n' | grep -c -e {real}); \
             echo \"files=[$files] processes=[$live]\""
        ),
        10,
    )
    .await
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

/// What the feed's log says of one tool: requests answered `200`, requests whose credential (a
/// header, or the request target) was the real token, requests that carried a stand-in, requests
/// refused with `401`.
struct Reach {
    ok: usize,
    real: usize,
    stand_in: usize,
    refused: usize,
}

/// The credential a request carried (the `authorization` value, basic authentication decoded)
/// and its request target.
fn carried(seen: &puddle_e2e::lab::Seen) -> String {
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
    format!("{decoded} {}", seen.target)
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
        r.ok += usize::from(seen.status == 200);
        r.refused += usize::from(seen.status == 401);
        let carried = carried(seen);
        r.real += usize::from(carried.contains(REAL));
        r.stand_in += usize::from(carried.contains("puddle-secret-"));
    }
    r
}

/// A fake internet with the four private feeds, a workspace that may reach them and `extra`
/// sites, one secret per `(variable, feed host)` holding [`REAL`], and the workspace started.
/// `tag` names the scratch home and the workspace: one per test, a second claim fails.
async fn boot(
    tag: &str,
    secrets: &[(&str, &str)],
    extra: &[&str],
) -> (WebInternet, VmHost, WorkspaceId) {
    let header = json!({"in": "header"});
    let placed: Vec<(&str, &str, &serde_json::Value)> =
        secrets.iter().map(|(v, h)| (*v, *h, &header)).collect();
    boot_placed(tag, &placed, extra, false).await
}

/// [`boot`] with a place per secret (the `place` of the environment API), and, when
/// `placements`, the four feeds of [`placement_feeds`] as well.
async fn boot_placed(
    tag: &str,
    secrets: &[(&str, &str, &serde_json::Value)],
    extra: &[&str],
    placements: bool,
) -> (WebInternet, VmHost, WorkspaceId) {
    let mut hosts = vec![NPM, NUGET, PIP, MAVEN];
    let feeds = private_feeds(NPM, NUGET, PIP, MAVEN, REAL);
    let responder: puddle_e2e::lab::Responder = if placements {
        hosts.extend([PATH_HOST, QUERY_HOST, CARGO_HOST, GO_HOST]);
        let more = placement_feeds(PLACEMENT_HOSTS, REAL);
        std::sync::Arc::new(move |seen| feeds(seen).or_else(|| more(seen)))
    } else {
        feeds
    };
    let net = web_internet(
        &hosts,
        WebOptions {
            responder: Some(responder),
            ..WebOptions::default()
        },
        named_id_options(),
    )
    .await;
    let vm = start_vm_host_platform(tag, net.options.clone(), &net.platform, |_| {}).await;
    let name = format!("{}-{tag}", vm.settings.prefix.as_str());
    let workspace = WorkspaceId::new(&name).unwrap();
    for site in hosts.iter().chain(extra) {
        puddle_store::testing::own(vm.host.store(), &workspace);
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
    for (var, host, place) in secrets {
        let reply = api
            .put(
                &format!("/api/env/{var}"),
                &json!({"kind": "secret", "value": REAL, "hosts": [host], "place": place})
                    .to_string(),
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
        "feed",
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
    for tool in ["npm", "pip", "nuget", "maven"] {
        assert!(
            control.contains(&format!("control={tool} rc="))
                && !control.contains(&format!("control={tool} rc=0")),
            "{tool} installed from the feed with no credential, so a pass above proves nothing: {control}"
        );
    }

    // The real token is nowhere in the guest: no file, no process environment. The tools' own
    // config and caches are included (they live under /root and /tmp).
    let search = search_for_real_token(&vm, &workspace).await;
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
    let (net, vm, workspace) = boot("feed-nuget", &[("NUGET_FEED_TOKEN", NUGET)], &sites).await;

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

/// Installs `npm` and Maven with a JDK (`jar` builds the Maven upload). No `dotnet`, no `pip`.
const PUBLISH_INSTALL: &str = r#"
export DEBIAN_FRONTEND=noninteractive
{ apt-get update -qq && apt-get install -y -qq --no-install-recommends nodejs npm maven default-jdk-headless ca-certificates-java curl ca-certificates; } >/tmp/apt.log 2>&1
echo "apt=$?"
tail -5 /tmp/apt.log
node --version; npm --version; mvn -v | head -1
true
"#;

/// `npm publish` to the feed, the token in `.npmrc` as the stand-in's variable (`with_credential`)
/// or absent (the control).
fn npm_publish_script(tag: &str, with_credential: bool) -> String {
    let auth = if with_credential {
        format!("//{NPM}/:_authToken=${{NPM_FEED_TOKEN}}\\n")
    } else {
        String::new()
    };
    format!(
        r#"rm -rf /tmp/npp-{tag} && mkdir -p /tmp/npp-{tag} && cd /tmp/npp-{tag}
printf 'registry=https://{NPM}/\n{auth}' > .npmrc
echo '{{"name":"{NPM_PUBLISH_PACKAGE}","version":"1.0.0","main":"index.js"}}' > package.json
echo "module.exports = 'published';" > index.js
npm publish --loglevel=http 2>&1 | tail -12
echo "tool={tag} rc=${{PIPESTATUS[0]}}"
"#
    )
}

/// `mvn deploy:deploy-file` of a small jar to the feed, the server entry (id `feed`) holding the
/// stand-in's variable (`with_credential`) or absent (the control).
fn maven_deploy_script(tag: &str, with_credential: bool) -> String {
    let servers = if with_credential {
        "<server><id>feed</id><username>feeduser</username><password>${env.MAVEN_FEED_TOKEN}</password></server>"
    } else {
        ""
    };
    let artifact = MAVEN_PUBLISH_DIR
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    format!(
        r#"rm -rf /tmp/mp-{tag} /tmp/m2-{tag} && mkdir -p /tmp/mp-{tag}/c && cd /tmp/mp-{tag}
printf '%s\n' '<settings><servers>{servers}</servers></settings>' > settings.xml
echo published > c/a.txt && jar cf probe.jar -C c .
mvn -B -q -s settings.xml -Dmaven.repo.local=/tmp/m2-{tag} org.apache.maven.plugins:maven-deploy-plugin:3.1.3:deploy-file \
  -Dfile=probe.jar -DgroupId=com.example -DartifactId={artifact} -Dversion=1.0 -Dpackaging=jar \
  -DrepositoryId=feed -Durl=https://{MAVEN}/m2 2>&1 | tail -12
echo "tool={tag} rc=${{PIPESTATUS[0]}}"
"#
    )
}

/// What the feed's log says of the uploads (`PUT`) one tool made: the paths accepted (`201`),
/// those refused (`401`), those whose credential was the real token or a stand-in, and the
/// uploads whose body held a token in any form.
struct Uploads {
    accepted: Vec<String>,
    refused: usize,
    real: usize,
    stand_in: usize,
    body_bytes: usize,
    token_in_body: usize,
}

fn uploads(web: &puddle_e2e::lab::WebHost, host: &str, from: usize) -> Uploads {
    let mut u = Uploads {
        accepted: Vec::new(),
        refused: 0,
        real: 0,
        stand_in: 0,
        body_bytes: 0,
        token_in_body: 0,
    };
    for seen in web.seen().iter().skip(from).filter(|s| {
        s.method == "PUT"
            && s.headers_named("host")
                .first()
                .is_some_and(|h| h.split(':').next() == Some(host))
    }) {
        if seen.status == 201 {
            u.accepted
                .push(seen.target.split('?').next().unwrap_or_default().to_owned());
            u.body_bytes += seen.body.len();
        }
        u.refused += usize::from(seen.status == 401);
        let credential = carried(seen);
        u.real += usize::from(credential.contains(REAL));
        u.stand_in += usize::from(credential.contains("puddle-secret-"));
        let body = String::from_utf8_lossy(&seen.body);
        u.token_in_body += usize::from(body.contains(REAL) || body.contains("puddle-secret-"));
    }
    u
}

type PublishRow = (&'static str, bool, Uploads);
type ControlRow = (&'static str, String, Uploads);

/// What is wrong with the publish test's measurements, one line each.
fn publish_failures(report: &[PublishRow], controls: &[ControlRow], search: &str) -> Vec<String> {
    let mut failures = Vec::new();
    for (tool, worked, u) in report {
        if !worked {
            failures.push(format!("{tool}: the upload did not work"));
        }
        if u.accepted.is_empty() || u.real == 0 {
            failures.push(format!(
                "{tool}: the feed accepted no upload with the real token"
            ));
        }
        if u.body_bytes == 0 {
            failures.push(format!("{tool}: the accepted upload had no request body"));
        }
        if u.stand_in > 0 || u.token_in_body > 0 {
            failures.push(format!(
                "{tool}: a stand-in or token reached the feed unswapped (header {}, body {})",
                u.stand_in, u.token_in_body
            ));
        }
    }
    let npm = &report[0].2;
    if !npm
        .accepted
        .iter()
        .any(|p| p == &format!("/{NPM_PUBLISH_PACKAGE}"))
    {
        failures.push(format!("npm: wrong upload path {:?}", npm.accepted));
    }
    let maven = &report[1].2;
    for ext in [".jar", ".pom"] {
        if !maven
            .accepted
            .iter()
            .any(|p| p.starts_with(MAVEN_PUBLISH_DIR) && p.ends_with(ext))
        {
            failures.push(format!("maven: no {ext} upload in {:?}", maven.accepted));
        }
    }
    for (tool, out, u) in controls {
        if out.contains(&format!("tool={tool} rc=0")) || !u.accepted.is_empty() || u.refused == 0 {
            failures.push(format!(
                "{tool}: an upload with no credential was not refused, so a pass above proves nothing: {out}"
            ));
        }
    }
    if search.trim() != "files=[] processes=[0]" {
        failures.push(format!("the real token is in the guest: {search}"));
    }
    failures
}

/// `npm publish` and `mvn deploy` (the deploy plugin's `deploy-file`) upload a package to a feed
/// that answers `401` to an upload without the token. The guest's config names the stand-in; the
/// test says, per tool: did the upload work, did the feed accept a request body upload that
/// carried the real token, did a stand-in or token reach the feed unswapped (header or body), and
/// does the same upload with no credential fail. The real token must be nowhere in the guest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_publish_to_a_private_feed_carries_the_real_token_in_the_header() {
    let (net, vm, workspace) = boot(
        "feed-publish",
        &[("NPM_FEED_TOKEN", NPM), ("MAVEN_FEED_TOKEN", MAVEN)],
        MIRRORS,
    )
    .await;

    let installed = run(&vm, &workspace, PUBLISH_INSTALL, 20).await;
    assert!(
        installed.contains("apt=0"),
        "the tools could not be installed (live third party: the Debian mirrors; an outage there fails this test): {installed}"
    );

    let mut report = Vec::new();
    for (tool, host, script) in [
        ("npm-publish", NPM, npm_publish_script("npm-publish", true)),
        (
            "maven-deploy",
            MAVEN,
            maven_deploy_script("maven-deploy", true),
        ),
    ] {
        let from = net.web.seen().len();
        let out = run(&vm, &workspace, &script, 15).await;
        let worked = out.contains(&format!("tool={tool} rc=0"));
        report.push((tool, worked, uploads(&net.web, host, from)));
    }

    let mut controls = Vec::new();
    for (tool, host, script) in [
        ("npm-control", NPM, npm_publish_script("npm-control", false)),
        (
            "maven-control",
            MAVEN,
            maven_deploy_script("maven-control", false),
        ),
    ] {
        let from = net.web.seen().len();
        let out = run(&vm, &workspace, &script, 10).await;
        controls.push((tool, out, uploads(&net.web, host, from)));
    }

    let search = search_for_real_token(&vm, &workspace).await;

    eprintln!(
        "tool          worked  accepted  real-token  stand-in  token-in-body  body-bytes  refused"
    );
    for (tool, worked, u) in &report {
        eprintln!(
            "{tool:<13} {worked:<7} {:<9} {:<11} {:<9} {:<14} {:<11} {}",
            u.accepted.len(),
            u.real,
            u.stand_in,
            u.token_in_body,
            u.body_bytes,
            u.refused
        );
        eprintln!("  accepted paths: {:?}", u.accepted);
    }
    for (tool, _, u) in &controls {
        eprintln!(
            "{tool}: accepted={} refused={}",
            u.accepted.len(),
            u.refused
        );
    }
    eprintln!("search for the real token: {search}");

    let failures = publish_failures(&report, &controls, &search);
    assert!(failures.is_empty(), "publish: {failures:#?}");

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// Hosts the placement test's tool installs reach, on top of [`MIRRORS`]: the npm registry (for
/// `pnpm` and `yarn`), Gradle's distribution and where it redirects to.
const PLACEMENT_SITES: &[&str] = &[
    "registry.npmjs.org",
    "services.gradle.org",
    "downloads.gradle.org",
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// Installs `cargo`, `go`, Java, `pnpm`, `yarn` (1.x and 4.x) and Gradle.
const PLACEMENT_INSTALL: &str = r#"
export DEBIAN_FRONTEND=noninteractive
{ apt-get update -qq && apt-get install -y -qq --no-install-recommends nodejs npm python3 python3-pip curl ca-certificates cargo golang-go default-jdk-headless ca-certificates-java unzip; } >/tmp/apt.log 2>&1
echo "apt=$?"
{ npm install -g pnpm@9.15.0 yarn@1.22.22 && npm install -g --prefix /opt/yarn4 @yarnpkg/cli-dist@4.5.3; } >/tmp/npm-global.log 2>&1
echo "npm_global=$?"
{ curl -fsSL https://services.gradle.org/distributions/gradle-8.10.2-bin.zip -o /tmp/gradle.zip && unzip -q /tmp/gradle.zip -d /opt; } >/tmp/gradle-install.log 2>&1
echo "gradle_install=$?"
for f in /tmp/apt.log /tmp/npm-global.log /tmp/gradle-install.log; do tail -n 4 $f; done
grep PRETTY /etc/os-release; cargo --version; go version; pnpm --version; yarn --version; /opt/yarn4/bin/yarn --version; /opt/gradle-8.10.2/bin/gradle --version | grep -m1 Gradle
true
"#;

/// Fills the `@NAME@` marks of a guest script. The scripts hold shell and TOML braces, so they
/// are not `format!` strings.
fn fill(script: &str) -> String {
    let wheel = wheel_file();
    [
        ("@NPM@", NPM),
        ("@PIPPKG@", PIP_PROJECT),
        ("@PIP@", PIP),
        ("@MAVEN@", MAVEN),
        ("@PATH@", PATH_HOST),
        ("@QUERY@", QUERY_HOST),
        ("@CARGO@", CARGO_HOST),
        ("@GO@", GO_HOST),
        ("@PKG@", NPM_PACKAGE),
        ("@CRATE@", CARGO_CRATE),
        ("@MODULE@", GO_MODULE),
        ("@COORDINATES@", MAVEN_COORDINATES),
        ("@WHEEL@", &wheel),
    ]
    .iter()
    .fold(script.to_owned(), |text, (mark, value)| {
        text.replace(mark, value)
    })
}

/// pnpm with the token in `.npmrc` as an environment reference, like npm.
const PNPM: &str = r#"rm -rf /tmp/pnpm-t && mkdir -p /tmp/pnpm-t && cd /tmp/pnpm-t
printf 'registry=https://@NPM@/\n//@NPM@/:_authToken=${NPM_FEED_TOKEN}\n' > .npmrc
echo '{"name":"consumer","version":"1.0.0"}' > package.json
CI=1 pnpm add @PKG@ --reporter=append-only 2>&1 | tail -12
echo "tool=pnpm rc=${PIPESTATUS[0]} installed=$(test -f node_modules/@PKG@/index.js && echo yes || echo no)"
"#;

/// yarn 1.x: registry in `.yarnrc`, the token in `.npmrc` (the shell expands the variable).
const YARN_CLASSIC: &str = r#"rm -rf /tmp/yarn1-t && mkdir -p /tmp/yarn1-t && cd /tmp/yarn1-t
printf 'registry "https://@NPM@"\n' > .yarnrc
printf '//@NPM@/:_authToken=%s\nalways-auth=true\n' "$NPM_FEED_TOKEN" > .npmrc
echo '{"name":"consumer","version":"1.0.0"}' > package.json
yarn add @PKG@ --non-interactive 2>&1 | tail -12
echo "tool=yarn1 rc=${PIPESTATUS[0]} installed=$(test -f node_modules/@PKG@/index.js && echo yes || echo no)"
"#;

/// yarn 4.x: `npmAuthToken` in `.yarnrc.yml` with an environment reference.
const YARN_BERRY: &str = r#"rm -rf /tmp/yarn4-t && mkdir -p /tmp/yarn4-t && cd /tmp/yarn4-t
cat > .yarnrc.yml <<'RC_END'
nodeLinker: node-modules
enableTelemetry: false
npmRegistryServer: "https://@NPM@"
npmAlwaysAuth: true
npmAuthToken: "${NPM_FEED_TOKEN}"
RC_END
echo '{"name":"consumer","version":"1.0.0"}' > package.json
touch yarn.lock
/opt/yarn4/bin/yarn add @PKG@ 2>&1 | tail -12
echo "tool=yarn4 rc=${PIPESTATUS[0]} installed=$(test -f node_modules/@PKG@/index.js && echo yes || echo no)"
"#;

/// Gradle with `credentials` in a repository block, the password read from the environment.
const GRADLE: &str = r#"rm -rf /tmp/gradle-t && mkdir -p /tmp/gradle-t && cd /tmp/gradle-t
touch settings.gradle
cat > build.gradle <<'RC_END'
repositories {
  maven {
    url = uri('https://@MAVEN@/m2')
    credentials { username = 'feeduser'; password = System.getenv('MAVEN_FEED_TOKEN') }
  }
}
configurations { probe }
dependencies { probe '@COORDINATES@' }
tasks.register('fetch') { doLast { configurations.probe.files.each { println "resolved=${it.name}" } } }
RC_END
export GRADLE_USER_HOME=/tmp/gradle-t/home
timeout 600 /opt/gradle-8.10.2/bin/gradle --no-daemon --console=plain -q -Dorg.gradle.jvmargs=-Xmx768m fetch 2>&1 | tail -12
echo "tool=gradle rc=${PIPESTATUS[0]} installed=$(find /tmp/gradle-t/home -name 'probe-private-1.0.jar' | grep -q . && echo yes || echo no)"
"#;

/// `cargo` with a registry token in the environment (`CARGO_REGISTRIES_FEED_TOKEN`): `cargo`
/// sends it bare as the whole `authorization` value.
const CARGO: &str = r#"rm -rf /tmp/cargo-t && mkdir -p /tmp/cargo-t/.cargo /tmp/cargo-t/src && cd /tmp/cargo-t
cat > .cargo/config.toml <<'RC_END'
[registries.feed]
index = "sparse+https://@CARGO@/"
[registry]
global-credential-providers = ["cargo:token"]
RC_END
cat > Cargo.toml <<'RC_END'
[package]
name = "consumer"
version = "0.1.0"
edition = "2021"
[dependencies]
@CRATE@ = { version = "1", registry = "feed" }
RC_END
touch src/lib.rs
export CARGO_HOME=/tmp/cargo-t/home
CARGO_REGISTRIES_FEED_TOKEN="$CARGO_FEED_TOKEN" cargo fetch 2>&1 | tail -12
echo "tool=cargo rc=${PIPESTATUS[0]} installed=$(ls /tmp/cargo-t/home/registry/src/*/@CRATE@-1.0.0/Cargo.toml >/dev/null 2>&1 && echo yes || echo no)"
"#;

/// `go get` against a module proxy; `@SETUP@` sets `GOPROXY` (with user information, or plain and
/// `.netrc` holding the credential).
const GO: &str = r#"rm -rf /tmp/go-@TAG@ && mkdir -p /tmp/go-@TAG@/p && cd /tmp/go-@TAG@/p
export GOPATH=/tmp/go-@TAG@/path GOCACHE=/tmp/go-@TAG@/cache GOSUMDB=off GOFLAGS=-mod=mod GOTOOLCHAIN=local
@SETUP@
go mod init consumer >/dev/null 2>&1
go get @MODULE@@v1.0.0 2>&1 | tail -12
echo "tool=go-@TAG@ rc=${PIPESTATUS[0]} installed=$(test -f $GOPATH/pkg/mod/@MODULE@@v1.0.0/go.mod && echo yes || echo no)"
rm -f ~/.netrc
"#;

fn go_script(tag: &str, setup: &str) -> String {
    fill(&GO.replace("@TAG@", tag).replace("@SETUP@", setup))
}

/// `.netrc` with the stand-in, and a plain address.
const NETRC_PIP: &str = r#"rm -rf /tmp/pip-netrc && printf 'machine @PIP@ login feeduser password %s\n' "$PIP_FEED_TOKEN" > ~/.netrc && chmod 600 ~/.netrc
python3 -m pip download --no-deps --retries 0 -d /tmp/pip-netrc --index-url https://@PIP@/simple/ @PIPPKG@ 2>&1 | tail -12
echo "tool=pip-netrc rc=${PIPESTATUS[0]} installed=$(ls /tmp/pip-netrc/*.whl >/dev/null 2>&1 && echo yes || echo no)"
rm -f ~/.netrc
"#;

/// `curl --netrc`.
const NETRC_CURL: &str = r#"rm -f /tmp/netrc.whl && printf 'machine @PIP@ login feeduser password %s\n' "$PIP_FEED_TOKEN" > ~/.netrc && chmod 600 ~/.netrc
curl -fsS --netrc -o /tmp/netrc.whl https://@PIP@/files/@WHEEL@ 2>&1 | tail -3
echo "tool=curl-netrc rc=${PIPESTATUS[0]} installed=$(test -s /tmp/netrc.whl && echo yes || echo no)"
rm -f ~/.netrc
"#;

/// A direct download link with the token in the query, fetched by `curl` and by `pip` (which
/// installs from a URL as it is). `@VAR@` is the secret's variable and `@PARAM@` the parameter
/// name the client uses.
const QUERY: &str = r#"rm -rf /tmp/q-@TAG@ && mkdir -p /tmp/q-@TAG@ && cd /tmp/q-@TAG@
curl -fsS -o curl.whl "https://@QUERY@/files/@WHEEL@?@PARAM@=$@VAR@" 2>&1 | tail -3
echo "tool=query-curl-@TAG@ rc=${PIPESTATUS[0]} installed=$(test -s curl.whl && echo yes || echo no)"
python3 -m pip download --no-deps --retries 0 -d pip "https://@QUERY@/files/@WHEEL@?@PARAM@=$@VAR@" 2>&1 | tail -3
echo "tool=query-pip-@TAG@ rc=${PIPESTATUS[0]} installed=$(ls pip/*.whl >/dev/null 2>&1 && echo yes || echo no)"
"#;

fn query_script(tag: &str, var: &str, param: &str) -> String {
    fill(
        &QUERY
            .replace("@TAG@", tag)
            .replace("@VAR@", var)
            .replace("@PARAM@", param),
    )
}

/// npm with the token in the registry's path (`registry=https://host/t/<token>/`), no header.
const PATH: &str = r#"rm -rf /tmp/path-@TAG@ && mkdir -p /tmp/path-@TAG@ && cd /tmp/path-@TAG@
printf 'registry=https://@PATH@/t/%s/\n' "$@VAR@" > .npmrc
echo '{"name":"consumer","version":"1.0.0"}' > package.json
npm install @PKG@ --no-audit --no-fund --loglevel=http 2>&1 | tail -12
echo "tool=path-@TAG@ rc=${PIPESTATUS[0]} installed=$(test -f node_modules/@PKG@/index.js && echo yes || echo no)"
"#;

fn path_script(tag: &str, var: &str) -> String {
    fill(&PATH.replace("@TAG@", tag).replace("@VAR@", var))
}

/// The same fetches with no credential: each must fail, or a pass above proves nothing.
const PLACEMENT_CONTROL: &str = r#"rm -rf /tmp/c-q /tmp/c-p /tmp/c-c /tmp/c-g /tmp/c-pn && mkdir -p /tmp/c-q /tmp/c-p /tmp/c-c/src /tmp/c-c/.cargo /tmp/c-g/p /tmp/c-pn
curl -fsS -o /tmp/c-q/x.whl "https://@QUERY@/files/@WHEEL@" >/dev/null 2>&1
echo "control=query rc=$?"
cd /tmp/c-p && printf 'registry=https://@PATH@/\n' > .npmrc && echo '{"name":"c","version":"1.0.0"}' > package.json
npm install @PKG@ --no-audit --no-fund >/dev/null 2>&1
echo "control=path rc=$?"
cd /tmp/c-pn && printf 'registry=https://@NPM@/\n' > .npmrc && echo '{"name":"c","version":"1.0.0"}' > package.json
CI=1 pnpm add @PKG@ --reporter=silent --store-dir /tmp/c-pn/store --cache-dir /tmp/c-pn/cache >/dev/null 2>&1
echo "control=pnpm rc=$?"
cd /tmp/c-c && printf '[registries.feed]\nindex = "sparse+https://@CARGO@/"\n[registry]\nglobal-credential-providers = ["cargo:token"]\n' > .cargo/config.toml
printf '[package]\nname = "c"\nversion = "0.1.0"\nedition = "2021"\n[dependencies]\n@CRATE@ = { version = "1", registry = "feed" }\n' > Cargo.toml
touch src/lib.rs
CARGO_HOME=/tmp/c-c/home cargo fetch >/dev/null 2>&1
echo "control=cargo rc=$?"
cd /tmp/c-g/p && GOPATH=/tmp/c-g/path GOCACHE=/tmp/c-g/cache GOSUMDB=off GOFLAGS=-mod=mod GOTOOLCHAIN=local GOPROXY=https://@GO@ sh -c 'go mod init c >/dev/null 2>&1; go get @MODULE@@v1.0.0' >/dev/null 2>&1
echo "control=go rc=$?"
"#;

/// What a case is expected to show at the feed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Expect {
    /// The install works; the feed got the real token and never a stand-in.
    Swapped,
    /// The install fails; the feed got the stand-in and never the real token. The text says why.
    Leaks(&'static str),
}

/// The cases of the placement test: tool, feed host, guest script, expectation.
fn placement_cases() -> Vec<(&'static str, &'static str, String, Expect)> {
    let go_userinfo = format!("export GOPROXY=\"https://feeduser:${{GO_FEED_TOKEN}}@{GO_HOST}\"");
    let go_netrc = format!(
        "export GOPROXY=\"https://{GO_HOST}\"\nprintf 'machine {GO_HOST} login feeduser password %s\\n' \"$GO_FEED_TOKEN\" > ~/.netrc && chmod 600 ~/.netrc"
    );
    let leak_header_query =
        Expect::Leaks("a secret left on the header place: the query stand-in goes out as it is");
    let leak_other_name =
        Expect::Leaks("a secret placed on another query name than the client sends");
    let leak_header_path =
        Expect::Leaks("a secret left on the header place: the path stand-in goes out as it is");
    let cases: Vec<(&'static str, &'static str, String, Expect)> = vec![
        ("pnpm", NPM, fill(PNPM), Expect::Swapped),
        ("yarn1", NPM, fill(YARN_CLASSIC), Expect::Swapped),
        ("yarn4", NPM, fill(YARN_BERRY), Expect::Swapped),
        ("gradle", MAVEN, fill(GRADLE), Expect::Swapped),
        ("cargo", CARGO_HOST, fill(CARGO), Expect::Swapped),
        ("pip-netrc", PIP, fill(NETRC_PIP), Expect::Swapped),
        ("curl-netrc", PIP, fill(NETRC_CURL), Expect::Swapped),
        (
            "go-netrc",
            GO_HOST,
            go_script("netrc", &go_netrc),
            Expect::Swapped,
        ),
        (
            "go-userinfo",
            GO_HOST,
            go_script("userinfo", &go_userinfo),
            Expect::Swapped,
        ),
        (
            "query-placed",
            QUERY_HOST,
            query_script("placed", "QUERY_FEED_TOKEN", "access_token"),
            Expect::Swapped,
        ),
        (
            "path-placed",
            PATH_HOST,
            path_script("placed", "PATH_FEED_TOKEN"),
            Expect::Swapped,
        ),
        (
            "query-header-place",
            QUERY_HOST,
            query_script("header-place", "QUERY_HEADER_TOKEN", "access_token"),
            leak_header_query,
        ),
        (
            "query-other-name",
            QUERY_HOST,
            query_script("other-name", "QUERY_OTHER_NAME_TOKEN", "access_token"),
            leak_other_name,
        ),
        (
            "path-header-place",
            PATH_HOST,
            path_script("header-place", "PATH_HEADER_TOKEN"),
            leak_header_path,
        ),
    ];
    cases
}

type PlacementRow = (&'static str, Expect, String, Reach);

/// What differs from each case's expectation, and a real token found in the guest.
fn placement_failures(rows: &[PlacementRow], search: &str) -> Vec<String> {
    let mut failures = Vec::new();
    for (case, expect, out, r) in rows {
        let worked = out.lines().any(|l| l.contains("rc=0 installed=yes"));
        match expect {
            Expect::Swapped => {
                if !worked {
                    failures.push(format!("{case}: the install did not work: {out}"));
                }
                if r.real == 0 {
                    failures.push(format!("{case}: the feed never received the real token"));
                }
                if r.stand_in > 0 {
                    failures.push(format!(
                        "{case}: {} request(s) reached the feed with the stand-in",
                        r.stand_in
                    ));
                }
            }
            Expect::Leaks(what) => {
                if worked || r.real > 0 {
                    failures.push(format!(
                        "{case}: expected the stand-in to leak ({what}) but the feed got the real token: {out}"
                    ));
                }
                if r.stand_in == 0 {
                    failures.push(format!(
                        "{case}: the stand-in never reached the feed ({what})"
                    ));
                }
            }
        }
    }
    if search.trim() != "files=[] processes=[0]" {
        failures.push(format!("the real token is in the guest: {search}"));
    }
    failures
}

/// Clients that put the token somewhere other than a header. Per case: the tool, the feed it
/// talks to, its guest script, and what the feed must show. A leak here is the stand-in (not the
/// real value) reaching the feed unswapped, so the call fails: the secret was not placed where the
/// client sends it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_token_placements_reach_the_feed_swapped_or_say_what_leaks() {
    let header = json!({"in": "header"});
    let query = json!({"in": "query", "name": "access_token"});
    let other_name = json!({"in": "query", "name": "token"});
    let path = json!({"in": "path"});
    let sites: Vec<&str> = MIRRORS.iter().chain(PLACEMENT_SITES).copied().collect();
    let (net, vm, workspace) = boot_placed(
        "feed-place",
        &[
            ("NPM_FEED_TOKEN", NPM, &header),
            ("PIP_FEED_TOKEN", PIP, &header),
            ("MAVEN_FEED_TOKEN", MAVEN, &header),
            ("CARGO_FEED_TOKEN", CARGO_HOST, &header),
            ("GO_FEED_TOKEN", GO_HOST, &header),
            ("QUERY_FEED_TOKEN", QUERY_HOST, &query),
            ("PATH_FEED_TOKEN", PATH_HOST, &path),
            ("QUERY_HEADER_TOKEN", QUERY_HOST, &header),
            ("QUERY_OTHER_NAME_TOKEN", QUERY_HOST, &other_name),
            ("PATH_HEADER_TOKEN", PATH_HOST, &header),
        ],
        &sites,
        true,
    )
    .await;

    let installed = run(&vm, &workspace, PLACEMENT_INSTALL, 30).await;
    assert!(
        ["apt=0", "npm_global=0", "gradle_install=0"]
            .iter()
            .all(|ok| installed.contains(ok)),
        "the tools could not be installed (live third parties: the Debian mirrors, the npm registry, Gradle's distribution; an outage there fails this test): {installed}"
    );

    let cases = placement_cases();

    // The lines `tool=<name> rc=<n> installed=<yes|no>` say which tool worked (a query script
    // runs curl and pip); the feed log says what the script's requests carried as a whole.
    let mut rows = Vec::new();
    for (case, host, script, expect) in &cases {
        let from = net.web.seen().len();
        let out = run(&vm, &workspace, script, 20).await;
        rows.push((*case, *expect, out, reach(&net.web, host, from)));
    }

    let control = run(&vm, &workspace, &fill(PLACEMENT_CONTROL), 10).await;
    for tool in ["query", "path", "pnpm", "cargo", "go"] {
        assert!(
            control.contains(&format!("control={tool} rc="))
                && !control.contains(&format!("control={tool} rc=0")),
            "{tool} fetched from the feed with no credential, so a pass above proves nothing: {control}"
        );
    }

    let search = search_for_real_token(&vm, &workspace).await;

    eprintln!("case                 expected  feed-200  real-token  unswapped-stand-in  refused");
    for (case, expect, _, r) in &rows {
        let expected = if matches!(expect, Expect::Swapped) {
            "swapped"
        } else {
            "leaks"
        };
        eprintln!(
            "{case:<20} {expected:<9} {:<9} {:<11} {:<19} {}",
            r.ok, r.real, r.stand_in, r.refused
        );
    }
    eprintln!("search for the real token: {search}");

    let failures = placement_failures(&rows, &search);
    assert!(failures.is_empty(), "token placements: {failures:#?}");

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
