// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use super::*;

fn name() -> WorkspaceId {
    WorkspaceId::new("box").unwrap()
}

#[test]
fn status_names_and_down_states() {
    let all = [
        (WorkspaceStatus::Created, "created", true),
        (WorkspaceStatus::Starting, "starting", false),
        (WorkspaceStatus::Running, "running", false),
        (WorkspaceStatus::Draining, "draining", false),
        (WorkspaceStatus::Paused, "paused", false),
        (WorkspaceStatus::Stopped, "stopped", true),
        (WorkspaceStatus::Crashed, "crashed", true),
        (WorkspaceStatus::VolumeMissing, "volume_missing", true),
    ];
    for (status, text, down) in all {
        assert_eq!(status.to_string(), text);
        assert_eq!(status.is_down(), down, "{status}");
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            format!("\"{text}\"")
        );
    }
}

#[test]
fn oom_kill_bounds_and_cleans_the_guest_supplied_name() {
    let long = "a".repeat(1000);
    assert_eq!(
        Event::oom_kill(name(), 7, &long),
        Event::OomKill {
            workspace: name(),
            pid: 7,
            process: "a".repeat(MAX_PROCESS_NAME_CHARS),
        }
    );
    assert_eq!(
        Event::oom_kill(name(), 7, "a\nb\0c"),
        Event::OomKill {
            workspace: name(),
            pid: 7,
            process: "a?b?c".to_owned(),
        }
    );
}

#[test]
fn events_are_internally_tagged_snake_case() {
    let e = Event::oom_kill(name(), 42, "node");
    assert_eq!(
        serde_json::to_string(&e).unwrap(),
        r#"{"type":"oom_kill","workspace":"box","pid":42,"process":"node"}"#
    );
    let s = Event::StatusChanged {
        workspace: name(),
        status: WorkspaceStatus::Crashed,
    };
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(
        json,
        r#"{"type":"status_changed","workspace":"box","status":"crashed"}"#
    );
    assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), s);
    assert_eq!(s.workspace(), Some(&name()));
    assert_eq!(e.workspace(), Some(&name()));
}

#[test]
fn a_disk_warning_names_the_disk_and_belongs_to_its_workspace() {
    let e = Event::DiskNearlyFull {
        workspace: name(),
        disk: DiskKind::Root,
        percent: 96,
    };
    let json = serde_json::to_string(&e).unwrap();
    assert_eq!(
        json,
        r#"{"type":"disk_nearly_full","workspace":"box","disk":"root","percent":96}"#
    );
    assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), e);
    assert_eq!(e.workspace(), Some(&name()));
}

#[test]
fn workspace_progress_is_per_workspace_and_always_carries_detail() {
    let steps = [
        (WorkspaceStep::PreparingVolume, "preparing_volume"),
        (WorkspaceStep::PullingImage, "pulling_image"),
        (WorkspaceStep::Cloning, "cloning"),
        (WorkspaceStep::Starting, "starting"),
        (WorkspaceStep::Syncing, "syncing"),
        (WorkspaceStep::Checking, "checking"),
        (WorkspaceStep::Reclaiming, "reclaiming"),
        (WorkspaceStep::ResettingRootDisk, "resetting_root_disk"),
        (WorkspaceStep::GrowingRootDisk, "growing_root_disk"),
        (WorkspaceStep::Stopping, "stopping"),
        (WorkspaceStep::Removing, "removing"),
        (WorkspaceStep::Done, "done"),
        (WorkspaceStep::Failed, "failed"),
    ];
    for (step, text) in steps {
        let e = Event::WorkspaceProgress {
            workspace: name(),
            step,
            detail: None,
            repository: None,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(
            json,
            format!(
                r#"{{"type":"workspace_progress","workspace":"box","step":"{text}","detail":null,"repository":null}}"#
            )
        );
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), e);
        assert_eq!(e.workspace(), Some(&name()));
    }
    let with_detail: Event = serde_json::from_str(
        r#"{"type":"workspace_progress","workspace":"box","step":"failed","detail":"disk full"}"#,
    )
    .unwrap();
    assert!(matches!(
        with_detail,
        Event::WorkspaceProgress { detail: Some(ref d), repository: None, .. } if d == "disk full"
    ));
    let cloning: Event = serde_json::from_str(
            r#"{"type":"workspace_progress","workspace":"box","step":"cloning","detail":"https://h/a/web.git","repository":"web"}"#,
        )
        .unwrap();
    assert!(matches!(
        cloning,
        Event::WorkspaceProgress { repository: Some(ref r), .. } if r.as_str() == "web"
    ));
}

