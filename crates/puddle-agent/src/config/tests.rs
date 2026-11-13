// SPDX-License-Identifier: GPL-3.0-or-later
use std::collections::HashMap;

use super::*;

fn from(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
    let map: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    Config::from_lookup(|k| map.get(k).cloned())
}

#[test]
fn no_variables_give_the_defaults() {
    let c = from(&[]).unwrap();
    assert_eq!(c, Config::default());
    assert_eq!(c.listen.to_string(), "127.0.0.1:3128");
    assert_eq!(c.target, Target::Vsock { cid: 2, port: 5000 });
    assert_eq!(c.vsock_buffer, 16 << 20);
    assert_eq!(c.oom, Some(OomSources::default()));
    assert_eq!(c.ports, Some(PortWatch::default()));
}

#[test]
fn the_ports_watch_has_its_own_switches_and_the_agents_own_ports_are_reserved() {
    let c = from(&[
        ("PUDDLE_AGENT_PORTS_DIR", "/x/net"),
        ("PUDDLE_AGENT_PORTS_POLL_MS", "50"),
    ])
    .unwrap();
    assert_eq!(
        c.ports,
        Some(PortWatch {
            dir: PathBuf::from("/x/net"),
            poll: Duration::from_millis(50)
        })
    );
    assert_eq!(from(&[("PUDDLE_AGENT_PORTS", "0")]).unwrap().ports, None);
    for (var, value) in [
        ("PUDDLE_AGENT_PORTS", "yes"),
        ("PUDDLE_AGENT_PORTS_DIR", ""),
        ("PUDDLE_AGENT_PORTS_POLL_MS", "0"),
    ] {
        assert_eq!(from(&[(var, value)]).unwrap_err().var, var);
    }
    // The proxy and the Docker bridge listener share a port by default; the others follow
    // what is switched on.
    assert_eq!(from(&[]).unwrap().reserved_ports(), [3128]);
    let c = from(&[
        ("PUDDLE_AGENT_BRIDGE", "172.17.0.1:3130"),
        ("PUDDLE_AGENT_CAPTURE", "on"),
        ("PUDDLE_AGENT_CAPTURE_PORT", "3129"),
        ("PUDDLE_AGENT_EDITOR_DIR", "/e"),
        ("PUDDLE_AGENT_EDITOR_DATA", "/d"),
        ("PUDDLE_AGENT_EDITOR_FOLDER", "/f"),
        ("PUDDLE_AGENT_EDITOR_PORT", "41337"),
        ("PUDDLE_AGENT_EDITOR_GRACE", "300"),
    ])
    .unwrap();
    assert_eq!(c.reserved_ports(), [3128, 3129, 3130, 41_337]);
}

#[test]
fn the_editor_server_is_off_until_the_host_names_its_folder() {
    assert_eq!(
        from(&[("PUDDLE_AGENT_EDITOR_PORT", "4000")])
            .unwrap()
            .editor,
        None
    );
    let c = from(&[
        ("PUDDLE_AGENT_EDITOR_DIR", "/puddle/code-server"),
        (
            "PUDDLE_AGENT_EDITOR_DATA",
            "/workspaces/a/.puddle/code-server",
        ),
        ("PUDDLE_AGENT_EDITOR_FOLDER", "/workspaces/a"),
        ("PUDDLE_AGENT_EDITOR_GRACE", "60"),
    ])
    .unwrap();
    let editor = c.editor.unwrap();
    assert_eq!(editor.server.grace_secs, 60);
    assert_eq!(editor.server.port, 41_337);
    assert_eq!(
        editor.password_file,
        PathBuf::from("/run/puddle/editor-password")
    );
    let c = from(&[
        ("PUDDLE_AGENT_EDITOR_DIR", "/e"),
        ("PUDDLE_AGENT_EDITOR_DATA", "/d"),
        ("PUDDLE_AGENT_EDITOR_FOLDER", "/f"),
        ("PUDDLE_AGENT_EDITOR_PASSWORD_FILE", "/x/pw"),
    ])
    .unwrap();
    assert_eq!(c.editor.unwrap().password_file, PathBuf::from("/x/pw"));
}

