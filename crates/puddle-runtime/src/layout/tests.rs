// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn abs(p: &str) -> PathBuf {
    std::path::absolute(p).unwrap()
}

#[test]
fn installed_layout_sits_next_to_the_exe_and_in_the_data_dir() {
    let exe = abs("app/puddle.exe");
    let data = abs("data/puddle");
    let l = RuntimeLayout::installed(&exe, &data).unwrap();
    assert_eq!(l.runtime_dir(), abs("app/runtime"));
    assert_eq!(l.msb_path(), abs("app/runtime").join(MSB_FILE_NAME));
    assert_eq!(l.home(), abs("data/puddle/msb"));
    assert_eq!(l.config_path(), abs("data/puddle/msb/config.json"));
    assert_eq!(
        l.libkrunfw_path(),
        abs("app/runtime").join(HostOs::current().runtime_files().libkrunfw)
    );
    assert_eq!(l.required_files().first(), Some(&l.msb_path()));
    assert_eq!(l.required_files().len(), if cfg!(windows) { 2 } else { 1 });
}

#[test]
fn relative_paths_are_refused() {
    let err = RuntimeLayout::new(PathBuf::from("runtime"), abs("home")).unwrap_err();
    assert_eq!(
        err,
        RuntimeError::RelativePath {
            path: "runtime".into()
        }
    );
    assert!(RuntimeLayout::new(abs("rt"), PathBuf::from("home")).is_err());
    assert!(
        RuntimeLayout::installed(Path::new("puddle.exe"), &abs("d"))
            .unwrap_err()
            .to_string()
            .contains("must be absolute")
    );
}

#[test]
fn an_exe_without_parent_is_refused() {
    let err = RuntimeLayout::installed(Path::new(""), &abs("d")).unwrap_err();
    assert!(matches!(err, RuntimeError::NoExeDir { .. }));
}

#[test]
fn file_names_come_from_the_platform_table() {
    let files = HostOs::current().runtime_files();
    assert_eq!(MSB_FILE_NAME, files.msb);
    assert_eq!(LIBKRUNFW_FILE_NAME, files.libkrunfw);
}

#[test]
fn installed_for_user_puts_the_home_in_the_data_folder() {
    let exe = abs("app/puddle");
    let l = RuntimeLayout::installed_for_user(&exe).unwrap();
    assert_eq!(l.home(), puddle_fs::data_dir().unwrap().join("msb"));
    assert_eq!(l.runtime_dir(), abs("app/runtime"));
}
