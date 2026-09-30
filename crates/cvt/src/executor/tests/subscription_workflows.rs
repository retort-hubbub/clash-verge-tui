//! Subscription edits, regeneration and scoped rule changes through real effects.
use super::super::*;
use std::time::Duration;
use tokio::sync::mpsc;

fn fixture(url: &str) -> (tempfile::TempDir, Executor) {
    let home = tempfile::tempdir().unwrap();
    let service = Service::open(AppPaths::new(home.path())).unwrap();
    let mut store = service.store().unwrap();
    let item = PrfItem::remote("base", "base", url);
    store.add(item.clone());
    store
        .write_document(
            &item,
            "proxies: []\nproxy-groups: []\nrules:\n  - MATCH,DIRECT\n",
        )
        .unwrap();
    store.set_current("base").unwrap();
    store.save().unwrap();
    let outcome = service.generate().unwrap();
    service.pipeline().commit(&outcome, false).unwrap();
    (home, Executor::new(service))
}

async fn next(rx: &mut mpsc::Receiver<Event>) -> Event {
    let event = tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await
        .unwrap()
        .unwrap();
    if let Event::Failed(error) = &event {
        panic!("effect failed: {error}");
    }
    event
}

async fn subscription_server(body: &'static str) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/subscription", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = vec![0; 8192];
        let count = stream.read(&mut bytes).await.unwrap();
        let request = String::from_utf8_lossy(&bytes[..count]).to_ascii_lowercase();
        assert!(
            request.contains("user-agent:"),
            "subscription request needs a User-Agent"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/yaml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    (url, server)
}

#[tokio::test]
async fn downloaded_subscription_updates_existing_runtime_without_switching() {
    let (url, server) = subscription_server("proxies: []\nproxy-groups: []\nrules:\n  - DOMAIN,updated.example,DIRECT\n  - MATCH,DIRECT\n").await;
    let (_home, executor) = fixture(&url);
    let (sender, mut rx) = mpsc::channel(64);
    let sink = EventSink::new(sender);
    update_profiles(
        Arc::clone(&executor.service),
        vec!["base".to_owned()],
        &sink,
    )
    .await;
    assert!(matches!(
        next(&mut rx).await,
        Event::Done(Done::ProfilesUpdated {
            updated: 1,
            failed: 0
        })
    ));
    assert!(matches!(
        next(&mut rx).await,
        Event::Done(Done::ProfileContentChanged)
    ));
    executor.perform(Effect::SynchronizeConfig, &sink);
    assert!(matches!(
        next(&mut rx).await,
        Event::Done(Done::ConfigApplied { reload: None, .. })
    ));
    executor.with_service(|service| {
        assert_eq!(service.store().unwrap().current_uid(), Some("base"));
        assert!(
            service
                .paths()
                .read(&service.paths().runtime_config())
                .unwrap()
                .contains("updated.example")
        );
    });
    server.await.unwrap();
}

#[tokio::test]
async fn changing_source_fetches_new_url_and_failed_fetch_keeps_old_url() {
    let (url, server) = subscription_server("proxies: []\nproxy-groups: []\nrules:\n  - DOMAIN,new-source.example,DIRECT\n  - MATCH,DIRECT\n").await;
    let (_home, executor) = fixture("https://old.example/sub");
    let (sender, mut rx) = mpsc::channel(64);
    let sink = EventSink::new(sender);
    executor.perform(
        Effect::EditProfileSource {
            uid: "base".to_owned(),
            url: url.clone(),
        },
        &sink,
    );
    assert!(matches!(
        next(&mut rx).await,
        Event::Done(Done::ProfilesUpdated {
            updated: 1,
            failed: 0
        })
    ));
    assert!(matches!(
        next(&mut rx).await,
        Event::Done(Done::ProfileContentChanged)
    ));
    executor.with_service(|service| {
        let store = service.store().unwrap();
        let item = store.get("base").unwrap();
        assert_eq!(item.url.as_deref(), Some(url.as_str()));
        assert!(
            store
                .read_document(item)
                .unwrap()
                .contains("new-source.example")
        );
    });
    server.await.unwrap();
    executor.perform(
        Effect::EditProfileSource {
            uid: "base".to_owned(),
            url: "http://127.0.0.1:0/unreachable".to_owned(),
        },
        &sink,
    );
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(20), rx.recv())
            .await
            .unwrap()
            .unwrap(),
        Event::Failed(_)
    ));
    executor.with_service(|service| {
        assert_eq!(
            service.store().unwrap().get("base").unwrap().url.as_deref(),
            Some(url.as_str())
        )
    });
}

