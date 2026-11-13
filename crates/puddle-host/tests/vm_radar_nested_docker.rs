// SPDX-License-Identifier: GPL-3.0-or-later
//! The nested-Docker workloads against the real registries, as the session user, through the whole
//! stack in a real microVM: Docker CE installed from `download.docker.com` in the devcontainers .NET
//! image (which has `sudo`, so every Docker command is `sudo docker`), a `BuildKit` build with a
//! real `apk add`, a published port reached from this computer through a forward, two containers on
//! a user bridge, Compose with two services, Testcontainers (Python) with its Ryuk reaper, and .NET
//! Aspire (`aspire run` of the starter app, its Redis container started by Aspire in the nested
//! Docker).
//!
//! Radar rules: a step fails the test when it fails or when its log shows a retry. A host that no rule
//! allows is not silently approved: the proxy refuses it, the step fails and the message names the
//! open inbox rows, so the list of hosts below is what these workloads really need. Live third
//! parties: the Ubuntu and Docker apt repositories, Docker Hub, Alpine's mirror, `PyPI`, `NuGet` and
//! Microsoft's download hosts. The test is named `vm_radar_*`: the per-pull-request jobs skip it and
//! the weekly `full-test` workflow runs it. The half that needs no registry is
//! `vm_nested_docker.rs`.
#![expect(
    clippy::unwrap_used,
    reason = "VM test: a failed step fails the test; what each step said goes to the log"
)]

mod support;
mod vm_support;

use std::time::Duration;

use puddle_e2e::lab::WebOptions;
use puddle_test_kit::eventually_async;
use serde_json::json;
use support::{Api, named_id_options};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use vm_support::{VmHost, create_workspace_from, guest_as, start_vm_host_platform, web_internet};

/// The hosts the workloads reach: patterns of suffixes, so a new mirror of the same service is not a
/// failure but a host of another service is.
const HOSTS: &[&str] = &[
    // Ubuntu and Docker apt, Docker Hub and its blob hosts.
    "archive.ubuntu.com",
    "security.ubuntu.com",
    "ports.ubuntu.com",
    "download.docker.com",
    "*.docker.io",
    "*.docker.com",
    // Alpine's mirror, PyPI.
    "dl-cdn.alpinelinux.org",
    "pypi.org",
    "files.pythonhosted.org",
    // .NET, Aspire, NuGet and what they fetch.
    "aka.ms",
    "*.nuget.org",
    "*.microsoft.com",
    "dotnetcli.azureedge.net",
    "dotnetbuilds.azureedge.net",
    "dotnetcli.blob.core.windows.net",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github.com",
    "api.github.com",
    "*.visualstudio.com",
    "*.dot.net",
    "dot.net",
    // The revocation lookups of signed packages (plain HTTP).
    "*.digicert.com",
    "*.symcb.com",
    "*.symcd.com",
    "*.symantec.com",
];

const PUBLISHED: u16 = 18081;

/// One step: its name, the guest script, and how long it may take.
struct Step {
    name: &'static str,
    script: String,
    limit: Duration,
}