#[test]
fn global_events_have_no_workspace_and_round_trip_without_one() {
    let g = Event::RulesChanged {};
    assert_eq!(g.workspace(), None);
    let json = serde_json::to_string(&g).unwrap();
    assert_eq!(json, r#"{"type":"rules_changed"}"#);
    assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), g);
    let sink = CollectingSink::default();
    sink.emit(g.clone());
    assert_eq!(sink.take(), [g]);
}

#[test]
fn login_problem_kinds_have_fixed_wire_names() {
    for (kind, wire) in [
        (LoginProblemKind::UnnamedToken, "unnamed_token"),
        (LoginProblemKind::CaptureOff, "capture_off"),
        (LoginProblemKind::Unknown, "unknown"),
    ] {
        let event = Event::LoginProblem {
            workspace: name(),
            service: "GitHub".into(),
            kind,
        };
        let json = format!(
            r#"{{"type":"login_problem","workspace":"box","service":"GitHub","kind":"{wire}"}}"#
        );
        assert_eq!(serde_json::to_string(&event).unwrap(), json);
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), event);
    }
}

#[test]
fn a_change_to_the_request_rules_has_a_fixed_shape_and_no_workspace() {
    let event = Event::RequestRulesChanged {};
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"request_rules_changed"}"#
    );
    assert_eq!(event.workspace(), None);
}

