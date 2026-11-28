// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[tokio::test]
async fn without_a_factory_a_workspace_gets_puddles_git_injector_which_reads_the_store() {
    let store = store();
    let (injection, terminations) = injection(&store);
    let ws = workspace("alpha");
    injection.begin(&ws, true).await.unwrap();
    let termination = terminations.termination(&ws).unwrap();
    assert!(format!("{termination:?}").contains("GitInjector"));
    // A push to a repository that is not on the workspace's list is refused, so the injector
    // read the workspace's settings from the store.
    let host = Host::parse_normalised("github.com").unwrap();
    let context = InjectContext {
        workspace: &ws,
        host: &host,
    };
    let lines = ["host: github.com".to_owned()];
    let view = RequestView::new(
        "GET",
        "/acme/web.git/info/refs?service=git-receive-pack",
        &lines,
    );
    let injector = Arc::clone(&injection.running().get(&ws).unwrap().injector);
    let decision = injector.decide(&context, &view).await;
    assert!(
        matches!(&decision, InjectDecision::Refuse(refusal) if refusal.code() == ReasonCode::PushDenied)
    );
}
#[tokio::test]
async fn each_start_gets_an_injector_made_for_its_workspace_and_it_stays_across_changes() {
    let store = store();
    let made = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&made);
    let factory: InjectorFactory = Arc::new(move |_, workspace| {
        record.lock().unwrap().push(workspace.clone());
        Arc::new(NoInjection)
    });
    let terminations = Arc::new(Terminations::new());
    let injection = Injection::new(
        Arc::clone(&terminations),
        inputs(&store),
        Some(factory),
        Arc::new(MemoryStore::new()),
    );
    let (a, b) = (workspace("alpha"), workspace("beta"));
    injection.begin(&a, false).await.unwrap();
    injection.begin(&b, false).await.unwrap();
    assert_eq!(*made.lock().unwrap(), [a.clone(), b]);
    // A change to a running workspace keeps its injector (it may hold state of its own).
    let before = format!("{:?}", terminations.termination(&a).unwrap());
    attach(&store, &a, "ada", "github.com");
    assert!(injection.resync(&a).unwrap().is_some());
    assert_eq!(made.lock().unwrap().len(), 2);
    assert!(format!("{:?}", terminations.termination(&a).unwrap()).contains("NoInjection"));
    assert!(before.contains("NoInjection"));
    // A restart makes a new one.
    injection.begin(&a, false).await.unwrap();
    assert_eq!(made.lock().unwrap().len(), 3);
}
