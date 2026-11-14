// SPDX-License-Identifier: AGPL-3.0-or-later
//! The workloads of a developer's Docker, run by the session user against a real `dockerd` in a real
//! microVM, with the proxy as the only way out: a `BuildKit` build whose `RUN` step fetches from a
//! site, a container's published port reached from this computer through a forward, two containers
//! talking on a user bridge, and Compose with two services.
//!
//! The guest image is `docker:dind` and every image the guest runs is built in the guest from files
//! the guest has (`FROM scratch`), so no result depends on a registry. The sites the build and
//! the Compose client reach are a fake website behind a fake company proxy (`web_internet`). The
//! half that pulls from the real registries (Testcontainers with its reaper, .NET Aspire, an image
//! with `sudo`) is `vm_radar_nested_docker_*` (`vm_radar_nested_docker.rs`).
//!
//! Which workload needs which rule is part of the bar: the two sites are allowed up front, and the
//! container-to-container traffic of the bridge and of Compose must make no inbox row and no audit
//! line, because it never leaves the guest.
mod support;
mod vm_support;

use std::time::Duration;

use puddle_e2e::lab::WebOptions;
use puddle_test_kit::eventually_async;
use puddle_types::WorkspaceId;
use serde_json::json;
use support::{Api, named_id_options};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use vm_support::{
    PROBE_IMAGE, VmHost, allowed_connections, as_agent, container_fetch, create_workspace_from,
    field, reached_by_host, start_vm_host_platform, wait_for_connection_record, web_internet,
};

/// The fake sites the workloads reach, each allowed by a rule from the start.
const BUILD_SITE: &str = "build.invalid";
const COMPOSE_SITE: &str = "compose.invalid";

/// The published port in the guest.
const PUBLISHED: u16 = 18080;

/// A one-line HTTP server in a container: busybox of `docker:dind` has no `httpd`, so `nc` answers
/// each connection with `$1` and listens again.
fn nc_server(answer: &str) -> String {
    format!(
        "while true; do printf 'HTTP/1.0 200 OK\\r\\n\\r\\n{answer}\\n' | /bin/busybox nc -l -p 8080; done"
    )
}

/// The Dockerfile of the build: the `RUN` step fetches from [`BUILD_SITE`] with no proxy variable in
/// it (the capture redirects it), and the second stage keeps what the first fetched.
const BUILD: &str = r#"d=/tmp/build
mkdir -p $d
cat > $d/Dockerfile <<'EOF'
FROM probe:local AS fetch
RUN ["/bin/busybox", "wget", "-T", "10", "-q", "-O", "/body", "http://build.invalid/from-the-build-step"]
FROM probe:local
COPY --from=fetch /body /body
CMD ["/bin/busybox", "cat", "/body"]
EOF
dsudo build --progress=plain -t built:local $d 2>&1
echo "build=$?"
dsudo run --rm --pull=never built:local 2>&1
echo "run=$?""#;

/// Everything of the `docker:dind` workspace goes through the session user's group, as in
/// `vm_capture.rs`: wait for `dockerd`, then build the probe image.
async fn dockerd_and_probe_image(vm: &VmHost, workspace: &WorkspaceId) {
    eventually_async("dockerd to answer", Duration::from_secs(100), || async {
        let (code, out) = as_agent(vm, workspace, "dsudo info >/dev/null 2>&1").await;
        (code == 0).then_some(()).ok_or(out)
    })
    .await;
    let (code, built) = as_agent(vm, workspace, PROBE_IMAGE).await;
    assert_eq!(code, 0, "building the probe image: {built}");
}

/// What a connection to `127.0.0.1:port` on this computer is answered to a `GET /`, within 30 s.
async fn get_through_forward(port: u16) -> String {
    eventually_async(
        "the published port to answer through the forward",
        Duration::from_secs(30),
        || async {
            let mut conn = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .map_err(|e| e.to_string())?;
            conn.write_all(b"GET / HTTP/1.0\r\n\r\n")
                .await
                .map_err(|e| e.to_string())?;
            let mut text = String::new();
            tokio::time::timeout(Duration::from_secs(5), conn.read_to_string(&mut text))
                .await
                .map_err(|_| "no answer in 5 s".to_owned())?
                .map_err(|e| e.to_string())?;
            text.contains("published")
                .then_some(text.clone())
                .ok_or(text)
        },
    )
    .await
}

