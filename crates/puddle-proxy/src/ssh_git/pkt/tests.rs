// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

async fn lines(bytes: &[u8]) -> io::Result<Vec<Line>> {
    let mut reader = PktReader::new(bytes);
    let mut out = Vec::new();
    while let Some(line) = reader.next().await? {
        out.push(line);
    }
    Ok(out)
}

#[tokio::test]
async fn each_line_comes_with_the_bytes_it_was_sent_as() {
    let read = lines(b"000ahello\n000000010002000400").await;
    let err = read.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "a cut line");
    let read = lines(b"000ahello\n0000000100020004").await.unwrap();
    assert_eq!(
        read,
        vec![
            Line {
                pkt: Pkt::Data(b"hello\n".to_vec()),
                raw: b"000ahello\n".to_vec()
            },
            Line {
                pkt: Pkt::Flush,
                raw: b"0000".to_vec()
            },
            Line {
                pkt: Pkt::Delim,
                raw: b"0001".to_vec()
            },
            Line {
                pkt: Pkt::ResponseEnd,
                raw: b"0002".to_vec()
            },
            Line {
                pkt: Pkt::Data(Vec::new()),
                raw: b"0004".to_vec()
            },
        ]
    );
}

#[tokio::test]
async fn a_length_that_is_not_one_git_writes_is_refused() {
    for bad in [&b"0003"[..], b"+00a", b"zzzz", b"fff1", b"ffff", b"00 4"] {
        let err = lines(bad).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{bad:?}");
    }
    let mut longest = b"fff0".to_vec();
    longest.resize(MAX_LINE, b'x');
    assert_eq!(lines(&longest).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_stream_that_ends_inside_a_length_is_an_error_and_one_that_ends_between_lines_is_not() {
    assert_eq!(lines(b"").await.unwrap(), Vec::<Line>::new());
    let err = lines(b"00").await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn the_service_line_and_its_flush_are_measured_only_when_they_are_there() {
    let advert = b"001e# service=git-upload-pack\n0000003fabc";
    assert_eq!(service_header_len(advert), Some(34));
    // A line with no flush behind it: only the line.
    assert_eq!(
        service_header_len(b"001e# service=git-upload-pack\n0042"),
        Some(30)
    );
    // Protocol v2 and v0 over SSH start with something else.
    assert_eq!(service_header_len(b"000eversion 2\n"), Some(0));
    assert_eq!(service_header_len(b"0008abcd"), Some(0));
    assert_eq!(service_header_len(b"0000"), Some(0));
    assert_eq!(service_header_len(b"zzzz"), Some(0));
    // A line shorter than the marker is not one; one exactly as long is.
    assert_eq!(service_header_len(b"000d# service0000"), Some(0));
    assert_eq!(service_header_len(b"000e# service=0000"), Some(18));
    // Too few bytes to tell.
    for short in [
        &b""[..],
        b"001",
        b"001e# serv",
        b"001e# service=git-upload-pack\n",
        b"001e# service=git-upload-pack\n00",
    ] {
        assert_eq!(service_header_len(short), None, "{short:?}");
    }
}
