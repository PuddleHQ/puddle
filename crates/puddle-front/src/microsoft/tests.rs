// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn headers(cookies: &[&str]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for c in cookies {
        h.append(header::COOKIE, HeaderValue::from_str(c).unwrap());
    }
    h
}

#[test]
fn the_key_is_sha256_of_the_two_halves() {
    let key = key(&[1; 32], &[2; 32]);
    let mut both = vec![1_u8; 32];
    both.extend([2_u8; 32]);
    assert_eq!(key.to_vec(), Sha256::digest(&both).to_vec());
    assert_ne!(key, super::key(&[2; 32], &[1; 32]));
}

#[test]
fn the_client_half_is_read_from_its_cookie_among_others() {
    let half = [7_u8; 32];
    let cookie = format!("a=b; {HALF_COOKIE}={}; c=d", URL_SAFE_NO_PAD.encode(half));
    assert_eq!(client_half(&headers(&[&cookie])), Some(half));
    assert_eq!(client_half(&headers(&["a=b"])), None);
    assert_eq!(client_half(&headers(&[])), None);
}

#[test]
fn a_half_of_the_wrong_length_or_alphabet_is_not_one() {
    for bad in [
        "puddle-secret-half=AAAA",
        "puddle-secret-half=!!!",
        "puddle-secret-half=",
    ] {
        assert_eq!(client_half(&headers(&[bad])), None, "{bad}");
    }
}

#[test]
fn the_set_cookies_point_at_the_mint_path_and_hide_the_half_from_scripts() {
    let [path, half] = set_cookies(&[9; 32]);
    assert_eq!(
        path.to_str().unwrap(),
        "vscode-secret-key-path=/_puddle/mint-key; SameSite=Strict; Path=/"
    );
    let half = half.to_str().unwrap();
    assert!(half.starts_with("puddle-secret-half="), "{half}");
    assert!(
        half.contains("HttpOnly") && half.contains("SameSite=Strict"),
        "{half}"
    );
    assert_eq!(
        client_half(&headers(&[half.split(';').next().unwrap()])),
        Some([9; 32])
    );
}

#[test]
fn stripping_removes_only_the_half() {
    let mut h = headers(&["a=b; puddle-secret-half=xyz", "c=d"]);
    strip_client_half(&mut h);
    assert_eq!(h.get(header::COOKIE).unwrap(), "a=b; c=d");
    let mut only = headers(&["puddle-secret-half=xyz"]);
    strip_client_half(&mut only);
    assert!(!only.contains_key(header::COOKIE));
    let mut none = headers(&[]);
    strip_client_half(&mut none);
    assert!(none.is_empty());
}