#[test]
fn pending_rule_and_audit_events_have_fixed_shapes() {
    let request = PendingSummary {
        id: 7,
        workspace: name(),
        host: "www.example.com".into(),
        registrable_domain: "example.com".into(),
        port: 443,
        first_seen: 1,
        last_seen: 2,
        attempts: 3,
    };
    for (event, json, workspace) in [
        (
            Event::PendingOpened { request },
            r#"{"type":"pending_opened","request":{"id":7,"workspace":"box","host":"www.example.com","registrable_domain":"example.com","port":443,"first_seen":1,"last_seen":2,"attempts":3}}"#,
            Some(name()),
        ),
        (
            Event::PendingUpdated {
                workspace: name(),
                id: 7,
                attempts: 4,
                last_seen: 9,
            },
            r#"{"type":"pending_updated","workspace":"box","id":7,"attempts":4,"last_seen":9}"#,
            Some(name()),
        ),
        (
            Event::PendingClosed {
                workspace: name(),
                id: 7,
                state: PendingEnd::Allowed,
                rule_id: Some(2),
            },
            r#"{"type":"pending_closed","workspace":"box","id":7,"state":"allowed","rule_id":2}"#,
            Some(name()),
        ),
        (
            Event::PendingClosed {
                workspace: name(),
                id: 8,
                state: PendingEnd::Expired,
                rule_id: None,
            },
            r#"{"type":"pending_closed","workspace":"box","id":8,"state":"expired","rule_id":null}"#,
            Some(name()),
        ),
        (Event::RulesChanged {}, r#"{"type":"rules_changed"}"#, None),
        (
            Event::IdentitiesChanged {},
            r#"{"type":"identities_changed"}"#,
            None,
        ),
        (
            Event::RepoListsChanged {},
            r#"{"type":"repo_lists_changed"}"#,
            None,
        ),
        (
            Event::WorkspaceGitChanged {
                workspace: WorkspaceId::new("box").unwrap(),
            },
            r#"{"type":"workspace_git_changed","workspace":"box"}"#,
            Some(WorkspaceId::new("box").unwrap()),
        ),
        (
            Event::AuditAppended { id: 99, user: 0 },
            r#"{"type":"audit_appended","id":99}"#,
            None,
        ),
        (
            Event::NetworkChanged { epoch: 4 },
            r#"{"type":"network_changed","epoch":4}"#,
            None,
        ),
        (
            Event::ProxySignInFailed {
                proxy: "proxy.corp:8080".into(),
                scheme: Some("Basic".into()),
            },
            r#"{"type":"proxy_sign_in_failed","proxy":"proxy.corp:8080","scheme":"Basic"}"#,
            None,
        ),
        (
            Event::ProxySignInFailed {
                proxy: "proxy.corp:8080".into(),
                scheme: None,
            },
            r#"{"type":"proxy_sign_in_failed","proxy":"proxy.corp:8080","scheme":null}"#,
            None,
        ),
    ] {
        assert_eq!(serde_json::to_string(&event).unwrap(), json);
        assert_eq!(serde_json::from_str::<Event>(json).unwrap(), event);
        assert_eq!(event.workspace(), workspace.as_ref());
    }
    // `rule_id` may be left out by an older sender.
    let closed: Event = serde_json::from_str(
        r#"{"type":"pending_closed","workspace":"box","id":1,"state":"denied"}"#,
    )
    .unwrap();
    assert!(matches!(closed, Event::PendingClosed { rule_id: None, .. }));
}

#[test]
fn port_events_have_fixed_shapes() {
    for (event, json, workspace) in [
        (
            Event::WorkspacePortsChanged {
                workspace: WorkspaceId::new("box").unwrap(),
            },
            r#"{"type":"workspace_ports_changed","workspace":"box"}"#,
            Some(WorkspaceId::new("box").unwrap()),
        ),
        (
            Event::WorkspacePortForwardFailed {
                workspace: WorkspaceId::new("box").unwrap(),
                guest_port: 3000,
                host_port: 3001,
                reason: "port 3001 is in use".into(),
            },
            r#"{"type":"workspace_port_forward_failed","workspace":"box","guest_port":3000,"host_port":3001,"reason":"port 3001 is in use"}"#,
            Some(WorkspaceId::new("box").unwrap()),
        ),
    ] {
        assert_eq!(serde_json::to_string(&event).unwrap(), json);
        assert_eq!(serde_json::from_str::<Event>(json).unwrap(), event);
        assert_eq!(event.workspace(), workspace.as_ref());
    }
}

#[test]
fn credential_and_git_access_events_have_fixed_shapes() {
    for (event, json, workspace) in [
        (
            Event::GlobalEnvChanged {},
            r#"{"type":"global_env_changed"}"#,
            None,
        ),
        (
            Event::WorkspaceEnvChanged {
                workspace: WorkspaceId::new("box").unwrap(),
            },
            r#"{"type":"workspace_env_changed","workspace":"box"}"#,
            Some(WorkspaceId::new("box").unwrap()),
        ),
        (
            Event::WorkspaceEnvFailed {
                workspace: WorkspaceId::new("box").unwrap(),
                reason: "cannot read the secret T".into(),
            },
            r#"{"type":"workspace_env_failed","workspace":"box","reason":"cannot read the secret T"}"#,
            Some(WorkspaceId::new("box").unwrap()),
        ),
        (
            Event::WorkspaceSshFailed {
                workspace: WorkspaceId::new("box").unwrap(),
                reason: "no SSH endpoint: the listener failed".into(),
            },
            r#"{"type":"workspace_ssh_failed","workspace":"box","reason":"no SSH endpoint: the listener failed"}"#,
            Some(WorkspaceId::new("box").unwrap()),
        ),
        (
            Event::WorkspaceSshRecovered {
                workspace: WorkspaceId::new("box").unwrap(),
            },
            r#"{"type":"workspace_ssh_recovered","workspace":"box"}"#,
            Some(WorkspaceId::new("box").unwrap()),
        ),
        (
            Event::CredentialSignInNeeded {
                host: "github.com".into(),
                source: "gh account me on github.com".into(),
            },
            r#"{"type":"credential_sign_in_needed","host":"github.com","source":"gh account me on github.com"}"#,
            None,
        ),
        (
            Event::GitAccessDenied {
                workspace: name(),
                host: "github.com".into(),
                owner: "acme".into(),
                repo: "web-shop".into(),
                access: GitAccess::Pull,
            },
            r#"{"type":"git_access_denied","workspace":"box","host":"github.com","owner":"acme","repo":"web-shop","access":"pull"}"#,
            Some(name()),
        ),
        (
            Event::GitAccessDenied {
                workspace: name(),
                host: "dev.azure.com".into(),
                owner: "acme".into(),
                repo: "proj/web".into(),
                access: GitAccess::Push,
            },
            r#"{"type":"git_access_denied","workspace":"box","host":"dev.azure.com","owner":"acme","repo":"proj/web","access":"push"}"#,
            Some(name()),
        ),
        (
            Event::GitNoIdentity {
                workspace: name(),
                host: "gitlab.com".into(),
                owner: "group".into(),
                repo: "sub/web".into(),
            },
            r#"{"type":"git_no_identity","workspace":"box","host":"gitlab.com","owner":"group","repo":"sub/web"}"#,
            Some(name()),
        ),
        (
            Event::LoginProblem {
                workspace: name(),
                service: "Claude Code".into(),
                kind: LoginProblemKind::StoreUnavailable,
            },
            r#"{"type":"login_problem","workspace":"box","service":"Claude Code","kind":"store_unavailable"}"#,
            Some(name()),
        ),
        (
            Event::LoginProblem {
                workspace: name(),
                service: "GitHub".into(),
                kind: LoginProblemKind::Unreadable,
            },
            r#"{"type":"login_problem","workspace":"box","service":"GitHub","kind":"unreadable"}"#,
            Some(name()),
        ),
    ] {
        assert_eq!(serde_json::to_string(&event).unwrap(), json);
        assert_eq!(serde_json::from_str::<Event>(json).unwrap(), event);
        assert_eq!(event.workspace(), workspace.as_ref());
    }
}

#[test]
fn a_foreign_credential_names_the_host_and_the_kind_and_nothing_else() {
    let event = Event::ForeignCredential {
        workspace: name(),
        host: "api.github.com".into(),
        kind: ForeignCredentialKind::GithubToken,
    };
    let json = r#"{"type":"foreign_credential","workspace":"box","host":"api.github.com","kind":"github_token"}"#;
    assert_eq!(serde_json::to_string(&event).unwrap(), json);
    assert_eq!(serde_json::from_str::<Event>(json).unwrap(), event);
    assert_eq!(event.workspace(), Some(&name()));
    for kind in [
        ForeignCredentialKind::StaleStandIn,
        ForeignCredentialKind::GithubToken,
        ForeignCredentialKind::NpmToken,
        ForeignCredentialKind::GitlabToken,
        ForeignCredentialKind::AzureDevopsToken,
        ForeignCredentialKind::GitAuthorization,
    ] {
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            format!("\"{}\"", kind.as_str())
        );
    }
}

