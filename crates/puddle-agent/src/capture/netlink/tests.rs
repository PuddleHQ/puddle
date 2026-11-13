// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_nested_attribute_covers_its_children_and_every_attribute_is_aligned() {
    let mut w = Writer::default();
    let start = w.begin(0x0a00, F_REQUEST, 7);
    w.nfgen(1, 0);
    w.nested(4, |w| {
        w.string(1, "ab");
        w.be32(2, 5);
    });
    w.end(start);
    let bytes = w.finish();
    assert_eq!(bytes.len() % 4, 0);
    let all = messages(&bytes);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].ty, 0x0a00);
    assert_eq!(all[0].seq, 7);
    let inner = attrs(&all[0].body[4..]);
    assert_eq!(inner.len(), 1);
    assert_eq!(inner[0].0, 4);
    let children = attrs(inner[0].1);
    assert_eq!(children[0], (1, &b"ab\0"[..]));
    assert_eq!(children[1], (2, &[0, 0, 0, 5][..]));
}

#[test]
fn a_truncated_buffer_yields_only_whole_messages() {
    let mut w = Writer::default();
    let a = w.begin(1, 0, 1);
    w.end(a);
    let mut bytes = w.finish();
    let whole = bytes.len();
    bytes.extend_from_slice(&[64, 0, 0, 0, 1, 0]);
    assert_eq!(messages(&bytes).len(), 1);
    assert_eq!(messages(&bytes[..whole - 1]).len(), 0);
    assert_eq!(attrs(&[9, 0, 1, 0, 1]).len(), 0);
    assert_eq!(
        messages(&[0u8; 16]).len(),
        0,
        "a message shorter than its own header"
    );
}

#[test]
fn acknowledgements_and_errors_are_told_apart() {
    let mut body = 0i32.to_ne_bytes().to_vec();
    body.extend_from_slice(&[0; 16]);
    let ok = Message {
        ty: ERROR,
        seq: 3,
        body: &body,
    };
    assert_eq!(ack(&ok), Some(Ok(())));
    let mut body = (-17i32).to_ne_bytes().to_vec();
    body.extend_from_slice(&[0; 16]);
    let bad = Message {
        ty: ERROR,
        seq: 4,
        body: &body,
    };
    assert_eq!(ack(&bad), Some(Err((17, 4))));
    let other = Message {
        ty: 9,
        seq: 0,
        body: &[],
    };
    assert_eq!(ack(&other), None);
    let short = Message {
        ty: ERROR,
        seq: 5,
        body: &[1],
    };
    assert_eq!(ack(&short), Some(Err((71, 5))));
}