#[test]
fn an_unusable_editor_variable_is_named_and_never_replaced() {
    let err = from(&[
        ("PUDDLE_AGENT_EDITOR_DIR", "/e"),
        ("PUDDLE_AGENT_EDITOR_DATA", "/d"),
        ("PUDDLE_AGENT_EDITOR_FOLDER", "/f"),
        ("PUDDLE_AGENT_EDITOR_GRACE", "5"),
    ])
    .unwrap_err();
    assert_eq!(err.var, "PUDDLE_AGENT_EDITOR_GRACE");
    assert_eq!(err.value, "5");
    let err = from(&[("PUDDLE_AGENT_EDITOR_DIR", "/e")]).unwrap_err();
    assert_eq!(err.var, "PUDDLE_AGENT_EDITOR_DATA");
}

#[test]
fn capture_is_off_unless_asked_for_and_then_brings_the_stub_with_it() {
    assert_eq!(from(&[]).unwrap().capture, None);
    assert_eq!(
        from(&[("PUDDLE_AGENT_CAPTURE", "off")]).unwrap().capture,
        None
    );
    let c = from(&[("PUDDLE_AGENT_CAPTURE", "on")]).unwrap();
    assert_eq!(c.capture, Some(CaptureConfig::default()));
    assert_eq!(
        c.dns,
        Some(DnsConfig::default()),
        "the redirect sends DNS to the stub"
    );
    let c = from(&[
        ("PUDDLE_AGENT_CAPTURE", "on"),
        ("PUDDLE_AGENT_DNS", "198.18.0.9:5353"),
        ("PUDDLE_AGENT_CAPTURE_PORT", "4000"),
        ("PUDDLE_AGENT_CAPTURE_IFACE", "cap0"),
        ("PUDDLE_AGENT_CAPTURE_POLL_MS", "100"),
        ("PUDDLE_AGENT_CAPTURE_LOG_GROUP", "7"),
    ])
    .unwrap();
    let capture = c.capture.unwrap();
    assert_eq!(
        (
            capture.port,
            capture.interface.as_str(),
            capture.poll,
            capture.log_group
        ),
        (4000, "cap0", Duration::from_millis(100), Some(7))
    );
    assert_eq!(c.dns.unwrap().listen.to_string(), "198.18.0.9:5353");
    let c = from(&[
        ("PUDDLE_AGENT_CAPTURE", "on"),
        ("PUDDLE_AGENT_CAPTURE_LOG_GROUP", "0"),
    ])
    .unwrap();
    assert_eq!(c.capture.unwrap().log_group, None);
}

#[test]
fn capture_without_its_stub_or_with_odd_values_is_an_error() {
    let err = from(&[("PUDDLE_AGENT_CAPTURE", "on"), ("PUDDLE_AGENT_DNS", "off")]).unwrap_err();
    assert_eq!(err.var, "PUDDLE_AGENT_DNS");
    for (var, value) in [
        ("PUDDLE_AGENT_CAPTURE", "yes"),
        ("PUDDLE_AGENT_CAPTURE_PORT", "0"),
        ("PUDDLE_AGENT_CAPTURE_PORT", "70000"),
        ("PUDDLE_AGENT_CAPTURE_IFACE", ""),
        ("PUDDLE_AGENT_CAPTURE_IFACE", "a/b"),
        ("PUDDLE_AGENT_CAPTURE_IFACE", "sixteen-chars-xxx"),
        ("PUDDLE_AGENT_CAPTURE_POLL_MS", "0"),
        ("PUDDLE_AGENT_CAPTURE_LOG_GROUP", "x"),
    ] {
        let err = from(&[("PUDDLE_AGENT_CAPTURE", "on"), (var, value)]).unwrap_err();
        assert_eq!(err.var, var, "{value}");
    }
}

