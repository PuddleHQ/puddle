// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn args(line: &str) -> impl Iterator<Item = String> + '_ {
    line.split_whitespace().map(str::to_owned)
}

fn parsed(line: &str) -> Command {
    parse(args(line)).unwrap_or_else(|e| panic!("{line}: {e}"))
}

fn refused(line: &str) -> String {
    parse(args(line)).expect_err(line)
}

#[test]
fn flags_select_the_pieces_and_the_defaults_fill_the_rest() {
    let cmd = parsed(
        "--squid-basic --interceptor --pac --name intranet.corp.test --pac-direct .corp.test --info lab.json",
    );
    assert_eq!(cmd.plan.squid, Some(SquidAuth::Basic));
    assert!(cmd.plan.interceptor && cmd.plan.pac && !cmd.plan.ntlm);
    assert_eq!(cmd.plan.names, ["intranet.corp.test"]);
    assert_eq!(cmd.plan.pac_direct, [".corp.test"]);
    assert_eq!(cmd.plan.allowed_ports, [80, 443]);
    assert_eq!(cmd.info.as_deref(), Some(Path::new("lab.json")));
    assert!(!cmd.self_test);
}

#[test]
fn all_keeps_the_other_settings_and_a_port_list_replaces_the_default() {
    let cmd = parsed(
        "--name a.test --allow-port 8080 --allow-port 8443 --all --stop s --ca c.pem --info i.json",
    );
    assert_eq!(cmd.plan.squid, Some(SquidAuth::Basic));
    assert!(cmd.plan.interceptor && cmd.plan.ntlm && cmd.plan.pac);
    assert_eq!(cmd.plan.names, ["a.test"]);
    assert_eq!(cmd.plan.allowed_ports, [8080, 8443]);
    assert_eq!(cmd.stop.as_deref(), Some(Path::new("s")));
    assert_eq!(cmd.ca.as_deref(), Some(Path::new("c.pem")));
}

#[test]
fn a_self_test_with_no_pieces_runs_them_all_and_needs_no_info_file() {
    let cmd = parsed("--self-test --name a.test");
    assert_eq!(cmd.plan.names, ["a.test"]);
    assert!(cmd.plan.squid.is_some() && cmd.plan.interceptor && cmd.plan.ntlm && cmd.plan.pac);
    let some = parsed("--self-test --ntlm");
    assert!(some.plan.squid.is_none() && some.plan.ntlm && !some.plan.pac);
}

#[test]
fn the_squid_chain_is_read_and_must_name_pieces_that_run() {
    let cmd = parsed("--squid --ntlm --squid-through ntlm --info i");
    assert_eq!(cmd.plan.squid_through, Some(Parent::Ntlm));
    let cmd = parsed("--squid --interceptor --squid-through interceptor --info i");
    assert_eq!(cmd.plan.squid_through, Some(Parent::Interceptor));
    assert!(refused("--squid --squid-through proxy --info i").contains("interceptor or ntlm"));
    assert!(refused("--ntlm --squid-through ntlm --info i").contains("needs --squid"));
    assert!(refused("--squid --squid-through ntlm --info i").contains("needs --ntlm"));
    assert!(
        refused("--squid --squid-through interceptor --info i").contains("needs --interceptor")
    );
}

#[test]
fn a_bad_command_line_says_what_is_wrong() {
    assert!(refused("--info i").contains("select at least one piece"));
    assert!(refused("--squid").contains("--info is required"));
    assert!(refused("--squid --squid-basic --info i").contains("exclude each other"));
    assert!(refused("--pac --info i").contains("needs a proxy"));
    assert!(refused("--squid --bogus --info i").contains("unknown argument --bogus"));
    assert!(refused("--squid --info").contains("--info needs a value"));
    assert!(refused("--squid --allow-port http --info i").contains("--allow-port http"));
    // The same flag twice is fine.
    assert_eq!(
        parsed("--squid-basic --squid-basic --info i").plan.squid,
        Some(SquidAuth::Basic)
    );
}

#[test]
fn the_pac_piece_alone_serves_a_script_and_lists_it_in_the_info() {
    let plan = Plan {
        pac: true,
        pac_direct: vec![".corp.test".into()],
        ..Plan::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let lab = Lab::start(&plan, &dir.path().join("ca.pem")).unwrap();
    let info = lab.info();
    let url = info["pac"]["url"].as_str().unwrap();
    assert!(
        url.starts_with("http://127.0.0.1:")
            && Path::new(url).extension().is_some_and(|e| e == "pac"),
        "{url}"
    );
    assert_eq!(info["pac"]["proxies"], json!([]));
    assert!(
        info.get("squid").is_none()
            && info.get("interceptor").is_none()
            && info.get("ntlm").is_none()
    );
    let lines = lab.check(1);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].as_ref().unwrap().contains(url), "{lines:?}");
}

#[test]
fn a_pac_answer_that_is_wrong_is_reported_with_the_answer() {
    let pac = PacServer::start("not a script");
    let wrong = check_pac(&pac, &["127.0.0.1:1".into()]).unwrap_err();
    assert!(
        wrong.contains("the script is wrong") && wrong.contains("not a script"),
        "{wrong}"
    );
}

#[test]
fn the_origin_answers_and_a_status_other_than_the_wanted_one_is_an_error() {
    let origin = origin_server().unwrap();
    let answer = http(origin, "GET / HTTP/1.1\r\nConnection: close\r\n\r\n").unwrap();
    assert_eq!(status(&answer), "HTTP/1.1 200 OK");
    assert!(expect_status(&answer, "200", "origin").is_ok());
    let wrong = expect_status(&answer, "407", "origin").unwrap_err();
    assert_eq!(wrong, "origin: wanted 407, got HTTP/1.1 200 OK");
    assert_eq!(status(""), "(nothing)");
}

#[test]
fn a_server_that_hangs_up_without_answering_is_a_wrong_status_not_a_hang() {
    let listener = TcpListener::bind((LOCAL, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let hangup = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        // Read the request first: closing with it unread would reset the connection.
        let _ = conn.read(&mut [0_u8; 64]);
    });
    let answer = http(port, "GET / HTTP/1.1\r\n\r\n").unwrap();
    hangup.join().unwrap();
    let wrong = expect_status(&answer, "200", "silent server").unwrap_err();
    assert_eq!(wrong, "silent server: wanted 200, got (nothing)");
}

#[test]
fn a_ca_file_without_a_certificate_fails_the_interceptor_check() {
    let origin = origin_server().unwrap();
    // The origin stands in for a proxy that forwards (it answers 200 to anything).
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, "nothing").unwrap();
    let wrong = check_interceptor(origin, &ca, origin).unwrap_err();
    assert!(wrong.contains("holds no certificate"), "{wrong}");
    let missing = check_interceptor(origin, &dir.path().join("none.pem"), origin).unwrap_err();
    assert!(missing.contains("CA file"), "{missing}");
    std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\n").unwrap();
    assert!(
        check_interceptor(origin, &ca, origin)
            .unwrap()
            .contains("root in")
    );
}
