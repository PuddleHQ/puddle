// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_users_ssh_config_is_in_the_home_folder_when_it_is_absolute() {
    let home = std::path::absolute("home").unwrap();
    assert_eq!(
        user_ssh_config_in(Some(&home)),
        Some(home.join(".ssh").join("config"))
    );
    assert_eq!(user_ssh_config_in(Some(Path::new("relative"))), None);
    assert_eq!(user_ssh_config_in(None), None);
    assert!(user_ssh_config().is_none_or(|own| own.ends_with(Path::new(".ssh").join("config"))));
    // The real one is the same lookup the folder helper makes for the OS's home folder.
    assert_eq!(
        user_ssh_config(),
        user_ssh_config_in(dirs::home_dir().as_deref())
    );
    assert!(
        user_ssh_config().is_some(),
        "a computer that runs the tests has a home folder"
    );
}

#[test]
fn the_folder_is_puddle_inside_the_base() {
    let base = std::path::absolute("base").unwrap();
    assert_eq!(data_dir_in(Some(&base)).unwrap(), base.join("puddle"));
}

#[test]
fn no_base_and_relative_bases_are_refused() {
    assert_eq!(data_dir_in(None), Err(DataDirError::NoBase));
    assert_eq!(
        data_dir_in(Some(Path::new("data"))),
        Err(DataDirError::Relative {
            path: "data".into()
        })
    );
    assert!(DataDirError::NoBase.to_string().contains("home"));
}

#[test]
fn the_real_folder_is_absolute_and_named_puddle() {
    // CI runners and developer machines all have a home folder.
    let dir = data_dir().unwrap();
    assert!(dir.is_absolute(), "{}", dir.display());
    assert_eq!(dir.file_name().unwrap(), "puddle");
}

#[cfg(windows)]
#[test]
fn windows_uses_local_app_data() {
    let local = std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA is set on Windows");
    assert_eq!(data_dir().unwrap(), Path::new(&local).join("puddle"));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_uses_xdg_data_home_or_the_home_folder() {
    let want = match std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        Some(xdg) if xdg.is_absolute() => xdg,
        _ => PathBuf::from(std::env::var_os("HOME").expect("HOME is set")).join(".local/share"),
    };
    assert_eq!(data_dir().unwrap(), want.join("puddle"));
}

#[cfg(target_os = "macos")]
#[test]
fn macos_uses_application_support() {
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set"));
    assert_eq!(
        data_dir().unwrap(),
        home.join("Library/Application Support/puddle")
    );
}