#[test]
fn every_variable_is_read() {
    let c = from(&[
        ("PUDDLE_AGENT_LISTEN", "0.0.0.0:3129"),
        ("PUDDLE_AGENT_TARGET", "unix:///run/p.sock"),
        ("PUDDLE_AGENT_MUX", "2"),
        ("PUDDLE_AGENT_VSOCK_BUF", "0"),
        ("PUDDLE_AGENT_WINDOW", "262144"),
        ("PUDDLE_AGENT_VMSTAT", "/x/vmstat"),
        ("PUDDLE_AGENT_KMSG", "/x/kmsg"),
        ("PUDDLE_AGENT_OOM_POLL_MS", "50"),
        ("PUDDLE_AGENT_OOM_GRACE_MS", "200"),
        ("PUDDLE_AGENT_LOG", "debug"),
    ])
    .unwrap();
    assert_eq!(c.listen.to_string(), "0.0.0.0:3129");
    assert_eq!(c.target, Target::Unix("/run/p.sock".into()));
    assert_eq!((c.mux, c.vsock_buffer, c.window), (2, 0, 262_144));
    let oom = c.oom.unwrap();
    assert_eq!(oom.vmstat, PathBuf::from("/x/vmstat"));
    assert_eq!(oom.kmsg, PathBuf::from("/x/kmsg"));
    assert_eq!(oom.poll, Duration::from_millis(50));
    assert_eq!(oom.grace, Duration::from_millis(200));
    assert_eq!(c.log, tracing::Level::DEBUG);
    assert_eq!(
        from(&[("PUDDLE_AGENT_OOM", "0")]).unwrap().oom,
        None,
        "0 turns the watch off"
    );
    assert!(from(&[("PUDDLE_AGENT_OOM", "1")]).unwrap().oom.is_some());
    assert_eq!(
        from(&[]).unwrap().entrypoint,
        Some(EntrypointWatch::default())
    );
    assert_eq!(
        from(&[("PUDDLE_AGENT_ENTRYPOINT_EXIT", "off")])
            .unwrap()
            .entrypoint,
        None
    );
    assert_eq!(
        from(&[("PUDDLE_AGENT_ENTRYPOINT_EXIT", "/tmp/exit")])
            .unwrap()
            .entrypoint
            .map(|w| w.exit_file),
        // flake-ok: a value that is parsed or compared and never opened, so the missing drive letter does not matter
        Some(PathBuf::from("/tmp/exit"))
    );
}

#[test]
fn invalid_values_are_errors_that_name_the_variable() {
    let cases = [
        ("PUDDLE_AGENT_LISTEN", "3128"),
        ("PUDDLE_AGENT_BRIDGE", "172.17.0.1"),
        ("PUDDLE_AGENT_BRIDGE", "0.0.0.0:3128"),
        ("PUDDLE_AGENT_BRIDGE", "127.0.0.1:3128"),
        ("PUDDLE_AGENT_BRIDGE", "224.0.0.1:3128"),
        ("PUDDLE_AGENT_BRIDGE", "255.255.255.255:3128"),
        ("PUDDLE_AGENT_BRIDGE", "172.17.0.1:0"),
        ("PUDDLE_AGENT_BRIDGE", "[::1]:3128"),
        ("PUDDLE_AGENT_BRIDGE", "on"),
        ("PUDDLE_AGENT_BRIDGE_POLL_MS", "0"),
        ("PUDDLE_AGENT_TARGET", "tcp://1.2.3.4:5"),
        ("PUDDLE_AGENT_TARGET", "vsock://2"),
        ("PUDDLE_AGENT_TARGET", "vsock://x:5000"),
        ("PUDDLE_AGENT_TARGET", "vsock://2:x"),
        ("PUDDLE_AGENT_TARGET", "unix://"),
        ("PUDDLE_AGENT_MUX", "0"),
        ("PUDDLE_AGENT_MUX", "65"),
        ("PUDDLE_AGENT_VSOCK_BUF", "-1"),
        ("PUDDLE_AGENT_WINDOW", "big"),
        ("PUDDLE_AGENT_VMSTAT", ""),
        ("PUDDLE_AGENT_KMSG", ""),
        ("PUDDLE_AGENT_OOM_POLL_MS", "0"),
        ("PUDDLE_AGENT_OOM_GRACE_MS", "60001"),
        ("PUDDLE_AGENT_OOM", "yes"),
        ("PUDDLE_AGENT_DNS", "yes"),
        ("PUDDLE_AGENT_DNS", "0.0.0.0:53"),
        ("PUDDLE_AGENT_DNS", "224.0.0.1:53"),
        ("PUDDLE_AGENT_DNS", "198.18.0.1"),
        ("PUDDLE_AGENT_DNS", "[::1]:53"),
        ("PUDDLE_AGENT_DNS_TABLE", ""),
        ("PUDDLE_AGENT_LOG", "loud"),
    ];
    for (var, value) in cases {
        let err = from(&[(var, value)]).unwrap_err();
        assert_eq!(err.var, var);
        assert!(err.to_string().starts_with(var), "{err}");
    }
}