#[tokio::test]
async fn added_rule_is_scoped_survives_update_and_follows_subscription_switch() {
    let (_home, executor) = fixture("https://old.example/sub");
    let (sender, mut rx) = mpsc::channel(64);
    let sink = EventSink::new(sender);
    executor.perform(
        Effect::AddProfileRule {
            rule: "DOMAIN,private.example,DIRECT".to_owned(),
        },
        &sink,
    );
    assert!(matches!(
        next(&mut rx).await,
        Event::Done(Done::ProfileContentChanged)
    ));
    executor.with_service(|service| {
        let mut store = service.store().unwrap();
        let base = store.get("base").unwrap().clone();
        assert!(
            !store
                .read_document(&base)
                .unwrap()
                .contains("private.example")
        );
        let patch = store
            .items()
            .iter()
            .find(|p| p.base_scope() == Some("base"))
            .unwrap()
            .clone();
        assert!(
            store
                .read_document(&patch)
                .unwrap()
                .contains("private.example")
        );
        // A subsequent download replaces the base, while the override stays.
        store
            .write_document(
                &base,
                "proxies: []\nproxy-groups: []\nrules:\n  - MATCH,DIRECT\n",
            )
            .unwrap();
        assert!(
            service
                .generate()
                .unwrap()
                .config
                .as_value()
                .to_string()
                .contains("private.example")
        );
        let other = PrfItem::local("other", "other");
        store.add(other.clone());
        store
            .write_document(
                &other,
                "proxies: []\nproxy-groups: []\nrules:\n  - MATCH,DIRECT\n",
            )
            .unwrap();
        store.set_chain(&[patch.uid.clone()]).unwrap();
        store.set_current("other").unwrap();
        store.save().unwrap();
        assert!(
            !service
                .generate()
                .unwrap()
                .config
                .as_value()
                .to_string()
                .contains("private.example")
        );
        store.set_current("base").unwrap();
        store.save().unwrap();
        assert!(
            service
                .generate()
                .unwrap()
                .config
                .as_value()
                .to_string()
                .contains("private.example")
        );
    });
}

/// Opt in with CVT_TEST_CORE=/path/to/mihomo; never touches the user's home/core.
#[tokio::test]
async fn live_core_subscription_update_changes_running_rules_without_switching() {
    let Ok(binary) = std::env::var("CVT_TEST_CORE") else {
        eprintln!("CVT_TEST_CORE unset; real-core subscription workflow not executed");
        return;
    };
    let (url, server) = subscription_server("proxies: []\nproxy-groups: []\nrules:\n  - DOMAIN,live-updated.example,DIRECT\n  - MATCH,DIRECT\n").await;
    let (_home, executor) = fixture(&url);
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    executor.with_service(|service| {
        let mut settings = service.settings().clone();
        settings.core.binary = Some(binary.into());
        settings.core.external_controller = Some(format!("127.0.0.1:{port}"));
        settings.core.secret = Some("isolated-workflow-test".to_owned());
        service.set_settings(settings);
        let generated = service.generate().unwrap();
        service.pipeline().commit(&generated, false).unwrap();
    });
    struct StopOnDrop(Service);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            let _ = self.0.stop_core();
        }
    }
    let _cleanup = StopOnDrop(executor.with_service(|service| service.clone()));
    let (sender, mut rx) = mpsc::channel(64);
    let sink = EventSink::new(sender);
    executor.perform(Effect::StartCore, &sink);
    loop {
        if matches!(next(&mut rx).await, Event::Done(Done::CoreStarted { .. })) {
            break;
        }
    }
    let client = executor.client().unwrap();
    assert!(
        !client
            .rules()
            .await
            .unwrap()
            .iter()
            .any(|rule| rule.payload == "live-updated.example")
    );
    // The asynchronous downloader and the same effect App requests on completion.
    update_profiles(
        Arc::clone(&executor.service),
        vec!["base".to_owned()],
        &sink,
    )
    .await;
    assert!(matches!(
        next(&mut rx).await,
        Event::Done(Done::ProfilesUpdated {
            updated: 1,
            failed: 0
        })
    ));
    assert!(matches!(
        next(&mut rx).await,
        Event::Done(Done::ProfileContentChanged)
    ));
    executor.perform(Effect::SynchronizeConfig, &sink);
    assert!(
        matches!(next(&mut rx).await, Event::Done(Done::ConfigApplied { reload: Some(ref outcome), .. }) if outcome.succeeded())
    );
    assert!(
        client
            .rules()
            .await
            .unwrap()
            .iter()
            .any(|rule| rule.payload == "live-updated.example")
    );
    executor
        .with_service(|service| assert_eq!(service.store().unwrap().current_uid(), Some("base")));
    server.await.unwrap();
}

#[tokio::test]
async fn low_port_dns_start_requests_authorization_before_launch() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let (_home, executor) = fixture("https://old.example/sub");
    executor.with_service(|service| {
        let mut settings = service.settings().clone();
        settings.core.binary = Some(std::env::current_exe().unwrap());
        service.set_settings(settings);
        let store = service.store().unwrap();
        let base = store.get("base").unwrap().clone();
        store.write_document(&base, "dns: {enable: true, listen: ':53', enhanced-mode: fake-ip, nameserver: [1.1.1.1]}\nproxies: []\nproxy-groups: []\nrules:\n  - MATCH,DIRECT\n").unwrap();
        let generated = service.generate().unwrap();
        service.pipeline().commit(&generated, false).unwrap();
    });
    let (sender, mut rx) = mpsc::channel(64);
    let sink = EventSink::new(sender);
    executor.perform(Effect::StartCore, &sink);
    assert!(
        matches!(next(&mut rx).await, Event::Data(Data::CoreAuthorization { ref capabilities, ref next, .. }) if capabilities == "cap_net_bind_service" && **next == Effect::StartCore)
    );
    assert!(!executor.with_service(|service| service.core_status().is_running()));
}
