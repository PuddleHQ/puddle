// SPDX-License-Identifier: AGPL-3.0-or-later
use std::net::{IpAddr, Ipv6Addr};

use super::*;

const PROXY: &str = "http://127.0.0.1:3128";
const BYPASS: &str = "localhost,127.0.0.1,::1,172.17.0.1";
const JAVA_BYPASS: &str = "localhost|127.*|[::1]|0.0.0.0|[::0]|172.17.0.1";

fn default_config() -> GuestProxyConfig {
    guest_proxy_config(&ProxySettings::default(), &[]).unwrap()
}

fn file_at<'a>(config: &'a GuestProxyConfig, path: &str) -> &'a GuestFile {
    config
        .files
        .iter()
        .find(|f| f.path().as_str() == path)
        .unwrap_or_else(|| panic!("no file at {path}"))
}

fn text(f: &GuestFile) -> &str {
    std::str::from_utf8(f.contents()).unwrap()
}

fn image(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn env_snapshot() {
    let config = default_config();
    let env: Vec<(&str, &str)> = config.env.iter().collect();
    let java = format!(
        "-Dhttp.proxyHost=127.0.0.1 -Dhttp.proxyPort=3128 -Dhttps.proxyHost=127.0.0.1 \
             -Dhttps.proxyPort=3128 -Dhttp.nonProxyHosts={JAVA_BYPASS}"
    );
    let expected = vec![
        ("CARGO_NET_GIT_FETCH_WITH_CLI", "true"),
        ("ELECTRON_GET_USE_PROXY", "1"),
        ("GLOBAL_AGENT_HTTPS_PROXY", PROXY),
        ("GLOBAL_AGENT_HTTP_PROXY", PROXY),
        ("GLOBAL_AGENT_NO_PROXY", BYPASS),
        ("HTTPS_PROXY", PROXY),
        ("HTTP_PROXY", PROXY),
        ("JAVA_TOOL_OPTIONS", java.as_str()),
        ("MAVEN_ARGS", "-gs /etc/puddle/maven/settings.xml"),
        ("NODE_USE_ENV_PROXY", "1"),
        ("NO_PROXY", BYPASS),
        ("YARN_HTTPS_PROXY", PROXY),
        ("YARN_HTTP_PROXY", PROXY),
        ("http_proxy", PROXY),
        ("https_proxy", PROXY),
        ("no_proxy", BYPASS),
    ];
    assert_eq!(env, expected);
}

#[test]
fn files_come_in_a_fixed_order_with_their_modes() {
    let c = default_config();
    let files: Vec<(&str, u32)> = c
        .files
        .iter()
        .map(|f| (f.path().as_str(), f.mode()))
        .collect();
    assert_eq!(
        files,
        [
            (APT_CONF_GUEST, 0o644),
            (SUDOERS_GUEST, 0o440),
            (MAVEN_SETTINGS_GUEST, 0o644),
            ("/root/.gradle/init.d/puddle-proxy.gradle", 0o644),
            (SSH_CONFIG_GUEST, 0o644),
            (SSH_CONFIG_SYSTEM, 0o644),
            ("/root/.docker/config.json", 0o600),
        ]
    );
}

#[test]
fn apt_snapshot() {
    let c = default_config();
    assert_eq!(
        text(file_at(&c, APT_CONF_GUEST)),
        "\
// Written by Puddle at every boot; changes here are overwritten.
// apt's downloads go through Puddle's proxy, whatever environment apt starts with.
Acquire::http::Proxy \"http://127.0.0.1:3128\";
Acquire::https::Proxy \"http://127.0.0.1:3128\";
Acquire::http::Proxy::localhost \"DIRECT\";
Acquire::https::Proxy::localhost \"DIRECT\";
Acquire::http::Proxy::127.0.0.1 \"DIRECT\";
Acquire::https::Proxy::127.0.0.1 \"DIRECT\";
Acquire::http::Proxy::172.17.0.1 \"DIRECT\";
Acquire::https::Proxy::172.17.0.1 \"DIRECT\";
"
    );
}

#[test]
fn ssh_config_snapshot() {
    let c = default_config();
    assert_eq!(
        text(file_at(&c, SSH_CONFIG_GUEST)),
        "\
# Written by Puddle at every boot; changes here are overwritten.
# SSH from a workspace is not supported yet: ssh runs the agent as its ProxyCommand, which
# refuses the connection and says why. Loopback and the container bridge stay direct (the
# agent checks the address). A ProxyCommand or ProxyJump in ~/.ssh/config comes first and
# wins.
Match host \"*,!localhost,!::1\"
    ProxyCommand '/puddle/puddle-agent' connect --local-net 172.17.0.0/16 %h %p %n
"
    );
}

#[test]
fn the_container_bridge_is_a_local_network_of_the_proxy_command_only_when_there_is_one() {
    let command = |container_proxy| {
        let settings = ProxySettings {
            container_proxy,
            ..ProxySettings::default()
        };
        let c = guest_proxy_config(&settings, &[]).unwrap();
        let ssh = text(file_at(&c, SSH_CONFIG_GUEST)).to_owned();
        // Names are matched only for the two that cannot be anything else.
        assert!(ssh.contains("Match host \"*,!localhost,!::1\"\n"), "{ssh}");
        assert!(
            !ssh.contains("127."),
            "no pattern for a name starting with 127.: {ssh}"
        );
        ssh.lines().last().unwrap().to_owned()
    };
    for none in [None, Some("[fd00::1]:3128".parse().unwrap())] {
        assert_eq!(
            command(none),
            "    ProxyCommand '/puddle/puddle-agent' connect %h %p %n"
        );
    }
    assert_eq!(
        command(Some("10.200.0.1:3128".parse().unwrap())),
        "    ProxyCommand '/puddle/puddle-agent' connect --local-net 10.200.0.0/16 %h %p %n"
    );
}

#[test]
fn the_system_ssh_config_gets_only_the_include_line_through_a_merge() {
    let c = default_config();
    let f = file_at(&c, SSH_CONFIG_SYSTEM);
    let spec = MergeSpec::new(
        MergeFormat::SshConfig,
        vec![MergeEntry::ssh_include("/etc/ssh/ssh_config.d/*.conf")],
    )
    .unwrap();
    assert_eq!(*f.apply(), puddle_types::ApplyKind::Merge(spec));
    assert!(text(f).ends_with("Include /etc/ssh/ssh_config.d/*.conf\n"));
    // The pattern is the folder the drop-in is written into.
    assert!(SSH_CONFIG_GUEST.starts_with("/etc/ssh/ssh_config.d/"));
}

#[test]
fn the_known_hosts_file_is_a_merge_of_one_block_and_refuses_what_is_not_host_keys() {
    let lines = "github.com ssh-ed25519 AAAA\n[github.com]:443 ssh-ed25519 AAAA";
    let f = ssh_known_hosts_file(lines).unwrap();
    assert_eq!(f.path().as_str(), SSH_KNOWN_HOSTS_GUEST);
    assert!(matches!(f.apply(), puddle_types::ApplyKind::Merge(_)));
    assert_eq!(
        text(&f),
        format!("# puddle: begin\n{lines}\n# puddle: end\n")
    );
    for bad in ["", "# a comment", "a ssh-rsa A\n\nb ssh-rsa B"] {
        let err = ssh_known_hosts_file(bad).unwrap_err();
        assert!(matches!(err, ConfigError::GuestPath { .. }), "{bad:?}");
    }
}

#[test]
fn the_agent_path_is_one_shell_word_in_the_proxy_command() {
    let settings = ProxySettings {
        agent: GuestPath::new("/opt/it's puddle/agent").unwrap(),
        ..ProxySettings::default()
    };
    let c = guest_proxy_config(&settings, &[]).unwrap();
    let ssh = text(file_at(&c, SSH_CONFIG_GUEST));
    assert!(
        ssh.ends_with(
            "ProxyCommand '/opt/it'\\''s puddle/agent' connect --local-net 172.17.0.0/16 %h %p %n\n"
        ),
        "{ssh}"
    );
    for bad in ["/opt/100%/agent", "/opt/new\nline/agent"] {
        let settings = ProxySettings {
            agent: GuestPath::new(bad).unwrap(),
            ..ProxySettings::default()
        };
        let err = guest_proxy_config(&settings, &[]).unwrap_err();
        assert!(err.to_string().contains("ProxyCommand"), "{bad}: {err}");
    }
}

#[test]
fn the_ssh_agent_socket_is_named_to_every_client_and_to_ssh_only_when_it_is_on() {
    let off = default_config();
    assert_eq!(off.env.get("SSH_AUTH_SOCK"), None);
    assert!(!text(file_at(&off, SSH_CONFIG_GUEST)).contains("IdentityAgent"));

    let settings = ProxySettings {
        ssh_agent: Some(GuestPath::new("/run/puddle/ssh-agent.sock").unwrap()),
        ..ProxySettings::default()
    };
    let on = guest_proxy_config(&settings, &[]).unwrap();
    assert_eq!(
        on.env.get("SSH_AUTH_SOCK"),
        Some("/run/puddle/ssh-agent.sock")
    );
    let sudoers = text(file_at(&on, SUDOERS_GUEST));
    assert!(sudoers.contains(" SSH_AUTH_SOCK "), "{sudoers}");
    let ssh = text(file_at(&on, SSH_CONFIG_GUEST));
    assert!(
        ssh.ends_with(
            "Match host \"*,!localhost,!::1\"\n    \
             ProxyCommand '/puddle/puddle-agent' connect --local-net 172.17.0.0/16 %h %p %n\n    \
             IdentityAgent \"/run/puddle/ssh-agent.sock\"\n"
        ),
        "{ssh}"
    );

    for bad in ["/run/100%.sock", "/run/${HOME}.sock", "/run/a\"b.sock"] {
        let settings = ProxySettings {
            ssh_agent: Some(GuestPath::new(bad).unwrap()),
            ..ProxySettings::default()
        };
        let err = guest_proxy_config(&settings, &[]).unwrap_err();
        assert!(err.to_string().contains("IdentityAgent"), "{bad}: {err}");
    }
}

#[test]
fn sudoers_snapshot() {
    let c = default_config();
    assert_eq!(
        text(file_at(&c, SUDOERS_GUEST)),
        "\
# Written by Puddle at every boot; changes here are overwritten.
# Keeps Puddle's proxy variables under sudo.
Defaults env_keep += \"CARGO_NET_GIT_FETCH_WITH_CLI ELECTRON_GET_USE_PROXY \
GLOBAL_AGENT_HTTPS_PROXY GLOBAL_AGENT_HTTP_PROXY GLOBAL_AGENT_NO_PROXY HTTPS_PROXY HTTP_PROXY \
JAVA_TOOL_OPTIONS MAVEN_ARGS NODE_USE_ENV_PROXY NO_PROXY YARN_HTTPS_PROXY YARN_HTTP_PROXY \
http_proxy https_proxy no_proxy\"
"
    );
}

#[test]
fn sudo_keeps_every_variable_puddle_sets() {
    let c = default_config();
    let sudoers = text(file_at(&c, SUDOERS_GUEST));
    let kept = sudoers
        .lines()
        .find_map(|l| l.strip_prefix("Defaults env_keep += \""))
        .unwrap()
        .trim_end_matches('"');
    let kept: Vec<&str> = kept.split(' ').collect();
    let set: Vec<&str> = c.env.iter().map(|(k, _)| k).collect();
    assert_eq!(kept, set);
}

#[test]
fn maven_snapshot() {
    let c = default_config();
    let proxy = |protocol: &str| {
        format!(
            "    <proxy>
      <id>puddle-{protocol}</id>
      <active>true</active>
      <protocol>{protocol}</protocol>
      <host>127.0.0.1</host>
      <port>3128</port>
      <nonProxyHosts>{JAVA_BYPASS}</nonProxyHosts>
    </proxy>
"
        )
    };
    let expected = format!(
            "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!-- Written by Puddle at every boot; changes here are overwritten.
     Maven's global settings through MAVEN_ARGS (-gs); ~/.m2/settings.xml still applies on top. -->
<settings xmlns=\"http://maven.apache.org/SETTINGS/1.0.0\"
          xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"
          xsi:schemaLocation=\"http://maven.apache.org/SETTINGS/1.0.0 https://maven.apache.org/xsd/settings-1.0.0.xsd\">
  <proxies>
{}{}  </proxies>
</settings>
",
            proxy("http"),
            proxy("https")
        );
    assert_eq!(text(file_at(&c, MAVEN_SETTINGS_GUEST)), expected);
}

#[test]
fn gradle_snapshot() {
    let c = default_config();
    assert_eq!(
        text(file_at(&c, "/root/.gradle/init.d/puddle-proxy.gradle")),
        format!(
            "\
// Written by Puddle at every boot; changes here are overwritten.
// Gradle keeps Puddle's proxy even when JAVA_TOOL_OPTIONS is cleared.
[
    'http.proxyHost': '127.0.0.1',
    'http.proxyPort': '3128',
    'https.proxyHost': '127.0.0.1',
    'https.proxyPort': '3128',
    'http.nonProxyHosts': '{JAVA_BYPASS}',
    'https.nonProxyHosts': '{JAVA_BYPASS}'
].each {{ key, value -> System.setProperty(key, value) }}
"
        )
    );
}

#[test]
fn docker_cli_snapshot() {
    let c = default_config();
    assert_eq!(
        text(file_at(&c, "/root/.docker/config.json")),
        "\
{
\t\"proxies\": {
\t\t\"default\": {
\t\t\t\"httpProxy\": \"http://172.17.0.1:3128\",
\t\t\t\"httpsProxy\": \"http://172.17.0.1:3128\",
\t\t\t\"noProxy\": \"localhost,127.0.0.1,::1,172.17.0.1\"
\t\t}
\t}
}
"
    );
}

