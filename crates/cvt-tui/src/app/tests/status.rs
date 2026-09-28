use super::*;

// -- status -------------------------------------------------------------

#[test]
fn every_footer_message_expires() {
    let info = Status::new(StatusKind::Info, "done");
    assert!(!info.is_expired_at(info.at));
    assert!(!info.is_expired_at(info.at + STATUS_TTL.saturating_sub(Duration::from_millis(1))));
    assert!(info.is_expired_at(info.at + STATUS_TTL));

    let error = Status::new(StatusKind::Error, "boom");
    assert!(error.is_expired_at(error.at + STATUS_TTL));
}

#[test]
fn a_message_disappears_after_its_lifetime_and_a_failure_replaces_it() {
    let mut a = loaded();
    let _ = a.on_event(Event::Done(Done::ChainSaved));
    let status = a.current_status().unwrap().clone();
    assert_eq!(status.kind, StatusKind::Success);

    let later = status.at + STATUS_TTL;
    a.expire_status_at(later);
    assert!(a.current_status().is_none(), "a success message expires");

    let _ = a.on_event(Event::Failed("the controller refused".to_owned()));
    let failed = a.current_status().unwrap();
    assert_eq!(failed.kind, StatusKind::Error);
    assert!(failed.text.contains("refused"));
    a.expire_status_at(failed.at + STATUS_TTL);
    assert!(a.current_status().is_none());
    assert_eq!(a.last_status().unwrap().text, "the controller refused");
}

#[test]
fn a_failed_batch_leaves_no_test_marked_as_running() {
    let mut a = loaded();
    goto(&mut a, Screen::Tests);
    let _ = press(&mut a, KeyCode::Enter);
    assert_eq!(a.queued_tests(), 1);
    let _ = a.on_event(Event::Failed("core went away".to_owned()));
    assert_eq!(a.queued_tests(), 0);
    assert!(
        a.tests
            .items()
            .iter()
            .all(|row| row.result == TestResult::Pending)
    );
}