/// Build, publish, bridge and Compose as `agent` on a `docker:dind` workspace whose only way out is
/// the proxy.
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_nested_docker_build_publish_bridge_and_compose_work_through_the_proxy() {
    let sites = [BUILD_SITE, COMPOSE_SITE];
    let net = web_internet(&sites, WebOptions::default(), named_id_options()).await;
    let vm = start_vm_host_platform("nested", net.options.clone(), &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let workspace = create_workspace_from(
        &vm,
        &api,
        "nested",
        &sites,
        json!({"image": "docker:dind", "memory_mib": 2048}),
    )
    .await;
    dockerd_and_probe_image(&vm, &workspace).await;

    // BuildKit: the progress output is BuildKit's, the `RUN` step reached the site through the
    // proxy, and the image holds what the site answered.
    let before = allowed_connections(&vm, BUILD_SITE, 80);
    let (code, built) = as_agent(&vm, &workspace, BUILD).await;
    eprintln!("build:\n{built}");
    assert_eq!(code, 0, "{built}");
    assert!(
        built.contains("[internal] load build definition"),
        "the build did not run on BuildKit: {built}"
    );
    assert_eq!(field(&built, "build="), "0", "{built}");
    assert_eq!(field(&built, "run="), "0", "{built}");
    assert!(
        built.contains("lab web host: /from-the-build-step"),
        "the image does not hold the site's answer: {built}"
    );
    wait_for_connection_record(&vm, BUILD_SITE, 80, before).await;
    assert!(
        reached_by_host(&net.web, BUILD_SITE) > 0,
        "the site did not see the build step: {:?}",
        net.web.seen()
    );

    // A published port: the container answers on the guest's loopback, the agent lists the port,
    // and a forward carries a connection from this computer to it.
    let (code, said) = as_agent(
        &vm,
        &workspace,
        &format!(
            "dsudo run -d --name pub --pull=never -p {PUBLISHED}:8080 probe:local /bin/busybox sh -c \"{}\"",
            nc_server("published")
        ),
    )
    .await;
    assert_eq!(code, 0, "{said}");
    let ports_path = format!(
        "/api/workspaces/{}-nested/ports",
        vm.settings.prefix.as_str()
    );
    eventually_async(
        "the agent to list the published port",
        Duration::from_secs(60),
        || async {
            let view = api.get(&ports_path).await.json();
            let listed = view["ports"].as_array().is_some_and(|ports| {
                ports
                    .iter()
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
            &format!("{ports_path}/{PUBLISHED}"),
            &json!({"host_port": host_port}).to_string(),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    let answer = get_through_forward(host_port).await;
    eprintln!("published port through the forward: {answer:?}");

    // A user bridge: a container reaches its neighbour by name.
    let (code, said) = as_agent(
        &vm,
        &workspace,
        &format!(
            "dsudo network create bridge-two >/dev/null && dsudo run -d --name neighbour --pull=never \
             --network bridge-two probe:local /bin/busybox sh -c \"{}\"",
            nc_server("neighbour")
        ),
    )
    .await;
    assert_eq!(code, 0, "{said}");
    let talk = container_fetch("dsudo", "bridge-two", "http://neighbour:8080/");
    let talked = eventually_async(
        "a container to reach its neighbour on the user bridge",
        Duration::from_secs(30),
        || async {
            let out = as_agent(&vm, &workspace, &talk).await.1;
            if field(&out, "exit=") == "0" {
                return Ok(out);
            }
            let said = as_agent(
                &vm,
                &workspace,
                "dsudo ps -a; dsudo logs neighbour 2>&1 | tail -5",
            )
            .await
            .1;
            Err(format!("{out}\n{said}"))
        },
    )
    .await;
    assert!(talked.contains("neighbour"), "{talked}");

    // Compose: two services, the client reaches the server by service name and then the site
    // through the proxy; its exit code is the command's.
    let before = allowed_connections(&vm, COMPOSE_SITE, 80);
    let (code, composed) = as_agent(&vm, &workspace, &compose_script()).await;
    eprintln!("compose:\n{composed}");
    assert_eq!(code, 0, "{composed}");
    assert_eq!(field(&composed, "compose="), "0", "{composed}");
    assert!(
        composed.contains("compose-server") && composed.contains("lab web host: /from-compose"),
        "the client did not get both answers: {composed}"
    );
    wait_for_connection_record(&vm, COMPOSE_SITE, 80, before).await;

    // What stayed inside the guest never reached the proxy: no inbox row, no audit line names a
    // container, a service or a network.
    let rows = vm
        .host
        .store()
        .open_pending_every_user(&puddle_store::testing::all_users(), Some(&workspace))
        .unwrap();
    assert!(rows.is_empty(), "inbox rows from nested traffic: {rows:?}");
    let audit = vm.host.store().audit_lines(0, 100_000).unwrap();
    for inner in ["neighbour", "compose-server", "pub", "bridge-two"] {
        assert!(
            !audit.iter().any(|(_, line)| line.contains(inner)),
            "the audit names {inner}, which never left the guest"
        );
    }

    // Nothing is left running or stopped after the workloads clean up after themselves.
    let (code, left) = as_agent(
        &vm,
        &workspace,
        "dsudo rm -f pub neighbour >/dev/null; dsudo compose -p nested down >/dev/null 2>&1; dsudo ps -aq",
    )
    .await;
    assert_eq!(code, 0, "{left}");
    assert_eq!(left.trim(), "", "containers left: {left}");

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// The Compose file and the command that runs it. The client retries its first request until the
/// server listens: Compose starts the services, it does not wait for the program in them.
fn compose_script() -> String {
    let server = nc_server("compose-server");
    let client = format!(
        "for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do \
           /bin/busybox wget -T 5 -q -O - http://server:8080/ && \
           /bin/busybox wget -T 10 -q -O - http://{COMPOSE_SITE}/from-compose && exit 0; \
           /bin/busybox sleep 1; done; exit 1"
    );
    let file = json!({
        "services": {
            "server": {
                "image": "probe:local", "pull_policy": "never",
                "command": ["/bin/busybox", "sh", "-c", server]
            },
            "client": {
                "image": "probe:local", "pull_policy": "never",
                "depends_on": ["server"],
                "command": ["/bin/busybox", "sh", "-c", client]
            }
        }
    });
    // Compose reads JSON as YAML.
    format!(
        "d=/tmp/compose; mkdir -p $d\ncat > $d/compose.yml <<'EOF'\n{file}\nEOF\n\
         dsudo compose -f $d/compose.yml -p nested up --abort-on-container-exit --exit-code-from client 2>&1\n\
         echo \"compose=$?\""
    )
}
