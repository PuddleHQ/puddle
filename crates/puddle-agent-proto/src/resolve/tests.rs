// SPDX-License-Identifier: AGPL-3.0-or-later
use proptest::prelude::*;

use super::*;

#[test]
fn a_query_round_trips_and_keeps_its_wire_shape() {
    let q = ResolveQuery::new("_mongodb._tcp.db.example.net", RecordType::Srv);
    let line = q.to_line().unwrap();
    assert_eq!(
        line,
        b"{\"name\":\"_mongodb._tcp.db.example.net\",\"type\":\"SRV\"}\n"
    );
    assert_eq!(ResolveQuery::from_line(&line).unwrap(), q);
}

#[test]
fn every_answer_round_trips() {
    let answers = [
        ResolveAnswer::StandIn {
            why: StandInReason::ViaUpstream,
            ttl: 30,
        },
        ResolveAnswer::NoSuchName { ttl: 5 },
        ResolveAnswer::NoData { ttl: 5 },
        ResolveAnswer::Records {
            records: vec![
                Record::Srv {
                    priority: 1,
                    weight: 2,
                    port: 27017,
                    target: "shard0.example.net".into(),
                },
                Record::Txt {
                    strings: vec!["a=b".into()],
                },
                Record::Mx {
                    preference: 10,
                    exchange: "mx.example.net".into(),
                },
            ],
            ttl: 60,
        },
        ResolveAnswer::Unavailable,
    ];
    for a in answers {
        assert_eq!(ResolveAnswer::from_line(&a.to_line().unwrap()).unwrap(), a);
    }
}

#[test]
fn an_answer_type_from_the_future_is_unknown_not_an_error() {
    assert_eq!(
        ResolveAnswer::from_line(b"{\"result\":\"teleport\",\"x\":1}\n").unwrap(),
        ResolveAnswer::Unknown
    );
}

#[test]
fn garbage_and_overlong_lines_are_errors() {
    assert!(matches!(
        ResolveQuery::from_line(b"hello\n"),
        Err(ResolveError::Invalid(_))
    ));
    assert!(matches!(
        ResolveQuery::from_line(b"{\"name\":\"x\",\"type\":\"AAAA\"}\n"),
        Err(ResolveError::Invalid(_))
    ));
    let long = ResolveQuery::new("a".repeat(MAX_QUERY_LINE), RecordType::A);
    assert!(matches!(long.to_line(), Err(ResolveError::TooLong)));
    let many = ResolveAnswer::Records {
        records: vec![
            Record::Txt {
                strings: vec!["x".repeat(1000)]
            };
            MAX_RECORDS + 1
        ],
        ttl: 1,
    };
    assert!(matches!(many.to_line(), Err(ResolveError::TooLong)));
}

#[tokio::test]
async fn ask_writes_the_preamble_and_the_query_and_reads_the_answer() {
    let (client, mut server) = tokio::io::duplex(4096);
    let host = tokio::spawn(async move {
        let mut reader = BufReader::new(&mut server);
        let mut preamble = vec![0u8; PREAMBLE.len()];
        reader.read_exact(&mut preamble).await.unwrap();
        assert_eq!(preamble, PREAMBLE);
        let q = read_query(&mut reader).await.unwrap();
        assert_eq!(q, ResolveQuery::new("example.com", RecordType::A));
        let answer = ResolveAnswer::NoSuchName { ttl: 9 }.to_line().unwrap();
        server.write_all(&answer).await.unwrap();
    });
    let got = ask(
        client,
        &ResolveQuery::new("example.com", RecordType::A),
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(got, ResolveAnswer::NoSuchName { ttl: 9 });
    host.await.unwrap();
}

#[tokio::test]
async fn ask_fails_on_a_closed_stream_a_bad_answer_a_long_answer_and_silence() {
    let q = ResolveQuery::new("example.com", RecordType::A);
    let limit = Duration::from_millis(200);

    let (client, server) = tokio::io::duplex(4096);
    drop(server);
    assert!(ask(client, &q, limit).await.is_err());

    let (client, mut server) = tokio::io::duplex(4096);
    server.write_all(b"nonsense\n").await.unwrap();
    assert!(matches!(
        ask(client, &q, limit).await,
        Err(ResolveError::Invalid(_))
    ));

    let (client, mut server) = tokio::io::duplex(64 * 1024);
    server
        .write_all(&vec![b'x'; MAX_ANSWER_LINE + 10])
        .await
        .unwrap();
    assert!(matches!(
        ask(client, &q, limit).await,
        Err(ResolveError::TooLong)
    ));

    let (client, _silent) = tokio::io::duplex(4096);
    assert!(matches!(
        ask(client, &q, limit).await,
        Err(ResolveError::Timeout(_))
    ));

    let (client, mut server) = tokio::io::duplex(4096);
    server.shutdown().await.unwrap();
    assert!(matches!(
        ask(client, &q, limit).await,
        Err(ResolveError::NoAnswer)
    ));
}

#[tokio::test]
async fn read_query_refuses_a_long_line_and_a_closed_stream() {
    let overlong = vec![b'x'; MAX_QUERY_LINE + 1];
    let mut long = BufReader::new(&overlong[..]);
    assert!(matches!(
        read_query(&mut long).await,
        Err(ResolveError::TooLong)
    ));
    let mut empty = BufReader::new(&b""[..]);
    assert!(matches!(
        read_query(&mut empty).await,
        Err(ResolveError::NoAnswer)
    ));
    let mut cut = BufReader::new(&b"{\"name\""[..]);
    assert!(matches!(
        read_query(&mut cut).await,
        Err(ResolveError::Io(_))
    ));
}

proptest! {
    #[test]
    fn any_bytes_parse_or_fail_without_panicking(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
        let _ = ResolveQuery::from_line(&bytes);
        let _ = ResolveAnswer::from_line(&bytes);
    }
}