/// `body` as a step script: its output goes to a log, the step ends with `rc=` and `retries=` lines.
fn logged(name: &str, body: &str) -> String {
    format!(
        "cat > /tmp/step-{name}.sh <<'PUDDLE_STEP_EOF'\n{body}\nPUDDLE_STEP_EOF\n\
         bash -e -o pipefail /tmp/step-{name}.sh >/tmp/step-{name}.log 2>&1 </dev/null\nrc=$?\n\
         tail -n 25 /tmp/step-{name}.log\n\
         echo \"rc=$rc\"; echo \"retries=$(grep -ciE 'retry|retrying' /tmp/step-{name}.log)\""
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "one script per step, read top to bottom"
)]
fn steps() -> Vec<Step> {
    let docker_install = r#"
. /etc/os-release
sudo install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/$ID/gpg | sudo tee /etc/apt/keyrings/docker.asc >/dev/null
echo "deb [signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/$ID $VERSION_CODENAME stable" | sudo tee /etc/apt/sources.list.d/docker.list >/dev/null
sudo apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin python3-venv python3-pip
sudo mkdir -p /var/lib/docker
(sudo nohup dockerd >/tmp/dockerd.log 2>&1 &)
for i in $(seq 1 60); do sudo docker info >/dev/null 2>&1 && break; sleep 1; done
sudo docker info >/dev/null
sudo docker run --rm hello-world | grep -q 'Hello from Docker'
"#;
    let build = r#"
d=/tmp/build; mkdir -p $d
cat > $d/Dockerfile <<'EOF'
FROM alpine:3.20 AS tools
RUN apk add --no-cache curl && echo built > /marker
FROM alpine:3.20
RUN apk add --no-cache curl
COPY --from=tools /marker /marker
CMD ["cat", "/marker"]
EOF
sudo docker build --progress=plain -t built:real $d
sudo docker run --rm built:real | grep -q built
"#;
    let publish = format!(
        r"
sudo docker run -d --name web -p {PUBLISHED}:80 nginx:alpine
for i in $(seq 1 30); do curl -fsS http://127.0.0.1:{PUBLISHED}/ | grep -q nginx && break; sleep 1; done
curl -fsS http://127.0.0.1:{PUBLISHED}/ | grep -q nginx
"
    );
    let bridge = r"
sudo docker network create user-bridge
sudo docker run -d --name backend --network user-bridge nginx:alpine
for i in $(seq 1 30); do sudo docker run --rm --network user-bridge built:real curl -fsS http://backend/ | grep -q nginx && break; sleep 1; done
sudo docker run --rm --network user-bridge built:real curl -fsS http://backend/ | grep -q nginx
";
    let compose = r#"
d=/tmp/compose; mkdir -p $d
cat > $d/compose.yml <<'EOF'
services:
  cache:
    image: redis:7-alpine
  client:
    image: redis:7-alpine
    depends_on: [cache]
    command: ["sh", "-c", "for i in $(seq 1 30); do redis-cli -h cache ping && exit 0; sleep 1; done; exit 1"]
EOF
sudo docker compose -f $d/compose.yml -p radar up --abort-on-container-exit --exit-code-from client
sudo docker compose -f $d/compose.yml -p radar down
"#;
    let testcontainers = r#"
python3 -m venv /tmp/tc
/tmp/tc/bin/pip install --quiet testcontainers redis
cat > /tmp/tc/probe.py <<'EOF'
import redis
from testcontainers.redis import RedisContainer
with RedisContainer("redis:7-alpine") as r:
    c = redis.Redis(host=r.get_container_host_ip(), port=int(r.get_exposed_port(6379)))
    assert c.ping()
    print("redis answered")
EOF
sudo env DOCKER_HOST=unix:///var/run/docker.sock /tmp/tc/bin/python /tmp/tc/probe.py
# Ryuk removed what the run started: nothing named after the test's containers is left.
for i in $(seq 1 30); do [ -z "$(sudo docker ps -aq --filter label=org.testcontainers=true)" ] && break; sleep 1; done
[ -z "$(sudo docker ps -aq --filter label=org.testcontainers=true)" ]
"#;
    let aspire = r#"
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1
export PATH="$PATH:$HOME/.dotnet/tools"
dotnet new install Aspire.ProjectTemplates
d=/tmp/aspire-app; rm -rf $d; mkdir -p $d; cd $d
dotnet new aspire-starter --name Probe --use-redis-cache
dotnet tool install -g aspire.cli || dotnet tool update -g aspire.cli
cd $d
trap 'tail -n 40 /tmp/aspire-run.log' EXIT
# Aspire talks to Docker as the session user, like Docker's post-install steps arrange: join the docker group.
sudo usermod -aG docker "$(id -un)"
sg docker -c 'docker ps >/dev/null'
(sg docker -c "nohup aspire run --non-interactive >/tmp/aspire-run.log 2>&1" &)
for i in $(seq 1 150); do sudo docker ps --format '{{.Image}}' | grep -qi redis && break; sleep 2; done
sudo docker ps --format '{{.Image}}' | grep -qi redis
# Which port is the web frontend is not readable (the process runs with the docker group, so ss -p hides it): try them all. It answers its weather page, which goes through the API service to Redis.
ok=""
for i in $(seq 1 90); do
  for port in $(ss -ltnH 2>/dev/null | awk '{ n = split($4, a, ":"); print a[n] }' | sort -un); do
    for scheme in http https; do
      curl -fsSk --max-time 5 "$scheme://localhost:$port/weather" 2>/dev/null | grep -qi 'demonstrates showing data' && ok=1 && break 3
    done
  done
  sleep 2
done
[ -n "$ok" ]
"#;
    vec![
        Step {
            name: "docker-install",
            script: logged("docker-install", docker_install),
            limit: Duration::from_mins(15),
        },
        Step {
            name: "build",
            script: logged("build", build),
            limit: Duration::from_mins(10),
        },
        Step {
            name: "publish",
            script: logged("publish", &publish),
            limit: Duration::from_mins(10),
        },
        Step {
            name: "bridge",
            script: logged("bridge", bridge),
            limit: Duration::from_mins(5),
        },
        Step {
            name: "compose",
            script: logged("compose", compose),
            limit: Duration::from_mins(10),
        },
        Step {
            name: "testcontainers",
            script: logged("testcontainers", testcontainers),
            limit: Duration::from_mins(15),
        },
        Step {
            name: "aspire",
            script: logged("aspire", aspire),
            limit: Duration::from_mins(30),
        },
    ]
}