#[test]
fn an_unknown_token_in_an_answer_names_the_host_and_the_kind_and_nothing_else() {
    let event = Event::UnknownTokenInAnswer {
        workspace: name(),
        host: "login.example.invalid".into(),
        kind: UnknownTokenKind::JwtWithRefreshToken,
    };
    let json = r#"{"type":"unknown_token_in_answer","workspace":"box","host":"login.example.invalid","kind":"jwt_with_refresh_token"}"#;
    assert_eq!(serde_json::to_string(&event).unwrap(), json);
    assert_eq!(serde_json::from_str::<Event>(json).unwrap(), event);
    assert_eq!(event.workspace(), Some(&name()));
    for (kind, text) in [
        (UnknownTokenKind::GithubToken, "github_token"),
        (
            UnknownTokenKind::GithubFineGrainedToken,
            "github_fine_grained_token",
        ),
        (UnknownTokenKind::GitlabToken, "gitlab_token"),
        (UnknownTokenKind::NpmToken, "npm_token"),
        (UnknownTokenKind::AzureDevopsToken, "azure_devops_token"),
        (UnknownTokenKind::AnthropicKey, "anthropic_key"),
        (UnknownTokenKind::OpenaiKey, "openai_key"),
        (
            UnknownTokenKind::JwtWithRefreshToken,
            "jwt_with_refresh_token",
        ),
    ] {
        assert_eq!(kind.as_str(), text);
        assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{text}\""));
    }
}

#[test]
fn a_repository_list_change_carries_only_the_workspace() {
    let event = Event::WorkspaceRepositoriesChanged { workspace: name() };
    let json = r#"{"type":"workspace_repositories_changed","workspace":"box"}"#;
    assert_eq!(serde_json::to_string(&event).unwrap(), json);
    assert_eq!(serde_json::from_str::<Event>(json).unwrap(), event);
    assert_eq!(event.workspace(), Some(&name()));
}

#[test]
fn shell_init_events_have_fixed_shapes() {
    for (event, json) in [
        (
            Event::WorkspaceShellInitChanged { workspace: name() },
            r#"{"type":"workspace_shell_init_changed","workspace":"box"}"#,
        ),
        (
            Event::WorkspaceShellInitRestored { workspace: name() },
            r#"{"type":"workspace_shell_init_restored","workspace":"box"}"#,
        ),
    ] {
        assert_eq!(serde_json::to_string(&event).unwrap(), json);
        assert_eq!(serde_json::from_str::<Event>(json).unwrap(), event);
        assert_eq!(event.workspace(), Some(&name()));
    }
}