#[test]
fn docker_cli_config_is_merged_so_a_docker_login_survives() {
    let c = default_config();
    let f = file_at(&c, "/root/.docker/config.json");
    let puddle_types::ApplyKind::Merge(spec) = f.apply() else {
        panic!("the Docker CLI config must be merged, not replaced")
    };
    assert_eq!(
        spec.keys(),
        [vec!["proxies".to_owned(), "default".to_owned()]]
    );
    let login = "{\n\t\"auths\": {\n\t\t\"registry.example.test\": {\n\t\t\t\"auth\": \"dTpw\"\n\t\t}\n\t}\n}\n";
    let puddle_types::Merged::Write(out) = spec.apply(Some(login.as_bytes()), &[]).unwrap() else {
        panic!("nothing merged")
    };
    let out = String::from_utf8(out).unwrap();
    assert!(out.starts_with("{\n\t\"auths\": {\n\t\t\"registry.example.test\": {\n\t\t\t\"auth\": \"dTpw\"\n\t\t}\n\t},\n\t\"proxies\": {"), "{out}");
    // Only Puddle's other files are written whole.
    assert!(
        c.files
            .iter()
            .filter(|g| g.path() != f.path() && g.path().as_str() != SSH_CONFIG_SYSTEM)
            .all(|g| *g.apply() == puddle_types::ApplyKind::Replace)
    );
}

