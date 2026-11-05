// SPDX-License-Identifier: GPL-3.0-or-later
//! The account a program the agent starts runs as: the workspace's session user, read from the
//! guest's own `/etc/passwd` and `/etc/group` (the boot hook wrote the user there).

use std::io;
use std::path::Path;

/// A user of the guest, with what a program started as that user needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// The login name.
    pub name: String,
    /// The user id.
    pub uid: u32,
    /// The primary group id.
    pub gid: u32,
    /// Every group the user is in, the primary group first.
    pub groups: Vec<u32>,
    /// The home folder.
    pub home: String,
    /// The login shell (`/bin/sh` when the entry names none).
    pub shell: String,
}

impl Account {
    /// The user `name` as `passwd` and `group` (the two files' text) list it; `None` when
    /// `passwd` has no usable entry for it.
    #[must_use]
    pub fn find(name: &str, passwd: &str, group: &str) -> Option<Self> {
        let (uid, gid, home, shell) = passwd.lines().find_map(|line| {
            match line.split(':').collect::<Vec<_>>().as_slice() {
                [user, _, uid, gid, _, home, shell, ..] if *user == name => {
                    Some((uid.parse().ok()?, gid.parse().ok()?, *home, *shell))
                }
                _ => None,
            }
        })?;
        let mut groups = vec![gid];
        for line in group.lines() {
            if let [_, _, id, members, ..] = line.split(':').collect::<Vec<_>>().as_slice()
                && members.split(',').any(|member| member == name)
                && let Ok(id) = id.parse()
                && !groups.contains(&id)
            {
                groups.push(id);
            }
        }
        Some(Self {
            name: name.to_owned(),
            uid,
            gid,
            groups,
            home: home.to_owned(),
            shell: if shell.is_empty() { "/bin/sh" } else { shell }.to_owned(),
        })
    }

    /// The user `name` of this system.
    ///
    /// # Errors
    ///
    /// When `/etc/passwd` can't be read or has no entry for `name`.
    pub fn read(name: &str) -> io::Result<Self> {
        let passwd = std::fs::read_to_string("/etc/passwd")?;
        // An image with no group file still has the primary group.
        let group = std::fs::read_to_string("/etc/group").unwrap_or_default();
        Self::find(name, &passwd, &group).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("/etc/passwd has no usable entry for {name}"),
            )
        })
    }

    /// The variables a login as this user sets.
    #[must_use]
    pub fn env(&self) -> [(&'static str, &str); 4] {
        [
            ("HOME", &self.home),
            ("USER", &self.name),
            ("LOGNAME", &self.name),
            ("SHELL", &self.shell),
        ]
    }

    /// Gives `path` (not what is below it) to this user and its primary group.
    ///
    /// # Errors
    ///
    /// When the owner can't be changed.
    pub fn own(&self, path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::chown(path, Some(self.uid), Some(self.gid))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(())
        }
    }

    /// Makes `command` run as this user: its groups, then its group, then its user, set in the
    /// child before the program starts. Std's own `uid` and `gid` can't be used: they run before
    /// the child's hook and have no supplementary groups on stable Rust, so a hook after them could
    /// no longer set the groups.
    #[cfg(target_os = "linux")]
    #[expect(
        unsafe_code,
        reason = "pre_exec runs in the forked child; setgroups, setgid and setuid through libc"
    )]
    pub fn run_as(&self, command: &mut tokio::process::Command) {
        let groups: Vec<libc::gid_t> = self.groups.clone();
        let (uid, gid) = (self.uid, self.gid);
        // SAFETY: the closure runs between fork and exec, so it may only make async-signal-safe
        // calls: it makes three system calls on values prepared before the fork and allocates
        // nothing.
        unsafe {
            command.pre_exec(move || {
                if libc::setgroups(groups.len(), groups.as_ptr()) != 0
                    || libc::setgid(gid) != 0
                    || libc::setuid(uid) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\n\
        vscode:x:1000:1000::/home/vscode:/bin/bash\n\
        agent:x:1001:1001::/home/agent:\n";
    const GROUP: &str = "root:x:0:\n\
        docker:x:998:vscode,agent\n\
        agent:x:1001:\n\
        audio:x:29:agent\n\
        broken\n";

    #[test]
    fn the_user_gets_its_ids_home_and_every_group_it_is_listed_in() {
        let agent = Account::find("agent", PASSWD, GROUP).unwrap();
        assert_eq!(
            agent,
            Account {
                name: "agent".to_owned(),
                uid: 1001,
                gid: 1001,
                groups: vec![1001, 998, 29],
                home: "/home/agent".to_owned(),
                shell: "/bin/sh".to_owned(),
            }
        );
        assert_eq!(
            agent.env(),
            [
                ("HOME", "/home/agent"),
                ("USER", "agent"),
                ("LOGNAME", "agent"),
                ("SHELL", "/bin/sh"),
            ]
        );
        let vscode = Account::find("vscode", PASSWD, "").unwrap();
        assert_eq!(
            (vscode.groups, vscode.shell.as_str()),
            (vec![1000], "/bin/bash")
        );
    }

    #[test]
    fn a_missing_or_unusable_entry_is_no_account() {
        assert_eq!(Account::find("nobody", PASSWD, GROUP), None);
        assert_eq!(
            Account::find("agent", "agent:x:one:1001::/h:/bin/sh\n", ""),
            None
        );
        assert_eq!(Account::find("agent", "agent:x:1001\n", ""), None);
        // A name that is only a prefix of an entry is not that entry.
        assert_eq!(Account::find("age", PASSWD, GROUP), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_systems_root_is_read_and_a_user_it_lacks_is_not_found() {
        let root = Account::read("root").unwrap();
        assert_eq!((root.uid, root.gid, root.groups[0]), (0, 0, 0));
        let missing = Account::read("puddle-no-such-user").unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        assert!(
            missing.to_string().contains("puddle-no-such-user"),
            "{missing}"
        );
    }
}
