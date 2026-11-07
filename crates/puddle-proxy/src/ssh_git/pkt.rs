// SPDX-License-Identifier: GPL-3.0-or-later
//! Git's pkt-lines (gitprotocol-common), read off the guest's side of a command so the HTTPS
//! bridge can tell where one request ends. Each line keeps the bytes it came as, so what goes
//! upstream is what the guest sent.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt as _};

/// The longest pkt-line, its length field included (`LARGE_PACKET_MAX` in Git's `pkt-line.h`).
const MAX_LINE: usize = 65520;

/// One pkt-line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pkt {
    /// `0000`: the end of a section or a request.
    Flush,
    /// `0001`: between a protocol-v2 command's capabilities and its arguments.
    Delim,
    /// `0002`: the end of a protocol-v2 response.
    ResponseEnd,
    /// A line with its payload.
    Data(Vec<u8>),
}

/// A pkt-line and the bytes it came as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    pub(crate) pkt: Pkt,
    pub(crate) raw: Vec<u8>,
}

/// Reads pkt-lines from `R`.
#[derive(Debug)]
pub(crate) struct PktReader<R> {
    inner: R,
}

impl<R: AsyncRead + Unpin> PktReader<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self { inner }
    }

    /// The stream under the reader, for what follows the pkt-lines (a pack).
    pub(crate) fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// The next line; `None` when the stream ends where a line would start. A stream that ends
    /// inside a line, or a length that is not four hex digits of a valid size, is an error.
    pub(crate) async fn next(&mut self) -> io::Result<Option<Line>> {
        let mut head = [0_u8; 4];
        if self.inner.read(&mut head[..1]).await? == 0 {
            return Ok(None);
        }
        self.inner.read_exact(&mut head[1..]).await?;
        let pkt = match length(head)? {
            0 => Pkt::Flush,
            1 => Pkt::Delim,
            2 => Pkt::ResponseEnd,
            len => {
                let mut data = vec![0_u8; len - 4];
                self.inner.read_exact(&mut data).await?;
                Pkt::Data(data)
            }
        };
        let mut raw = head.to_vec();
        if let Pkt::Data(data) = &pkt {
            raw.extend_from_slice(data);
        }
        Ok(Some(Line { pkt, raw }))
    }
}

/// The length a pkt-line's four hex digits give: 0, 1 or 2 for the special lines, else at least 4
/// (the length field itself) and at most [`MAX_LINE`].
fn length(head: [u8; 4]) -> io::Result<usize> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "not a pkt-line length");
    if !head.iter().all(u8::is_ascii_hexdigit) {
        return Err(bad());
    }
    let text = std::str::from_utf8(&head).map_err(|_| bad())?;
    let len = usize::from_str_radix(text, 16).map_err(|_| bad())?;
    match len {
        0..=2 | 4..=MAX_LINE => Ok(len),
        _ => Err(bad()),
    }
}

/// The length of a smart-HTTP advertisement's `# service=` line and the flush behind it at the
/// start of `start`, which SSH does not send; `0` when `start` begins otherwise, `None` when more
/// bytes are needed to tell. A protocol-v2 advertisement has no such line.
pub(crate) fn service_header_len(start: &[u8]) -> Option<usize> {
    const SERVICE: &[u8] = b"# service=";
    let head: [u8; 4] = start.get(..4)?.try_into().ok()?;
    let Ok(len) = length(head) else {
        return Some(0);
    };
    if len < 4 + SERVICE.len() {
        return Some(0);
    }
    let payload = start.get(4..4 + SERVICE.len())?;
    if payload != SERVICE {
        return Some(0);
    }
    let flush = start.get(len..len + 4)?;
    Some(if flush == b"0000" { len + 4 } else { len })
}

#[cfg(test)]
mod tests {
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
    async fn a_stream_that_ends_inside_a_length_is_an_error_and_one_that_ends_between_lines_is_not()
    {
        assert!(lines(b"").await.unwrap().is_empty());
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
}