#[test]
fn user_entries_reach_every_bypass_list() {
    let settings = ProxySettings {
        no_proxy: [
            "git.corp.example",
            ".corp.example",
            "10.1.2.3",
            "fd00::1",
            "localhost",
        ]
        .into_iter()
        .map(|e| NoProxyEntry::new(e).unwrap())
        .collect(),
        ..ProxySettings::default()
    };
    let c = guest_proxy_config(&settings, &[]).unwrap();
    let bypass = format!("{BYPASS},git.corp.example,.corp.example,10.1.2.3,fd00::1");
    assert_eq!(c.env.get("NO_PROXY"), Some(bypass.as_str()));
    assert_eq!(c.env.get("no_proxy"), Some(bypass.as_str()));
    assert_eq!(c.env.get("GLOBAL_AGENT_NO_PROXY"), Some(bypass.as_str()));
    let java = format!(
        "{JAVA_BYPASS}|git.corp.example|*.git.corp.example|*.corp.example|10.1.2.3|[fd00::1]|*.localhost"
    );
    assert!(c.env.get("JAVA_TOOL_OPTIONS").unwrap().ends_with(&java));
    assert!(text(file_at(&c, MAVEN_SETTINGS_GUEST)).contains(&java));
    let apt = text(file_at(&c, APT_CONF_GUEST));
    assert!(apt.contains("Acquire::https::Proxy::git.corp.example \"DIRECT\";"));
    assert!(apt.contains("Acquire::http::Proxy::10.1.2.3 \"DIRECT\";"));
    assert!(!apt.contains("corp.example \"DIRECT\";\nAcquire::https::Proxy::.corp"));
    assert!(!apt.contains("fd00"));
    assert!(text(file_at(&c, "/root/.docker/config.json")).contains(&bypass));
}