#[test]
fn per_workspace_json_is_unchanged_by_global_events() {
    // Events are not persisted today, but SSE clients parse them: the wire shape of
    // existing variants must stay exactly as it was (ADR 0002).
    for (json, want) in [
        (
            r#"{"type":"status_changed","workspace":"box","status":"running"}"#,
            Event::StatusChanged {
                workspace: name(),
                status: WorkspaceStatus::Running,
            },
        ),
        (
            r#"{"type":"oom_kill","workspace":"box","pid":1,"process":"x"}"#,
            Event::oom_kill(name(), 1, "x"),
        ),
    ] {
        let got: Event = serde_json::from_str(json).unwrap();
        assert_eq!(got, want);
        assert_eq!(got.workspace(), Some(&name()));
        assert_eq!(serde_json::to_string(&got).unwrap(), json);
    }
    assert!(serde_json::from_str::<Event>(r#"{"type":"oom_kill","pid":1,"process":"x"}"#).is_err());
}

#[cfg(feature = "openapi")]
#[test]
fn openapi_schema_is_the_tagged_union() {
    use utoipa::PartialSchema;
    let schema = serde_json::to_value(Event::schema()).unwrap();
    let variants = schema["oneOf"].as_array().unwrap();
    let tags: Vec<&str> = variants
        .iter()
        .map(|v| v["properties"]["type"]["enum"][0].as_str().unwrap())
        .collect();
    assert_eq!(
        tags,
        [
            "status_changed",
            "oom_kill",
            "disk_nearly_full",
            "workspace_progress",
            "pending_opened",
            "pending_updated",
            "pending_closed",
            "rules_changed",
            "request_rules_changed",
            "identities_changed",
            "repo_lists_changed",
            "workspace_git_changed",
            "global_env_changed",
            "workspace_env_changed",
            "workspace_env_failed",
            "workspace_shell_init_changed",
            "workspace_shell_init_restored",
            "workspace_ssh_failed",
            "workspace_ssh_recovered",
            "workspace_repositories_changed",
            "workspace_ports_changed",
            "workspace_port_forward_failed",
            "pass_through_learned",
            "credential_sign_in_needed",
            "git_access_denied",
            "git_no_identity",
            "login_problem",
            "foreign_credential",
            "unknown_token_in_answer",
            "audit_appended",
            "network_changed",
            "proxy_sign_in_failed",
            "problems_changed",
            "disk_space_changed"
        ]
    );
    for v in variants {
        let required = v["required"].as_array().unwrap();
        assert!(required.iter().any(|r| r == "type"), "{v}");
    }
    let progress = &variants[3];
    assert!(
        progress["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "detail"),
        "detail is always present, null when empty: {progress}"
    );
    let global = &variants[8];
    assert!(global["properties"].get("workspace").is_none());
    assert_eq!(
        variants[0]["properties"]["workspace"]["$ref"],
        "#/components/schemas/WorkspaceId"
    );
    let status = serde_json::to_value(WorkspaceStatus::schema()).unwrap();
    assert_eq!(status["enum"].as_array().unwrap().len(), 8);
}

#[test]
fn a_disk_space_change_is_global_and_names_the_level() {
    let event = Event::DiskSpaceChanged {
        level: DiskSpaceLevel::Low,
        free_mib: 12_345,
    };
    let json = r#"{"type":"disk_space_changed","level":"low","free_mib":12345}"#;
    assert_eq!(serde_json::to_string(&event).unwrap(), json);
    assert_eq!(serde_json::from_str::<Event>(json).unwrap(), event);
    assert_eq!(event.workspace(), None);
    assert!(
        DiskSpaceLevel::Ok < DiskSpaceLevel::Low && DiskSpaceLevel::Low < DiskSpaceLevel::Critical
    );
}

#[test]
fn sinks_collect_or_drop() {
    let sink = Arc::new(CollectingSink::default());
    let as_dyn: Arc<dyn EventSink> = sink.clone();
    as_dyn.emit(Event::oom_kill(name(), 1, "x"));
    sink.emit(Event::oom_kill(name(), 2, "y"));
    assert_eq!(sink.events().len(), 2);
    assert_eq!(sink.take().len(), 2);
    assert_eq!(sink.events(), []);
    NullSink.emit(Event::oom_kill(name(), 3, "z"));
}

#[test]
fn a_learned_pass_through_host_has_a_fixed_shape() {
    let event = Event::PassThroughLearned {
        workspace: WorkspaceId::new("box").unwrap(),
        host: "pinned.example".into(),
        count: 3,
    };
    let json =
        r#"{"type":"pass_through_learned","workspace":"box","host":"pinned.example","count":3}"#;
    assert_eq!(serde_json::to_string(&event).unwrap(), json);
    assert_eq!(serde_json::from_str::<Event>(json).unwrap(), event);
    assert_eq!(event.workspace(), Some(&WorkspaceId::new("box").unwrap()));
}

#[test]
fn the_user_an_audit_event_is_for_is_never_sent() {
    assert_eq!(
        serde_json::to_string(&Event::AuditAppended { id: 7, user: 2 }).unwrap(),
        r#"{"type":"audit_appended","id":7}"#
    );
}
