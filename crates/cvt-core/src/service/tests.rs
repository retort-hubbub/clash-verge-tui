use super::*;
use crate::profile::item::PrfItem;
use chrono::Utc;
use tempfile::TempDir;

const BASE: &str = r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
mode: rule
proxies:
  - { name: "JP 01", type: vless, server: 1.2.3.4, port: 443, uuid: u }
proxy-groups:
  - { name: PROXY, type: select, proxies: ["JP 01", DIRECT] }
rules:
  - MATCH,PROXY
"#;

struct Fixture {
    _dir: TempDir,
    service: Service,
}

fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let service = Service::open(AppPaths::new(dir.path())).unwrap();
    Fixture { _dir: dir, service }
}

impl Fixture {
    fn seed(&self) -> String {
        let mut store = self.service.store().unwrap();
        let uid = store.add(PrfItem::local("L1", "base"));
        let item = store.get(&uid).unwrap().clone();
        store.write_document(&item, BASE).unwrap();
        store.set_current(&uid).unwrap();
        store.save().unwrap();
        uid
    }
}

#[test]
fn opening_creates_the_home_and_loads_default_settings() {
    let f = fixture();
    assert!(f.service.paths().profiles_dir().is_dir());
    let mut expected = Settings::default();
    expected.core.secret = f.service.settings().core.secret.clone();
    assert_eq!(*f.service.settings(), expected);
    assert_eq!(expected.core.secret.as_deref().unwrap().len(), 64);
    assert!(f.service.store().unwrap().items().is_empty());
}

#[test]
fn a_malformed_settings_file_stops_the_service_from_opening() {
    let dir = TempDir::new().unwrap();
    let paths = AppPaths::new(dir.path());
    paths.ensure_dirs().unwrap();
    std::fs::write(paths.settings_file(), "core: [oops\n").unwrap();
    let err = Service::open(paths).unwrap_err();
    assert!(matches!(err, Error::Parse { .. }), "{err:?}");
}

#[test]
fn settings_survive_a_round_trip_through_the_service() {
    let mut f = fixture();
    let mut s = f.service.settings().clone();
    s.ui.refresh_ms = 250;
    f.service.set_settings(s.clone());
    f.service.save_settings().unwrap();
    let reopened = Service::open(f.service.paths().clone()).unwrap();
    assert_eq!(*reopened.settings(), s);
}

#[test]
fn an_invalid_setting_is_refused_before_it_reaches_the_disk() {
    let mut f = fixture();
    let before = f
        .service
        .paths()
        .read(&f.service.paths().settings_file())
        .unwrap();
    let mut s = f.service.settings().clone();
    s.test.concurrency = 0;
    f.service.set_settings(s);
    assert!(f.service.save_settings().is_err());
    assert_eq!(
        f.service
            .paths()
            .read(&f.service.paths().settings_file())
            .unwrap(),
        before
    );
}

