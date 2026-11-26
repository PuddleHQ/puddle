// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use puddle_services::credential_store::NoCredentialStore;

struct Panics;

impl CredentialStoreService for Panics {
    fn retry(&self) -> Option<bool> {
        panic!("the store blew up");
    }
}

struct Answers(bool);

impl CredentialStoreService for Answers {
    fn retry(&self) -> Option<bool> {
        Some(self.0)
    }
}

#[tokio::test]
async fn the_services_answer_is_passed_on() {
    for working in [true, false] {
        let reply = retry_within(Arc::new(Answers(working))).await.unwrap();
        assert_eq!(reply, CredentialStoreRetry { working });
    }
}

#[tokio::test]
async fn no_check_wired_is_unavailable() {
    let err = retry_within(Arc::new(NoCredentialStore)).await.unwrap_err();
    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn a_service_that_panics_is_a_500_not_a_broken_route() {
    let err = retry_within(Arc::new(Panics)).await.unwrap_err();
    assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
