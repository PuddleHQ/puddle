// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn name(tool: &str) -> String {
    let extension = if cfg!(windows) { ".exe" } else { "" };
    format!("{tool}{extension}")
}

#[tokio::test]
async fn output_larger_than_the_first_buffer_is_read_whole() {
    let data: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
    let mut reader = &data[..];
    let read = read_wiping(&mut reader).await.unwrap();
    assert_eq!(&read[..], &data[..]);
    // The buffer doubled itself three times; a Vec growing on its own would not stop at that.
    assert_eq!(read.capacity(), FIRST_READ * 8);
}

#[tokio::test]
async fn a_small_output_stays_in_the_first_buffer() {
    let mut reader = &b"token"[..];
    let read = read_wiping(&mut reader).await.unwrap();
    assert_eq!(&read[..], b"token");
    assert_eq!(read.capacity(), FIRST_READ);
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn only_an_executable_file_counts_as_a_tool() {
    let dir = tempfile::tempdir().unwrap();
    let found = dir.path().join("found");
    let plain = dir.path().join("plain");
    let folder = dir.path().join("folder");
    for d in [&found, &plain, &folder] {
        std::fs::create_dir(d).unwrap();
    }
    // `gh` runs; `git` is a file nobody may run (Unix) or is missing (Windows has no mode).
    std::fs::write(found.join(name("gh")), b"").unwrap();
    std::fs::write(plain.join(name("git")), b"").unwrap();
    // A folder with a tool's name is no tool.
    std::fs::create_dir(folder.join(name("gh"))).unwrap();
    std::fs::create_dir(folder.join(name("git"))).unwrap();
    #[cfg(unix)]
    {
        set_mode(&found.join("gh"), 0o755);
        set_mode(&plain.join("git"), 0o644);
        set_mode(&folder.join("gh"), 0o755);
        set_mode(&folder.join("git"), 0o755);
    }
    let paths = |dir: &Path| ToolPaths::from_search_path(dir.as_os_str());
    assert_eq!(paths(&found).gh, Some(found.join(name("gh"))));
    assert_eq!(paths(&found).git, None);
    assert_eq!(paths(&folder), ToolPaths::default());
    #[cfg(unix)]
    assert_eq!(paths(&plain), ToolPaths::default());
    #[cfg(windows)]
    assert_eq!(paths(&plain).git, Some(plain.join(name("git"))));
}

#[test]
fn the_process_path_is_searched_for_both_tools() {
    let from_env =
        ToolPaths::from_search_path(std::env::var_os("PATH").as_deref().unwrap_or_default());
    assert_eq!(ToolPaths::resolve(), from_env);
    assert!(
        from_env.git.is_some(),
        "git is on the PATH of every machine that builds Puddle"
    );
}
