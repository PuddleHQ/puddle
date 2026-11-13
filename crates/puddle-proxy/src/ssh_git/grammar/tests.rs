// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn host(name: &str) -> GitHost {
    built_in(&Host::parse_normalised(name).unwrap()).unwrap_or(GitHost {
        https: Host::parse_normalised(name).unwrap(),
        form: PathForm::Plain,
    })
}

fn repo(command: &str, name: &str) -> Result<Command, Refused> {
    parse(command, &host(name))
}

/// The command a fetch or a push of `path` on `https` reads into, shaped like `parse`'s result.
#[expect(
    clippy::unnecessary_wraps,
    reason = "the tests compare it with what `parse` returns"
)]
fn git(service: GitService, https: &str, path: &str) -> Result<Command, Refused> {
    Ok(Command::Git(
        service,
        Repository {
            host: Host::parse_normalised(https).unwrap(),
            path: path.to_owned(),
        },
    ))
}

#[test]
fn the_forms_git_writes_read_into_one_https_path() {
    for arg in [
        "'acme/web.git'",
        "'/acme/web.git'",
        "'acme/web'",
        "'/acme/web'",
    ] {
        assert_eq!(
            repo(&format!("git-upload-pack {arg}"), "github.com"),
            git(GitService::Upload, "github.com", "/acme/web.git"),
            "{arg}"
        );
    }
    assert_eq!(
        repo("git-receive-pack '/acme/web.git'", "ssh.github.com"),
        git(GitService::Receive, "github.com", "/acme/web.git")
    );
    assert_eq!(
        repo("git-upload-pack '/a/b/c.git'", "gitlab.com"),
        git(GitService::Upload, "gitlab.com", "/a/b/c.git")
    );
}

#[test]
fn a_repository_may_start_with_a_dot_and_keep_its_case() {
    assert_eq!(
        repo("git-upload-pack '/Acme/.github.git'", "github.com"),
        git(GitService::Upload, "github.com", "/Acme/.github.git")
    );
}

#[test]
fn azure_devops_paths_name_the_organisation_project_and_repository() {
    assert_eq!(
        repo(
            "git-upload-pack 'v3/org/My%20Project/repo'",
            "ssh.dev.azure.com"
        ),
        git(
            GitService::Upload,
            "dev.azure.com",
            "/org/My%20Project/_git/repo"
        )
    );
    assert_eq!(
        repo(
            "git-receive-pack '/v3/org/proj/repo'",
            "vs-ssh.visualstudio.com"
        ),
        git(
            GitService::Receive,
            "org.visualstudio.com",
            "/proj/_git/repo"
        )
    );
    for bad in [
        "'org/proj/repo'",
        "'v2/org/proj/repo'",
        "'v3/org/proj'",
        "'v3/org/proj/re$po'",
        "'v3/org/proj/..'",
    ] {
        assert_eq!(
            repo(&format!("git-upload-pack {bad}"), "ssh.dev.azure.com"),
            Err(Refused::BadPath),
            "{bad}"
        );
    }
    assert_eq!(
        repo("git-upload-pack 'v3/or%20g/proj/repo'", "ssh.dev.azure.com"),
        Err(Refused::BadPath)
    );
}

#[test]
fn every_other_spelling_of_a_path_is_bad_git_path() {
    for arg in [
        "'../etc'",
        "'acme/../web'",
        "'acme//web'",
        "'./acme/web'",
        "'acme/web/'",
        "'acme'",
        "''",
        "'/'",
        "'acme/web.git.git'",
        "'acme/web%2e'",
        "'acme/we%2Fb'",
        "'acme\\web'",
        "'~acme/web'",
        "'acme/we\u{e9}b'",
        "'acme/web\n'",
        "'acme/web;id'",
        "'acme/web'\\''x'",
        "acme/web",
        "\"acme/web\"",
        "'acme/web",
        "acme/web'",
        "'acme/%20b'",
    ] {
        assert_eq!(
            repo(&format!("git-upload-pack {arg}"), "github.com"),
            Err(Refused::BadPath),
            "{arg:?}"
        );
    }
}

#[test]
fn any_other_command_is_unsupported() {
    for command in [
        "git-upload-archive 'acme/web.git'",
        "git upload-pack 'acme/web.git'",
        "git-upload-pack",
        "git-upload-pack 'acme/web.git' extra",
        "git-upload-pack  'acme/web.git'",
        "git-receive-pack 'a b/c'",
        "sh -c id",
        "; id",
        "",
        " ",
        "GIT-UPLOAD-PACK 'acme/web.git'",
        "git-upload-pack-x 'acme/web.git'",
        "scp -t /tmp",
        "git-lfs-authenticate 'acme/web.git'",
        "git-lfs-authenticate 'acme/web.git' delete",
    ] {
        assert_eq!(
            parse(command, &host("github.com")),
            Err(Refused::Unsupported),
            "{command:?}"
        );
    }
}

#[test]
fn the_lfs_commands_are_told_apart() {
    assert_eq!(
        parse(
            "git-lfs-transfer 'acme/web.git' upload",
            &host("github.com")
        ),
        Ok(Command::LfsTransfer)
    );
    assert_eq!(
        parse("git-lfs-transfer", &host("github.com")),
        Ok(Command::LfsTransfer)
    );
    assert_eq!(
        parse(
            "git-lfs-authenticate 'acme/web.git' upload",
            &host("github.com")
        ),
        Ok(Command::LfsAuthenticate {
            repository: Repository {
                host: Host::parse_normalised("github.com").unwrap(),
                path: "/acme/web.git".to_owned(),
            },
            upload: true,
        })
    );
    assert_eq!(
        parse(
            "git-lfs-authenticate 'acme/../web.git' download",
            &host("github.com")
        ),
        Err(Refused::BadPath)
    );
}

#[test]
fn the_audit_spelling_is_lower_case_without_dot_git() {
    let plain = repository("'Acme/Web.git'", &host("github.com")).unwrap();
    assert_eq!(plain.audit_path(), "/acme/web");
    let azure = repository("'v3/Org/Proj/Repo'", &host("ssh.dev.azure.com")).unwrap();
    assert_eq!(azure.audit_path(), "/org/proj/repo");
}

#[test]
fn the_built_in_ssh_names_are_every_built_in_host_once() {
    let names = built_in_names();
    assert_eq!(names.len(), BUILT_IN.len());
    for name in ["github.com", "ssh.github.com", "ssh.dev.azure.com"] {
        assert!(
            names.contains(&Host::parse_normalised(name).unwrap()),
            "{name}"
        );
    }
    assert_eq!(crate::git_ssh_built_in_hosts(), names);
}

#[test]
fn the_built_in_names_map_to_their_https_hosts() {
    for (ssh, https) in [
        ("github.com", "github.com"),
        ("ssh.github.com", "github.com"),
        ("altssh.gitlab.com", "gitlab.com"),
        ("altssh.bitbucket.org", "bitbucket.org"),
        ("ssh.dev.azure.com", "dev.azure.com"),
    ] {
        let found = built_in(&Host::parse_normalised(ssh).unwrap()).unwrap();
        assert_eq!(found.https.to_string(), https);
    }
    for other in [
        "api.github.com",
        "example.com",
        "dev.azure.com",
        "github.io",
    ] {
        assert!(built_in(&Host::parse_normalised(other).unwrap()).is_none());
    }
}