#[test]
fn the_bridge_listener_is_on_for_the_docker_default_and_can_move_or_stop() {
    let default = from(&[]).unwrap().bridge.unwrap();
    assert_eq!(default.addr.to_string(), "172.17.0.1:3128");
    assert_eq!(default.poll, Duration::from_millis(200));
    let moved = from(&[
        ("PUDDLE_AGENT_BRIDGE", "192.168.200.1:3129"),
        ("PUDDLE_AGENT_BRIDGE_POLL_MS", "50"),
    ])
    .unwrap()
    .bridge
    .unwrap();
    assert_eq!(moved.addr.to_string(), "192.168.200.1:3129");
    assert_eq!(moved.poll, Duration::from_millis(50));
    assert_eq!(
        from(&[("PUDDLE_AGENT_BRIDGE", "off")]).unwrap().bridge,
        None
    );
}

#[test]
fn the_stub_dns_is_off_until_asked_for_and_binds_one_specific_address() {
    assert_eq!(from(&[]).unwrap().dns, None);
    assert_eq!(from(&[("PUDDLE_AGENT_DNS", "off")]).unwrap().dns, None);
    let on = from(&[("PUDDLE_AGENT_DNS", "on")]).unwrap().dns.unwrap();
    assert_eq!(on.listen.to_string(), "198.18.0.1:53");
    assert_eq!(on.table, Some(PathBuf::from("/run/puddle/dns-table")));
    let custom = from(&[
        ("PUDDLE_AGENT_DNS", "127.0.0.1:5353"),
        ("PUDDLE_AGENT_DNS_TABLE", "/x/table"),
    ])
    .unwrap()
    .dns
    .unwrap();
    assert_eq!(custom.listen.to_string(), "127.0.0.1:5353");
    assert_eq!(custom.table, Some(PathBuf::from("/x/table")));
    let no_file = from(&[
        ("PUDDLE_AGENT_DNS", "on"),
        ("PUDDLE_AGENT_DNS_TABLE", "off"),
    ])
    .unwrap()
    .dns
    .unwrap();
    assert_eq!(no_file.table, None);
}

#[test]
fn the_ssh_agent_socket_is_off_until_the_host_names_it_and_belongs_to_the_session_user() {
    assert_eq!(from(&[]).unwrap().ssh_agent, None);
    assert_eq!(
        from(&[("PUDDLE_AGENT_SSH_AGENT", "off")])
            .unwrap()
            .ssh_agent,
        None
    );
    assert_eq!(
        from(&[("PUDDLE_AGENT_SSH_AGENT_USER", "agent")])
            .unwrap()
            .ssh_agent,
        None,
        "a user without a socket turns nothing on"
    );
    let root = from(&[("PUDDLE_AGENT_SSH_AGENT", "/run/puddle/ssh-agent.sock")])
        .unwrap()
        .ssh_agent
        .unwrap();
    assert_eq!(root.socket, PathBuf::from("/run/puddle/ssh-agent.sock"));
    assert_eq!(root.user, "root");
    assert_eq!(root.owner_wait, Duration::from_secs(60));
    let session = from(&[
        ("PUDDLE_AGENT_SSH_AGENT", "/x/agent.sock"),
        ("PUDDLE_AGENT_SSH_AGENT_USER", "agent"),
    ])
    .unwrap()
    .ssh_agent
    .unwrap();
    assert_eq!(session.socket, PathBuf::from("/x/agent.sock"));
    assert_eq!(session.user, "agent");
    let err = from(&[("PUDDLE_AGENT_SSH_AGENT", "")]).unwrap_err();
    assert_eq!(err.var, "PUDDLE_AGENT_SSH_AGENT");
}

#[test]
fn an_echoed_value_is_cut() {
    let long = "x".repeat(500);
    let err = from(&[("PUDDLE_AGENT_LISTEN", &long)]).unwrap_err();
    assert_eq!(err.value.len(), 64);
}
