//! Profile selection, rejected listeners and exact runtime recovery.
use cvt_core::profile::item::PrfItem;
use cvt_core::{AppPaths, ReloadMode, Service};
use std::net::TcpListener;

struct Fixture {
    _home: tempfile::TempDir,
    service: Service,
}

impl Fixture {
    fn new(binary: &std::path::Path) -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut service = Service::open(AppPaths::new(home.path())).unwrap();
        let mut settings = service.settings().clone();
        let controller = TcpListener::bind("127.0.0.1:0").unwrap();
        settings.core.external_controller = Some(controller.local_addr().unwrap().to_string());
        settings.core.binary = Some(binary.to_path_buf());
        service.set_settings(settings);
        drop(controller);
        let mut store = service.store().unwrap();
        for (uid, mode) in [("old", "direct"), ("new", "rule")] {
            let item = PrfItem::local(uid, uid);
            store.add(item.clone());
            store.write_document(&item, &format!(
                "mode: {mode}\ndns: {{enable: false}}\ntun: {{enable: false}}\nproxies: []\nproxy-groups: []\nrules: [MATCH,DIRECT]\n"
            ).replace("rules: [MATCH,DIRECT]", "rules: ['MATCH,DIRECT']")).unwrap();
        }
        store.set_current("old").unwrap();
        store.save().unwrap();
        let outcome = service.generate().unwrap();
        service.pipeline().commit(&outcome, false).unwrap();
        Self {
            _home: home,
            service,
        }
    }

    fn occupied_dns(&self) -> TcpListener {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let store = self.service.store().unwrap();
        let item = store.get("new").unwrap();
        store.write_document(item, &format!(
            "mode: rule\ndns: {{enable: true, listen: '{}', nameserver: [1.1.1.1]}}\ntun: {{enable: false}}\nproxies: []\nproxy-groups: []\nrules: ['MATCH,DIRECT']\n",
            listener.local_addr().unwrap()
        )).unwrap();
        listener
    }

    fn assert_current(&self, uid: &str) {
        assert_eq!(self.service.store().unwrap().current_uid(), Some(uid));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.service.stop_core();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn refused_switch_preserves_selection_and_runtime() {
    let f = Fixture::new(std::path::Path::new("/bin/true"));
    let before = f
        .service
        .paths()
        .read(&f.service.paths().runtime_config())
        .unwrap();
    let _occupied = f.occupied_dns();
    let error = f
        .service
        .switch_profile("new", ReloadMode::Auto)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already in use"), "{error}");
    f.assert_current("old");
    assert_eq!(
        f.service
            .paths()
            .read(&f.service.paths().runtime_config())
            .unwrap(),
        before
    );
    // Read persisted state again, as a fresh process would.
    let reopened = Service::open(f.service.paths().clone()).unwrap();
    assert_eq!(reopened.store().unwrap().current_uid(), Some("old"));
}

#[cfg(unix)]
#[tokio::test]
async fn refused_switch_does_not_promote_previous_only_runtime() {
    let f = Fixture::new(std::path::Path::new("/bin/true"));
    std::fs::rename(
        f.service.paths().runtime_config(),
        f.service.paths().runtime_config_previous(),
    )
    .unwrap();
    let before = f
        .service
        .paths()
        .read(&f.service.paths().runtime_config_previous())
        .unwrap();
    let _occupied = f.occupied_dns();
    assert!(
        f.service
            .switch_profile("new", ReloadMode::Auto)
            .await
            .is_err()
    );
    f.assert_current("old");
    assert!(!f.service.paths().runtime_config().exists());
    assert_eq!(
        f.service
            .paths()
            .read(&f.service.paths().runtime_config_previous())
            .unwrap(),
        before
    );
}

#[cfg(unix)]
#[tokio::test]
async fn successful_stopped_switch_deploys_before_persisting_selection() {
    let f = Fixture::new(std::path::Path::new("/bin/true"));
    assert_eq!(
        f.service
            .switch_profile("new", ReloadMode::Auto)
            .await
            .unwrap(),
        "new"
    );
    f.assert_current("new");
    let config = cvt_core::model::config::Config::from_yaml(
        &f.service
            .paths()
            .read(&f.service.paths().runtime_config())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(config.mode(), "rule");
    assert!(!f.service.core_status().is_running());
}

#[cfg(unix)]
#[test]
fn starting_rejects_bad_selected_profile_instead_of_launching_old_runtime() {
    let f = Fixture::new(std::path::Path::new("/bin/true"));
    let before = f
        .service
        .paths()
        .read(&f.service.paths().runtime_config())
        .unwrap();
    let _occupied = f.occupied_dns();
    let mut store = f.service.store().unwrap();
    // Reproduce state left persisted by the older nontransactional interface.
    store.set_current("new").unwrap();
    store.save().unwrap();
    let error = f.service.start_core().unwrap_err();
    assert!(error.to_string().contains("already in use"), "{error}");
    assert!(!f.service.core_status().is_running());
    assert_eq!(
        f.service
            .paths()
            .read(&f.service.paths().runtime_config())
            .unwrap(),
        before
    );
}

#[cfg(unix)]
#[tokio::test]
async fn first_apply_failure_never_uses_unrelated_historic_snapshot() {
    use std::os::unix::fs::PermissionsExt;
    let mut f = Fixture::new(std::path::Path::new("/bin/false"));
    let binary = f.service.paths().core_dir().join("refuse-launch");
    std::fs::write(
        &binary,
        "#!/bin/sh\nif [ \"$1\" = \"-t\" ]; then exit 0; fi\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut settings = f.service.settings().clone();
    settings.core.binary = Some(binary);
    f.service.set_settings(settings);
    let snapshot = f.service.paths().snapshots_dir().join("config-old.yaml");
    f.service
        .paths()
        .write_atomic(&snapshot, "mode: global\nrules: ['MATCH,DIRECT']\n")
        .unwrap();
    std::fs::remove_file(f.service.paths().runtime_config()).unwrap();
    let error = f
        .service
        .apply(false, ReloadMode::Restart)
        .await
        .unwrap_err();
    assert!(
        matches!(error, cvt_core::Error::InvalidValue { field: "core", .. }),
        "{error}"
    );
    let runtime = f
        .service
        .paths()
        .read(&f.service.paths().runtime_config())
        .unwrap();
    assert_eq!(
        cvt_core::model::config::Config::from_yaml(&runtime)
            .unwrap()
            .mode(),
        "direct"
    );
    assert!(!f.service.core_status().is_running());
}

/// Opt in to a real core with no TUN, DNS hijack, system proxy or remote probes.
#[tokio::test]
async fn live_restart_and_rejected_switch_restore_the_running_profile() {
    let Some(binary) = std::env::var_os("CVT_TEST_MIHOMO") else {
        return;
    };
    let f = Fixture::new(std::path::Path::new(&binary));
    f.service.start_core().unwrap();
    f.service.wait_until_ready().await.unwrap();
    for _ in 0..5 {
        f.service.restart_core().unwrap();
        f.service.wait_until_ready().await.unwrap();
    }
    let before = f
        .service
        .paths()
        .read(&f.service.paths().runtime_config())
        .unwrap();
    let occupied = f.occupied_dns();
    let error = f
        .service
        .switch_profile("new", ReloadMode::Restart)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already in use"), "{error}");
    f.assert_current("old");
    assert_eq!(
        f.service
            .paths()
            .read(&f.service.paths().runtime_config())
            .unwrap(),
        before
    );
    assert!(f.service.core_status().is_running());
    assert_eq!(
        f.service
            .client()
            .unwrap()
            .configs()
            .await
            .unwrap()
            .mode
            .as_deref(),
        Some("direct")
    );
    drop(occupied);
    f.service
        .switch_profile("new", ReloadMode::Restart)
        .await
        .unwrap();
    f.assert_current("new");
    assert_eq!(
        f.service
            .client()
            .unwrap()
            .configs()
            .await
            .unwrap()
            .mode
            .as_deref(),
        Some("rule")
    );
}