#[test]
fn image_java_and_maven_values_are_kept_with_puddles_appended() {
    let env = image(&[
        ("JAVA_TOOL_OPTIONS", "-Xmx1g"),
        ("MAVEN_ARGS", "-B"),
        ("JAVA_TOOL_OPTIONS", " -Xss2m "),
    ]);
    let c = guest_proxy_config(&ProxySettings::default(), &env).unwrap();
    let java = c.env.get("JAVA_TOOL_OPTIONS").unwrap();
    assert!(
        java.starts_with("-Xss2m -Dhttp.proxyHost=127.0.0.1 "),
        "{java}"
    );
    assert_eq!(
        c.env.get("MAVEN_ARGS"),
        Some("-B -gs /etc/puddle/maven/settings.xml")
    );

    // A value that already carries Puddle's (an image built from a Puddle workspace) isn't doubled.
    let again = image(&[
        ("JAVA_TOOL_OPTIONS", java),
        ("MAVEN_ARGS", c.env.get("MAVEN_ARGS").unwrap()),
    ]);
    let c2 = guest_proxy_config(&ProxySettings::default(), &again).unwrap();
    assert_eq!(c2.env.get("JAVA_TOOL_OPTIONS"), Some(java));
    assert_eq!(c2.env.get("MAVEN_ARGS"), c.env.get("MAVEN_ARGS"));

    // Blank image values count as unset.
    let blank = image(&[("JAVA_TOOL_OPTIONS", "  "), ("MAVEN_ARGS", "")]);
    let c3 = guest_proxy_config(&ProxySettings::default(), &blank).unwrap();
    assert_eq!(c3.env, default_config().env);
}

