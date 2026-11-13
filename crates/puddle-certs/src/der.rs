// SPDX-License-Identifier: AGPL-3.0-or-later
//! Just enough DER to select roots: issuer, subject, `notAfter`, public key and common name of an
//! X.509 certificate. Input is the host's own certificate stores, but any bytes are handled
//! without panicking (property test below).

/// What a certificate that can't be read lacks, for the skipped list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not an X.509 certificate: {0}")]
pub(crate) struct Malformed(pub(crate) &'static str);

/// The fields root selection uses. Names and the key are the raw DER (tag included), so equal
/// bytes mean equal names, as in the chain engines' own issuer matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CertFields<'a> {
    pub(crate) issuer: &'a [u8],
    pub(crate) subject: &'a [u8],
    pub(crate) spki: &'a [u8],
    /// Seconds since the Unix epoch.
    pub(crate) not_after: i64,
    pub(crate) subject_cn: Option<String>,
}

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const INTEGER: u8 = 0x02;
const OID: u8 = 0x06;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
const VERSION: u8 = 0xa0;
/// 2.5.4.3, `commonName`.
const OID_CN: &[u8] = &[0x55, 0x04, 0x03];

/// One TLV: its tag, its contents, and the input after it.
struct Tlv<'a> {
    tag: u8,
    contents: &'a [u8],
    whole: &'a [u8],
    rest: &'a [u8],
}

fn tlv(input: &[u8]) -> Result<Tlv<'_>, Malformed> {
    let (&tag, after_tag) = input.split_first().ok_or(Malformed("truncated"))?;
    if tag & 0x1f == 0x1f {
        return Err(Malformed("multi-byte tag"));
    }
    let (&first, after_len) = after_tag.split_first().ok_or(Malformed("truncated"))?;
    let (len, body) = if first < 0x80 {
        (usize::from(first), after_len)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 {
            return Err(Malformed("unsupported length"));
        }
        let (bytes, body) = after_len
            .split_at_checked(n)
            .ok_or(Malformed("truncated"))?;
        let len = bytes
            .iter()
            .fold(0_usize, |acc, &b| (acc << 8) | usize::from(b));
        (len, body)
    };
    let (contents, rest) = body.split_at_checked(len).ok_or(Malformed("truncated"))?;
    let header = input.len() - body.len();
    let whole = input.get(..header + len).ok_or(Malformed("truncated"))?;
    Ok(Tlv {
        tag,
        contents,
        whole,
        rest,
    })
}

fn expect<'a>(input: &'a [u8], tag: u8, what: &'static str) -> Result<Tlv<'a>, Malformed> {
    let t = tlv(input)?;
    if t.tag == tag {
        Ok(t)
    } else {
        Err(Malformed(what))
    }
}

/// Reads `der` (one certificate).
pub(crate) fn parse(der: &[u8]) -> Result<CertFields<'_>, Malformed> {
    let cert = expect(der, SEQUENCE, "no certificate sequence")?;
    if !cert.rest.is_empty() {
        return Err(Malformed("trailing bytes"));
    }
    let tbs = expect(cert.contents, SEQUENCE, "no tbsCertificate")?;
    let mut rest = tbs.contents;
    if rest.first() == Some(&VERSION) {
        rest = tlv(rest)?.rest;
    }
    let serial = expect(rest, INTEGER, "no serial number")?;
    let algorithm = expect(serial.rest, SEQUENCE, "no signature algorithm")?;
    let issuer = expect(algorithm.rest, SEQUENCE, "no issuer")?;
    let validity = expect(issuer.rest, SEQUENCE, "no validity")?;
    let subject = expect(validity.rest, SEQUENCE, "no subject")?;
    let spki = expect(subject.rest, SEQUENCE, "no public key")?;

    let not_before = tlv(validity.contents)?;
    let not_after = tlv(not_before.rest)?;
    Ok(CertFields {
        issuer: issuer.whole,
        subject: subject.whole,
        spki: spki.whole,
        not_after: time(&not_after)?,
        subject_cn: common_name(subject.contents),
    })
}

/// `UTCTime` (`YYMMDDHHMMSSZ`, 1950–2049) or `GeneralizedTime` (`YYYYMMDDHHMMSSZ`) as Unix
/// seconds.
fn time(t: &Tlv<'_>) -> Result<i64, Malformed> {
    let digits = |s: &[u8]| -> Result<i64, Malformed> {
        s.iter().try_fold(0_i64, |acc, &b| {
            if b.is_ascii_digit() {
                Ok(acc * 10 + i64::from(b - b'0'))
            } else {
                Err(Malformed("bad time"))
            }
        })
    };
    let (year, rest) = match (t.tag, t.contents.len()) {
        (UTC_TIME, 13) => {
            let (yy, rest) = t.contents.split_at(2);
            let yy = digits(yy)?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, rest)
        }
        (GENERALIZED_TIME, 15) => {
            let (yyyy, rest) = t.contents.split_at(4);
            (digits(yyyy)?, rest)
        }
        _ => return Err(Malformed("bad time")),
    };
    let field = |i: usize| {
        rest.get(i..i + 2)
            .ok_or(Malformed("bad time"))
            .and_then(digits)
    };
    let (month, day, hour, minute, second) =
        (field(0)?, field(2)?, field(4)?, field(6)?, field(8)?);
    if rest.get(10) != Some(&b'Z')
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return Err(Malformed("bad time"));
    }
    Ok(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The first `commonName` in a Name's contents, for display; control characters become `?`.
fn common_name(mut rdns: &[u8]) -> Option<String> {
    while !rdns.is_empty() {
        let set = expect(rdns, SET, "").ok()?;
        rdns = set.rest;
        let mut atvs = set.contents;
        while !atvs.is_empty() {
            let atv = expect(atvs, SEQUENCE, "").ok()?;
            atvs = atv.rest;
            let oid = expect(atv.contents, OID, "").ok()?;
            if oid.contents != OID_CN {
                continue;
            }
            let value = tlv(oid.rest).ok()?;
            let text = match value.tag {
                // BMPString: UTF-16BE.
                0x1e => String::from_utf16_lossy(
                    &value
                        .contents
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_be_bytes(*c))
                        .collect::<Vec<_>>(),
                ),
                _ => String::from_utf8_lossy(value.contents).into_owned(),
            };
            return Some(
                text.chars()
                    .map(|c| if c.is_control() { '?' } else { c })
                    .collect(),
            );
        }
    }
    None
}

#[cfg(test)]
mod tests;
