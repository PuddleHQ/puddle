// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_request_is_the_preamble_and_the_port() {
    assert_eq!(request(65_535), b"\0puddle-forward/1\n65535\n");
    assert!(PREAMBLE.len() <= crate::kind::MAX_PREAMBLE);
}

#[test]
fn only_a_plain_port_number_parses() {
    assert_eq!(parse_request(b"8080\n"), Ok(8080));
    assert_eq!(parse_request(b"1"), Ok(1));
    for bad in [
        &b""[..],
        b"\n",
        b"0\n",
        b"65536\n",
        b"-1\n",
        b"+80\n",
        b" 80\n",
        b"80 \n",
        b"0x50\n",
        b"\xff\n",
        b"1e3\n",
    ] {
        assert_eq!(parse_request(bad), Err(LineError::BadPort), "{bad:?}");
    }
}

#[test]
fn answers_round_trip_and_a_reason_is_one_clean_line() {
    assert_eq!(Answer::from_line(&Answer::Ok.to_line()), Ok(Answer::Ok));
    let refused = Answer::refused("nothing\r\nlistens\x07 here");
    assert_eq!(refused, Answer::Refused("nothinglistens here".into()));
    assert_eq!(Answer::from_line(&refused.to_line()), Ok(refused));
    for bad in [&b"yes\n"[..], b"", b"refused\n", b"\xff", b"OK\n"] {
        assert_eq!(Answer::from_line(bad), Err(LineError::BadAnswer), "{bad:?}");
    }
    assert!(Answer::refused(&"x".repeat(10_000)).to_line().len() <= MAX_ANSWER_LINE);
}

#[tokio::test]
async fn a_line_is_read_without_taking_what_follows() {
    let mut input: &[u8] = b"ok\nDATA";
    assert_eq!(read_line_exact(&mut input, 16).await.unwrap(), b"ok\n");
    assert_eq!(input, b"DATA");
    let mut long: &[u8] = b"aaaaaaaaaaaaaaaaaaaa\n";
    assert_eq!(
        read_line_exact(&mut long, 8).await.unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let mut cut: &[u8] = b"ok";
    assert_eq!(
        read_line_exact(&mut cut, 8).await.unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}