#[test]
fn there_is_no_endpoint_before_anything_is_generated() {
    let f = fixture();
    assert!(f.service.endpoint().unwrap().is_none());
    let err = f.service.client().unwrap_err();
    assert!(
        matches!(
            err,
            Error::MissingField {
                field: "external-controller",
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn the_application_settings_are_the_authority_on_the_endpoint() {
    let f = fixture();
    f.seed();
    let outcome = f.service.generate().unwrap();
    f.service.pipeline().commit(&outcome, false).unwrap();
    let endpoint = f.service.endpoint().unwrap().unwrap();
    assert_eq!(
        endpoint,
        Endpoint::tcp("127.0.0.1:9090", f.service.settings().core.secret.clone())
    );
    assert!(endpoint.is_loopback());
}

#[test]
fn subscription_fallback_uses_the_proxy_listener_not_the_controller() {
    let f = fixture();
    f.seed();
    assert_eq!(f.service.proxy_addr(), None, "only deployed ports count");
    let outcome = f.service.generate().unwrap();
    f.service.pipeline().commit(&outcome, false).unwrap();
    assert_eq!(
        f.service.proxy_addr(),
        None,
        "a stopped core cannot serve a proxy"
    );
    assert_eq!(
        f.service.endpoint().unwrap().unwrap(),
        Endpoint::tcp("127.0.0.1:9090", f.service.settings().core.secret.clone())
    );

    std::fs::write(
        f.service.paths().runtime_config(),
        "socks-port: 7891\nexternal-controller: 127.0.0.1:9090\n",
    )
    .unwrap();
    assert_eq!(f.service.proxy_addr(), None, "SOCKS is not an HTTP proxy");
}

#[test]
fn the_endpoint_uses_application_settings_before_first_generation() {
    let f = fixture();
    f.seed();
    // Nothing generated yet, but the profile declares a controller.
    let endpoint = f.service.endpoint().unwrap().unwrap();
    assert_eq!(
        endpoint,
        Endpoint::tcp("127.0.0.1:9090", f.service.settings().core.secret.clone())
    );
}

#[test]
fn a_client_can_be_built_once_an_endpoint_is_known() {
    let f = fixture();
    f.seed();
    let client = f.service.client().unwrap();
    assert!(client.endpoint().is_loopback());
}

#[test]
fn core_status_is_reported_without_a_binary() {
    let f = fixture();
    assert_eq!(f.service.core_status(), CoreStatus::Stopped);
    assert!(
        f.service.core_binary().is_none(),
        "nothing is on PATH in the test home"
    );
}

#[test]
fn starting_without_a_binary_explains_how_to_fix_it() {
    let f = fixture();
    f.seed();
    f.service
        .pipeline()
        .commit(&f.service.generate().unwrap(), false)
        .unwrap();
    let err = f.service.start_core().unwrap_err();
    match err {
        Error::CoreUnavailable { reason } => {
            assert!(reason.contains("mihomo"), "{reason}");
            assert!(
                reason.contains("CVT_CORE"),
                "the message names the override: {reason}"
            );
        }
        other => panic!("expected CoreUnavailable, got {other:?}"),
    }
}

#[test]
fn starting_without_a_generated_configuration_is_refused() {
    let f = fixture();
    let err = f.service.start_core().unwrap_err();
    // The missing binary is detected first, which is the more useful
    // message when both are missing.
    assert!(matches!(err, Error::CoreUnavailable { .. }), "{err:?}");
}

#[cfg(unix)]
#[test]
fn starting_a_selected_profile_generates_the_first_runtime_configuration() {
    use std::os::unix::fs::PermissionsExt as _;

    let f = fixture();
    let uid = f.seed();
    let store = f.service.store().unwrap();
    store
        .write_document(
            store.get(&uid).unwrap(),
            &BASE.replace("mixed-port: 7890", "mixed-port: 0"),
        )
        .unwrap();
    let binary = f.service.paths().core_dir().join("mihomo");
    std::fs::write(
        &binary,
        "#!/bin/sh\nif [ \"$1\" = \"-t\" ]; then exit 0; fi\nsleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!f.service.paths().runtime_config().exists());

    let started = f.service.start_core();
    assert!(started.is_ok(), "{started:?}");
    assert!(f.service.paths().runtime_config().is_file());
    assert!(
        f.service
            .paths()
            .read(&f.service.paths().runtime_config())
            .unwrap()
            .contains("MATCH,PROXY")
    );
    f.service.stop_core().unwrap();
}

#[test]
fn an_invalid_generated_configuration_is_refused_before_the_core_sees_it() {
    let f = fixture();
    let mut store = f.service.store().unwrap();
    let uid = store.add(PrfItem::local("L1", "broken"));
    let item = store.get(&uid).unwrap().clone();
    store
        .write_document(
            &item,
            "mixed-port: 7890\nexternal-controller: 127.0.0.1:9090\nrules:\n  - DOMAIN,a.test,GHOST\n",
        )
        .unwrap();
    store.set_current(&uid).unwrap();
    store.save().unwrap();

    // Force the document onto disk despite the validation errors.
    f.service
        .pipeline()
        .commit(&f.service.generate().unwrap(), true)
        .unwrap();

    // A fake core binary, so the binary check passes and validation runs.
    let fake = f.service.paths().core_dir().join("mihomo");
    std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
    let err = f.service.start_core().unwrap_err();
    assert!(matches!(err, Error::Validation { .. }), "{err:?}");
}

#[tokio::test]
async fn a_hot_reload_against_a_dead_core_fails_rather_than_pretending() {
    let f = fixture();
    f.seed();
    f.service
        .pipeline()
        .commit(&f.service.generate().unwrap(), false)
        .unwrap();
    let err = f.service.hot_reload().await.unwrap_err();
    assert!(
        matches!(err, Error::ControllerUnreachable { .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("not running"), "{err}");
}

#[tokio::test]
async fn waiting_for_a_core_that_never_answers_times_out_with_a_reason() {
    let f = fixture();
    f.seed();
    f.service
        .pipeline()
        .commit(&f.service.generate().unwrap(), false)
        .unwrap();
    // Reserve a private port for the whole wait. A fixed 9090 can belong
    // to a real Mihomo process on the developer's machine, making this
    // test pass or fail depending on unrelated local state.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let runtime = f.service.paths().runtime_config();
    let document = f.service.paths().read(&runtime).unwrap();
    assert!(document.contains("127.0.0.1:9090"));
    let document = document.replace("127.0.0.1:9090", &silent.local_addr().unwrap().to_string());
    f.service.paths().write_atomic(&runtime, &document).unwrap();
    let err = f.service.wait_until_ready().await.unwrap_err();
    assert!(
        matches!(err, Error::ControllerUnreachable { .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("did not become ready"), "{err}");
}

#[test]
fn apply_without_a_current_profile_reports_the_missing_base() {
    let f = fixture();
    let err = f.service.generate().unwrap_err();
    assert!(err.to_string().contains("current"), "{err}");
}

#[tokio::test]
async fn replaying_nothing_asks_the_core_for_nothing() {
    // The ordinary answer for a profile nobody has chosen a node in: no
    // core is needed, and none is dialled — the check is that this returns
    // without one rather than that it returns a particular number.
    let f = fixture();
    f.seed();
    assert_eq!(f.service.restore_selections().await.unwrap(), 0);
}

#[test]
fn the_backup_just_taken_is_never_pruned_by_its_own_prune() {
    let f = fixture();
    f.seed();
    // Five entries named *later* than now: a clock that ran fast, an archive
    // restored from another machine, a hand-made directory. The new backup
    // sorts last, so pruning by age alone deletes the one thing the call
    // exists to produce and returns a path to nothing.
    let future = Utc::now().timestamp() + 1000;
    for offset in 0..BACKUP_LIMIT {
        let dir = f
            .service
            .paths()
            .backups_dir()
            .join((future + i64::try_from(offset).unwrap_or(i64::MAX)).to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("profiles.yaml"), "current: L1\nitems: []\n").unwrap();
    }

    let created = f.service.backup().unwrap();
    assert!(
        created.join("profiles.yaml").is_file(),
        "`backup()` returned {} and pruned it away",
        created.display()
    );
    assert!(
        f.service
            .backups()
            .unwrap()
            .iter()
            .any(|b| b.path == created),
        "and it is not even listed"
    );
}

#[test]
fn a_restore_keeps_the_state_it_replaces_even_when_the_backups_directory_is_full() {
    let f = fixture();
    f.seed();
    f.service.save_settings().unwrap();
    let future = Utc::now().timestamp() + 1000;
    // A real backup to restore from, and five entries named *later* than it
    // — so the safety copy `restore` takes, stamped now, sorts last of all.
    let source = f.service.paths().backups_dir().join(future.to_string());
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("profiles.yaml"), "current: L1\nitems: []\n").unwrap();
    std::fs::write(source.join("cvt.yaml"), "ui:\n  refresh_ms: 250\n").unwrap();
    for offset in 1..=BACKUP_LIMIT {
        let dir = f
            .service
            .paths()
            .backups_dir()
            .join((future + i64::try_from(offset).unwrap_or(i64::MAX)).to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("profiles.yaml"), "current: L1\nitems: []\n").unwrap();
    }

    let safety = f.service.restore(&source).unwrap();
    assert!(
        safety.join("profiles.yaml").is_file() && safety.join("cvt.yaml").is_file(),
        "the restore said the state it replaced was kept at {}, and it is not there",
        safety.display()
    );
}

#[test]
fn restoring_the_oldest_backup_reads_it_before_pruning() {
    let f = fixture();
    f.service.save_settings().unwrap();
    let before = std::fs::read(f.service.paths().settings_file()).unwrap();
    let saved = "ui:\n  refresh_ms: 250\n";
    for stamp in 1..=BACKUP_LIMIT {
        let path = f.service.paths().backups_dir().join(stamp.to_string());
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("cvt.yaml"), saved).unwrap();
    }

    let source = f.service.paths().backups_dir().join("1");
    let safety = f.service.restore(&source).unwrap();
    assert_eq!(
        std::fs::read_to_string(f.service.paths().settings_file()).unwrap(),
        saved,
        "creating the safety backup must not prune the restore source before it is read"
    );
    assert_eq!(std::fs::read(safety.join("cvt.yaml")).unwrap(), before);
    assert_eq!(f.service.backups().unwrap().len(), BACKUP_LIMIT);
}

/// A core that answers nothing must not hold an apply for twice its budget.
///
/// `wait_for_document` checked its deadline *between* `client.group()`
/// calls, and that call carries the client's own ten-second timeout. So a
/// core that answered `/version` and hung `/group` held `apply` for about
/// ten: a deadline the function could not
/// enforce, which is the same mistake the selection replay was fixed for
/// three times over.
#[tokio::test]
async fn waiting_for_a_document_is_bounded_by_its_own_deadline() {
    // A listener that accepts and never answers: the shape a reloading core
    // has while it is rebuilding its groups.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        // Held open and never answered, which is the shape being tested.
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
        drop(held);
    });

    let mut f = fixture();
    f.seed();
    let mut settings = f.service.settings().clone();
    settings.ui.refresh_ms = 1000;
    settings.core.external_controller = Some(format!("127.0.0.1:{port}"));
    f.service.set_settings(settings);
    let config = Config::from_yaml(
        "proxy-groups:\n  - {name: a, type: select, proxies: [DIRECT]}\n  \
         - {name: b, type: select, proxies: [DIRECT]}\nrules: ['MATCH,a']\n",
    )
    .unwrap();

    let started = std::time::Instant::now();
    f.service.wait_for_document(&config).await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(7),
        "the wait took {elapsed:?} for a five-second budget; a call is \
         outside the deadline"
    );
}

#[test]
fn a_backup_holds_what_a_person_would_have_to_recreate() {
    let f = fixture();
    f.seed();
    // A home that has never saved its settings has no settings file to
    // copy, which is correct — the defaults are implied — so the realistic
    // case is the one worth asserting on.
    f.service.save_settings().unwrap();
    let outcome = f.service.generate().unwrap();
    f.service.pipeline().commit(&outcome, false).unwrap();

    let path = f.service.backup().unwrap();
    for name in ["cvt.yaml", "profiles.yaml", "profiles", "overrides"] {
        assert!(
            path.join(name).exists(),
            "{name} is missing from the backup"
        );
    }
    // Derived, large and reproducible: not the point of a backup, and the
    // reason the two directories are documented as different things.
    assert!(!path.join("runtime").exists());
    assert!(!path.join("logs").exists());
    assert_eq!(f.service.backups().unwrap().len(), 1);
}

#[test]
fn restoring_puts_the_state_back_and_keeps_what_it_replaced() {
    let f = fixture();
    let uid = f.seed();
    let taken = f.service.backup().unwrap();
    std::fs::write(
        f.service.paths().profiles_dir().join("tail.yaml"),
        "later\n",
    )
    .unwrap();

    let safety = f.service.restore(&taken).unwrap();
    // The state being replaced is kept, so restoring the wrong backup is
    // itself undoable: `tail.yaml` is in the safety copy…
    assert!(safety.join("profiles").join("tail.yaml").exists());
    // …and it is still where it was, because a restore is additive. It is
    // now a document no index entry mentions, which this program preserves
    // on purpose — see the method's documentation.
    assert!(f.service.paths().profiles_dir().join("tail.yaml").exists());
    assert!(f.service.store().unwrap().get(&uid).is_some());
}

#[test]
fn a_directory_that_is_not_a_backup_is_refused() {
    let f = fixture();
    f.seed();
    let elsewhere = tempfile::TempDir::new().unwrap();
    let error = f.service.restore(elsewhere.path()).unwrap_err();
    assert!(
        error.to_string().contains("not a backup"),
        "a restore has to say why it refused: {error}"
    );
}

#[test]
fn only_the_newest_backups_are_kept() {
    let f = fixture();
    f.seed();
    // Five is the limit, and the directories are named by the second they
    // were taken, so this has to stand on one per second to make five
    // distinct ones.
    for _ in 0..BACKUP_LIMIT + 2 {
        // Never fails: a second backup in the same second is given a
        // suffixed name rather than refused.
        f.service.backup().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }
    let kept = f.service.backups().unwrap();
    assert_eq!(kept.len(), BACKUP_LIMIT, "the oldest two should be gone");
    assert!(
        !kept.iter().any(|backup| backup.name().is_empty()),
        "a backup with no name could not be restored by name"
    );
}

#[test]
fn reload_mode_labels_are_stable() {
    assert_eq!(ReloadMode::Auto.label(), "auto");
    assert_eq!(ReloadMode::HotReload.label(), "hot reload");
    assert_eq!(ReloadMode::Restart.label(), "restart");
}

#[test]
fn reload_outcomes_describe_themselves() {
    assert!(ReloadOutcome::HotReloaded.succeeded());
    assert!(ReloadOutcome::Restarted { pid: 7 }.succeeded());
    assert_eq!(
        ReloadOutcome::Restarted { pid: 7 }.summary(),
        "restarted (pid 7)"
    );

    let rolled = ReloadOutcome::RolledBack {
        reason: "bad yaml".to_owned(),
        snapshot: PathBuf::from("/tmp/s.yaml"),
    };
    assert!(!rolled.succeeded(), "a rollback means the request failed");
    assert!(
        rolled.summary().contains("restored"),
        "{}",
        rolled.summary()
    );
}

#[test]
fn the_supervisor_and_pipeline_share_the_service_home() {
    let f = fixture();
    assert_eq!(
        f.service.supervisor().log_file(),
        f.service.paths().core_log()
    );
    assert_eq!(
        f.service.pipeline().output_path(),
        f.service.paths().runtime_config()
    );
}

/// Point the service at a binary that exists but always fails, i.e. a core
/// that refuses every document. This is the case rollback exists for, and
/// it needs no real core on the test machine.
fn refuse_everything(f: &mut Fixture) {
    let mut s = f.service.settings().clone();
    s.core.binary = Some(PathBuf::from("/bin/false"));
    s.core.rollback_on_failure = true;
    f.service.set_settings(s);
}

/// Finding F19: `reload` replaced the reason a restart failed with
/// `InvalidValue { field: "rollback" }` — "there are no snapshots to
/// restore" — because `Pipeline::rollback` was called through `?` on a
/// service whose first document had never been committed. `config generate
/// --apply` therefore blamed rollback bookkeeping for a missing core and
/// for a document the core rejected.
#[tokio::test]
async fn a_failed_reload_reports_the_real_cause_not_the_rollback_bookkeeping() {
    let mut f = fixture();
    f.seed();
    // A first apply: the document is written, but nothing was there
    // before it, so the pipeline has no snapshot to restore.
    let outcome = f.service.generate().unwrap();
    f.service.pipeline().commit(&outcome, false).unwrap();
    assert!(f.service.pipeline().snapshots().unwrap().is_empty());
    refuse_everything(&mut f);

    let err = f.service.reload(ReloadMode::Auto).await.unwrap_err();
    assert!(
        !matches!(
            &err,
            Error::InvalidValue {
                field: "rollback",
                ..
            }
        ),
        "bookkeeping must never displace the cause: {err:?}"
    );
    assert!(
        !err.to_string().contains("snapshot"),
        "nothing to roll back to is not a diagnosis: {err}"
    );
    assert!(
        matches!(err, Error::ProcessFailed { .. }),
        "the core's own refusal is the real cause: {err:?}"
    );
}

/// The complement: when a previous document *is* on disk, a rejected one is
/// still undone, so the test above cannot pass by disabling rollback.
#[tokio::test]
async fn a_rejected_document_is_still_rolled_back_when_there_is_one_to_restore() {
    let mut f = fixture();
    let uid = f.seed();
    let first = f.service.generate().unwrap();
    f.service.pipeline().commit(&first, false).unwrap();

    {
        let store = f.service.store().unwrap();
        let item = store.get(&uid).unwrap().clone();
        store
            .write_document(&item, &BASE.replace("7890", "7891"))
            .unwrap();
        store.save().unwrap();
    }
    let second = f.service.generate().unwrap();
    f.service.pipeline().commit(&second, false).unwrap();
    assert_ne!(first.yaml, second.yaml, "the two documents must differ");

    refuse_everything(&mut f);
    // The restart cannot succeed with a core that refuses everything, but
    // the document that was working has to be back on disk.
    let _ = f.service.reload(ReloadMode::Auto).await;

    let restored = std::fs::read_to_string(f.service.paths().runtime_config()).unwrap();
    assert_eq!(
        restored, first.yaml,
        "the working document must be restored"
    );
}
