// SPDX-License-Identifier: AGPL-3.0-or-later
use proptest::prelude::*;

use super::*;

#[test]
fn every_kind_round_trips_through_its_preamble() {
    for kind in StreamKind::ALL {
        let p = kind.preamble();
        assert!(p.len() <= MAX_PREAMBLE);
        assert_eq!(p.first(), Some(&0));
        assert_eq!(parse_preamble(p), Ok((kind, kind.version())));
    }
}

#[test]
fn unknown_kinds_and_bad_shapes_are_errors() {
    assert_eq!(
        parse_preamble(b"\0puddle-telepathy/1\n"),
        Err(PreambleError::UnknownKind("telepathy".into()))
    );
    assert_eq!(
        parse_preamble(b"\0puddle-\x07x/1\n"),
        Err(PreambleError::UnknownKind("?x".into()))
    );
    for bad in [
        &b""[..],
        b"\0puddle-control/1",
        b"\0puddle-control\n",
        b"\0puddle-control/x\n",
        b"puddle-control/1\n",
        b"\0puddle-\xff/1\n",
    ] {
        assert_eq!(
            parse_preamble(bad),
            Err(PreambleError::Malformed),
            "{bad:?}"
        );
    }
    assert_eq!(
        parse_preamble(b"\0puddle-control/2\n"),
        Ok((StreamKind::Control, 2))
    );
}

proptest! {
    #[test]
    fn any_bytes_parse_or_fail_without_panicking(bytes in proptest::collection::vec(any::<u8>(), 0..80)) {
        let _ = parse_preamble(&bytes);
    }
}