#[test]
fn image_proxy_values_are_replaced() {
    let env = image(&[
        ("HTTPS_PROXY", "http://corp-proxy:8080"),
        ("NO_PROXY", "*.corp"),
    ]);
    let c = guest_proxy_config(&ProxySettings::default(), &env).unwrap();
    assert_eq!(c.env, default_config().env);
}

#[test]
fn image_dirs_move_the_per_user_files() {
    let env = image(&[
        ("GRADLE_USER_HOME", "/opt/gradle-home"),
        ("DOCKER_CONFIG", "/etc/docker-cli"),
    ]);
    let c = guest_proxy_config(&ProxySettings::default(), &env).unwrap();
    file_at(&c, "/opt/gradle-home/init.d/puddle-proxy.gradle");
    file_at(&c, "/etc/docker-cli/config.json");

    // Unusable values (relative, `..`, the root) fall back to the home.
    for bad in ["relative", "/a/../b", "/", "/trailing/"] {
        let env = image(&[("GRADLE_USER_HOME", bad), ("DOCKER_CONFIG", bad)]);
        let c = guest_proxy_config(&ProxySettings::default(), &env).unwrap();
        file_at(&c, "/root/.gradle/init.d/puddle-proxy.gradle");
        file_at(&c, "/root/.docker/config.json");
    }
}

#[test]
fn another_home_moves_the_per_user_files() {
    for (home, gradle) in [
        (
            "/home/vscode",
            "/home/vscode/.gradle/init.d/puddle-proxy.gradle",
        ),
        ("/", "/.gradle/init.d/puddle-proxy.gradle"),
    ] {
        let settings = ProxySettings {
            home: GuestPath::new(home).unwrap(),
            ..ProxySettings::default()
        };
        let c = guest_proxy_config(&settings, &[]).unwrap();
        file_at(&c, gradle);
    }
}

#[test]
fn a_home_too_long_for_a_guest_path_is_an_error() {
    let settings = ProxySettings {
        home: GuestPath::new(&format!("/{}", "h".repeat(4090))).unwrap(),
        ..ProxySettings::default()
    };
    let err = guest_proxy_config(&settings, &[]).unwrap_err();
    assert!(matches!(err, ConfigError::GuestPath { .. }), "{err}");
    assert!(err.to_string().contains("4096"), "{err}");
}

#[test]
fn an_image_value_with_nul_is_an_error() {
    let env = image(&[("JAVA_TOOL_OPTIONS", "-Da=\0")]);
    let err = guest_proxy_config(&ProxySettings::default(), &env).unwrap_err();
    assert_eq!(
        err,
        ConfigError::ImageEnv {
            name: "JAVA_TOOL_OPTIONS".to_owned(),
            reason: "must not contain NUL".to_owned(),
        }
    );
    assert!(err.to_string().contains("JAVA_TOOL_OPTIONS"));
}

#[test]
fn without_a_container_proxy_there_is_no_docker_config_or_bridge_entry() {
    let settings = ProxySettings {
        container_proxy: None,
        ..ProxySettings::default()
    };
    let c = guest_proxy_config(&settings, &[]).unwrap();
    assert!(
        c.files
            .iter()
            .all(|f| !f.path().as_str().contains("docker"))
    );
    assert_eq!(c.env.get("NO_PROXY"), Some("localhost,127.0.0.1,::1"));
    assert!(
        c.env
            .get("JAVA_TOOL_OPTIONS")
            .unwrap()
            .ends_with("-Dhttp.nonProxyHosts=localhost|127.*|[::1]|0.0.0.0|[::0]")
    );
}

#[test]
fn an_ipv6_proxy_is_bracketed_where_a_url_or_java_needs_it() {
    let settings = ProxySettings {
        proxy: SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 3128),
        ..ProxySettings::default()
    };
    let c = guest_proxy_config(&settings, &[]).unwrap();
    assert_eq!(c.env.get("HTTPS_PROXY"), Some("http://[::1]:3128"));
    assert!(
        c.env
            .get("JAVA_TOOL_OPTIONS")
            .unwrap()
            .starts_with("-Dhttp.proxyHost=[::1] ")
    );
    assert!(text(file_at(&c, MAVEN_SETTINGS_GUEST)).contains("<host>::1</host>"));
}