fn field<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .rev()
        .find_map(|l| l.strip_prefix(key))
        .unwrap_or("?")
}

/// The open inbox rows of the workspace: hosts a workload asked for that no rule allows.
fn asked_for(vm: &VmHost, workspace: &puddle_types::WorkspaceId) -> Vec<String> {
    vm.host
        .store()
        .open_pending_every_user(Some(workspace))
        .unwrap()
        .iter()
        .map(|r| format!("{}:{}", r.host, r.port))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_nested_docker_real_registries_testcontainers_and_aspire() {
    // No fake names: every name goes to the real internet through the company proxy.
    let net = web_internet(&[], WebOptions::default(), named_id_options()).await;
    let vm =
        start_vm_host_platform("nestedradar", net.options.clone(), &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let workspace = create_workspace_from(
        &vm,
        &api,
        "nr",
        HOSTS,
        json!({"image": "mcr.microsoft.com/devcontainers/dotnet:10.0", "memory_mib": 8192}),
    )
    .await;

    let mut failures = Vec::new();
    let name = format!("{}-nr", vm.settings.prefix.as_str());
    for step in steps() {
        let (code, out) =
            guest_as(&vm.runtime, &workspace, "agent", &step.script, step.limit).await;
        eprintln!("== {} (exit {code})\n{out}", step.name);
        let (rc, retries) = (field(&out, "rc="), field(&out, "retries="));
        if code != 0 || rc != "0" || retries != "0" {
            failures.push(format!(
                "{}: rc={rc} retries={retries} (live third parties: see HOSTS; hosts asked for and not allowed: {:?})",
                step.name,
                asked_for(&vm, &workspace)
            ));
        }
        if step.name == "aspire" && rc != "0" {
            let (_, said) = guest_as(
                &vm.runtime,
                &workspace,
                "agent",
                "id; tr '\\r' '\\n' </tmp/aspire-run.log | tail -n 60; sudo docker ps -a; ss -ltnp | head -30",
                Duration::from_secs(60),
            )
            .await;
            eprintln!("aspire after the failure:\n{said}");
        }
        // After the published-port step, a forward carries a connection from this computer to it.
        if step.name == "publish" && rc == "0" {
            let path = format!("/api/workspaces/{name}/ports");
            eventually_async(
                "the agent to list the published port",
                Duration::from_secs(60),
                || async {
                    let view = api.get(&path).await.json();
                    let listed = view["ports"].as_array().is_some_and(|p| {
                        p.iter()
                            .any(|p| p["guest_port"] == PUBLISHED && p["listening"] == true)
                    });
                    listed.then_some(()).ok_or_else(|| view.to_string())
                },
            )
            .await;
            let host_port = puddle_test_kit::quiet_port();
            let put = api
                .put(
                    // flake-ok: an API URL path, not a file path
                    &format!("{path}/{PUBLISHED}"),
                    &json!({"host_port": host_port}).to_string(),
                )
                .await;
            assert_eq!(put.status, 200, "{}", put.body);
            let page = eventually_async(
                "nginx to answer through the forward",
                Duration::from_secs(30),
                || async {
                    let mut conn = tokio::net::TcpStream::connect(("127.0.0.1", host_port))
                        .await
                        .map_err(|e| e.to_string())?;
                    conn.write_all(b"GET / HTTP/1.0\r\n\r\n")
                        .await
                        .map_err(|e| e.to_string())?;
                    let mut text = String::new();
                    tokio::time::timeout(Duration::from_secs(5), conn.read_to_string(&mut text))
                        .await
                        .map_err(|_| "no answer".to_owned())?
                        .map_err(|e| e.to_string())?;
                    text.contains("nginx").then_some(()).ok_or(text)
                },
            )
            .await;
            let () = page;
        }
    }
    assert!(failures.is_empty(), "nested Docker radar: {failures:#?}");

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
