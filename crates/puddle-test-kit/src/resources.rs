// SPDX-License-Identifier: GPL-3.0-or-later
//! Ports and folders that two parallel tests cannot collide on.

use std::net::{SocketAddr, TcpListener};
use std::ops::Deref;
use std::path::Path;

/// A loopback listener bound to port 0, with the port the system gave it. Hand the listener to the
/// code under test (or keep it while a child binds its own); never drop it to "free" the port, because
/// another test can take the port before the caller binds it.
///
/// # Panics
/// When the system has no loopback port to give.
#[must_use]
pub fn bound_port() -> (TcpListener, u16) {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .expect("bind a loopback port for a test");
    let port = listener
        .local_addr()
        .expect("read the port of a bound test listener")
        .port();
    (listener, port)
}

/// A temporary folder with a unique name that is removed when the value drops.
#[derive(Debug)]
pub struct TestDir(tempfile::TempDir);

impl TestDir {
    /// A new empty folder whose name starts with `label` (it shows in a failure that leaves the folder
    /// behind).
    ///
    /// # Panics
    /// When the folder cannot be made.
    #[must_use]
    pub fn new(label: &str) -> Self {
        Self(
            tempfile::Builder::new()
                .prefix(label)
                .tempdir()
                .expect("make a temporary folder for a test"),
        )
    }
}

impl Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        self.0.path()
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        self.0.path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_bound_ports_differ_and_stay_taken() {
        let (first, a) = bound_port();
        let (second, b) = bound_port();
        assert_ne!(a, b);
        assert!(TcpListener::bind(("127.0.0.1", a)).is_err());
        drop((first, second));
    }

    #[test]
    fn a_test_dir_is_unique_labelled_and_removed_on_drop() {
        let one = TestDir::new("kit-a-");
        let two = TestDir::new("kit-a-");
        assert_ne!(one.as_ref() as &Path, two.as_ref() as &Path);
        assert!(
            one.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("kit-a-")
        );
        std::fs::write(one.join("f"), "x").unwrap();
        let path = one.to_path_buf();
        drop(one);
        assert!(!path.exists());
    }
}
