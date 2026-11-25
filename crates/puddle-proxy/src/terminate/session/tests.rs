// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn unswapped(ids: &[String]) -> Swapped {
    Swapped {
        unswapped: ids.to_vec(),
        ..Swapped::default()
    }
}

#[test]
fn the_record_lists_each_unswapped_stand_in_once_and_stops_at_the_cap() {
    let host = Host::parse_normalised("bound.test").unwrap();
    let mut outcome = Outcome::default();
    let ids: Vec<String> = (0..MAX_UNSWAPPED_STAND_INS + 5)
        .map(|n| format!("stand-in:secret:S{n}"))
        .collect();
    outcome.record_stand_ins(&host, &unswapped(&ids[..3]));
    outcome.record_stand_ins(&host, &unswapped(&ids[1..4]));
    assert_eq!(outcome.unswapped_stand_ins, ids[..4]);
    outcome.record_stand_ins(&host, &unswapped(&ids));
    assert_eq!(outcome.unswapped_stand_ins, ids[..MAX_UNSWAPPED_STAND_INS]);
}

#[test]
fn the_record_lists_each_scrubbed_stand_in_once_and_stops_at_the_cap() {
    let ids: Vec<String> = (0..MAX_UNSWAPPED_STAND_INS + 5)
        .map(|n| format!("stand-in:secret:S{n}"))
        .collect();
    let notes = |ids: &[String]| {
        let notes = ScrubNotes::default();
        for id in ids {
            notes.note(id);
        }
        notes
    };
    let mut outcome = Outcome::default();
    outcome.record_scrubbed(&notes(&ids[..3]));
    outcome.record_scrubbed(&notes(&ids[1..4]));
    assert_eq!(outcome.scrubbed_stand_ins, ids[..4]);
    outcome.record_scrubbed(&notes(&ids));
    assert_eq!(outcome.scrubbed_stand_ins, ids[..MAX_UNSWAPPED_STAND_INS]);
    assert!(outcome.refused.is_none());
}

mod stream_failures {
    //! A guest stream that fails at one exact point, scripted with
    //! [`tokio_test::io::Builder`]: the connection ends, and nothing more is written to it.
    use std::io;

    use puddle_ca::CaBuilder;
    use puddle_test_kit::capture_logs;
    use puddle_types::{Host, NullSink, WorkspaceId};
    use tokio_test::io::Builder;

    use super::super::*;
    use crate::opened::ESTABLISHED;
    use crate::terminate::{NoInjection, Termination, TerminationSet};
    use crate::testing::{AnyAddress, StaticPolicy};

    fn reset() -> io::Error {
        io::Error::from(io::ErrorKind::ConnectionReset)
    }

    fn context(mode: Mode) -> Arc<Context> {
        let workspace = WorkspaceId::new("box").unwrap();
        let ca = Arc::new(CaBuilder::new("test").build().unwrap());
        let termination = Termination::new(
            TerminationSet::parse(["bound.test"]).unwrap(),
            ca,
            Arc::new(NoInjection),
        );
        let proxy = Proxy::new(Arc::new(StaticPolicy::new()), Arc::new(NullSink))
            .with_address_check(Arc::new(AnyAddress));
        Arc::new(Context {
            proxy: Arc::new(proxy),
            termination: Arc::new(termination),
            mode,
            tls: TlsClient::new([]).unwrap(),
            workspace,
            target: Target {
                host: Host::parse_normalised("bound.test").unwrap(),
                port: 443,
            },
            admitted: Admitted::checked(Vec::new(), true),
            connections: Arc::new(tokio::sync::Semaphore::new(8)),
            streams: Arc::new(tokio::sync::Semaphore::new(8)),
        })
    }

    #[tokio::test]
    async fn a_guest_that_resets_before_its_hello_is_logged_and_nothing_is_passed_through() {
        let (logs, _guard) = capture_logs();
        let stream = Builder::new()
            .write(ESTABLISHED)
            .read_error(reset())
            .build();
        let opened = Opened {
            stream,
            early: Vec::new(),
            answered: false,
        };
        let outcome = run(context(Mode::Everything), opened).await;
        assert!(
            logs.text()
                .contains("guest connection failed before a TLS hello"),
            "{}",
            logs.text()
        );
        assert_eq!(outcome.connect, None);
        assert!(!outcome.sni_mismatch && !outcome.certificate_refused);
    }

    #[tokio::test]
    async fn a_guest_that_resets_at_the_100_continue_ends_the_body_read_and_is_logged() {
        let (logs, _guard) = capture_logs();
        let cx = context(Mode::Bound);
        let parsed = Parsed {
            method: "POST".into(),
            target: "/".into(),
            http11: true,
            body: http::Body::Length(5),
            expect_continue: true,
            close: false,
            upgrade: false,
        };
        let mut reader = BufReader::new(Builder::new().build());
        let mut writer = Builder::new().write_error(reset()).build();
        let mut wire = H1Wire {
            cx: &cx,
            reader: &mut reader,
            writer: &mut writer,
            parsed: &parsed,
        };
        let read = wire.read_body(1024).await;
        assert!(matches!(read, Err(Stop::Abort)));
        assert!(
            logs.text().contains("guest went away before its body"),
            "{}",
            logs.text()
        );
    }
}
