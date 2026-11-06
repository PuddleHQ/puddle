// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows Error Reporting: the per-user list of executables that get no crash dump.

use windows_sys::Win32::System::ErrorReporting::{
    WerAddExcludedApplication, WerRemoveExcludedApplication,
};

use crate::Outcome;

/// The executables of Puddle that hold credentials. WER matches on the file name.
const PUDDLE_EXECUTABLES: [&str; 2] = ["puddle.exe", "puddle-app.exe"];

/// Puddle's executables and this process's own (a renamed copy, a test binary).
pub(crate) fn platform(allow: bool) -> Outcome {
    let mut names: Vec<String> = PUDDLE_EXECUTABLES.iter().map(|&n| n.to_owned()).collect();
    let own = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    if let Some(own) = own.filter(|own| !names.iter().any(|n| n.eq_ignore_ascii_case(own))) {
        names.push(own);
    }
    exclude(&names, allow)
}

/// Adds every name to the current user's WER exclusion list, or removes it again when `allow`.
pub(crate) fn exclude(names: &[String], allow: bool) -> Outcome {
    for name in names {
        if let Err(code) = set_excluded(name, !allow) {
            return Outcome::Failed(format!("{name}: HRESULT {code:#010x}"));
        }
    }
    if allow {
        Outcome::Allowed
    } else {
        Outcome::Protected
    }
}

/// One executable name, for the current user only (`bAllUsers` FALSE: no administrator needed).
#[expect(unsafe_code, reason = "Windows Error Reporting registration")]
fn set_excluded(name: &str, excluded: bool) -> Result<(), u32> {
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call; the second
    // argument is FALSE, so the per-user list in HKCU changes, not the machine's.
    let hr = unsafe {
        if excluded {
            WerAddExcludedApplication(wide.as_ptr(), 0)
        } else {
            WerRemoveExcludedApplication(wide.as_ptr(), 0)
        }
    };
    if hr >= 0 {
        Ok(())
    } else {
        // An HRESULT's bits, shown as hex.
        Err(u32::from_ne_bytes(hr.to_ne_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::{exclude, set_excluded};
    use crate::Outcome;

    const LIST: &str =
        r"HKCU\Software\Microsoft\Windows\Windows Error Reporting\ExcludedApplications";

    /// Whether the exclusion list of the current user names `name`, read with `reg query`.
    fn listed(name: &str) -> bool {
        Command::new("reg")
            .args(["query", LIST, "/v", name])
            .output()
            .expect("run reg.exe")
            .status
            .success()
    }

    #[test]
    fn a_name_is_listed_by_hardening_and_gone_again_with_the_opt_out() {
        let name = format!("puddle-no-dump-test-{}.exe", std::process::id());
        assert!(!listed(&name), "the name starts unlisted");
        assert_eq!(
            exclude(std::slice::from_ref(&name), false),
            Outcome::Protected
        );
        let after_hardening = listed(&name);
        assert_eq!(exclude(std::slice::from_ref(&name), true), Outcome::Allowed);
        assert!(after_hardening, "WER lists the executable after hardening");
        assert!(!listed(&name), "the opt-out removes it");
    }

    #[test]
    fn hardening_twice_is_the_same_as_once() {
        let name = format!("puddle-no-dump-twice-{}.exe", std::process::id());
        let names = [name.clone()];
        assert_eq!(exclude(&names, false), Outcome::Protected);
        assert_eq!(exclude(&names, false), Outcome::Protected);
        let listed_once = listed(&name);
        set_excluded(&name, false).expect("remove the test entry");
        assert!(listed_once);
    }
}
