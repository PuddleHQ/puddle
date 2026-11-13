// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

#[test]
fn the_linked_stack_is_a_number() {
    assert_eq!(linked_stack_bytes(), 8 * 1024 * 1024);
}

#[cfg(windows)]
#[test]
fn a_thread_with_too_little_stack_is_told_why_instead_of_overflowing() {
    let message = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(check_main_thread)
        .unwrap()
        .join()
        .unwrap()
        .unwrap_err();
    assert!(message.contains("/STACK"), "{message}");
}

#[cfg(windows)]
#[test]
fn a_thread_with_the_linked_stack_passes() {
    std::thread::Builder::new()
        .stack_size(linked_stack_bytes())
        .spawn(check_main_thread)
        .unwrap()
        .join()
        .unwrap()
        .unwrap();
}

#[cfg(not(windows))]
#[test]
fn elsewhere_the_check_passes() {
    check_main_thread().unwrap();
}
