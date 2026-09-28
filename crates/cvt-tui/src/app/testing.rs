//! Testing responsibilities of the application state machine.

use super::{App, Effect, StatusKind};
use crate::row::{TestKind, TestResult, TestRow};
use crate::state::Table;

impl App {
    // -- tests --------------------------------------------------------------

    /// Rebuild the tests screen from what the other screens currently show.
    ///
    /// Results are kept for a target that has not changed: re-entering the
    /// screen must not wipe a measurement the user just waited for.
    pub(super) fn rebuild_tests(&mut self) {
        let targets = TestKind::all().map(|kind| (kind, "current route".to_owned()));
        let selected = self
            .tests
            .selected_item()
            .map(|row| (row.kind, row.target.clone()));
        let previous = self.tests.items().to_vec();
        let rows: Vec<TestRow> = targets
            .into_iter()
            .map(|(kind, target)| {
                let result = previous
                    .iter()
                    .find(|row| row.kind == kind && row.target == target)
                    .map_or(TestResult::Pending, |row| row.result.clone());
                TestRow {
                    kind,
                    target,
                    result,
                }
            })
            .collect();
        let filter = self.tests.filter().to_owned();
        self.tests = Table::from_items(rows);
        if !filter.is_empty() {
            self.tests.set_filter(&filter);
        }
        // Arriving data can rebuild these rows several times a second, so the
        // cursor has to follow the row it was on rather than the position.
        if let Some((kind, target)) = selected
            && let Some(position) = self
                .tests
                .items()
                .iter()
                .position(|row| row.kind == kind && row.target == target)
        {
            self.tests.select(position);
        }
    }

    pub(super) fn run_tests(&mut self) -> Vec<Effect> {
        let Some(index) = self.tests.selected_source_index() else {
            self.refuse("no test selected");
            return Vec::new();
        };
        // A queued test is a request that has been accepted; only a test that
        // already ran (or is in flight) is worth refusing.
        if matches!(
            self.tests.items()[index].result,
            TestResult::Running | TestResult::Passed(_) | TestResult::Failed(_)
        ) {
            let label = self.tests.items()[index].kind.label();
            self.refuse(format!("`{label}` has already run; press c to clear it"));
            return Vec::new();
        }
        if !self.require_core("running a test") {
            return Vec::new();
        }
        let row = self.tests.items()[index].clone();
        self.set_test_result(index, TestResult::Running);
        if self.in_flight.is_some() {
            self.queue.push_back(index);
            let label = row.kind.label();
            let batch = self.queued_tests();
            self.set_status(
                StatusKind::Info,
                format!("queued `{label}` ({batch} in the batch)"),
            );
            return Vec::new();
        }
        self.in_flight = Some(index);
        vec![Effect::RunTest {
            kind: row.kind,
            target: row.target,
            mode: self.probe_mode,
        }]
    }

    pub(super) fn run_all_tests(&mut self) -> Vec<Effect> {
        if !self.require_core("running unlock checks") {
            return Vec::new();
        }
        if self.tests.items().is_empty() {
            self.refuse("no unlock checks are available");
            return Vec::new();
        }
        self.clear_test_results();
        self.queue = (1..self.tests.items().len()).collect();
        self.in_flight = Some(0);
        self.set_test_result(0, TestResult::Running);
        let first = &self.tests.items()[0];
        vec![
            Effect::CancelTests,
            Effect::RunTest {
                kind: first.kind,
                target: first.target.clone(),
                mode: self.probe_mode,
            },
        ]
    }

    pub(super) fn cancel_tests(&mut self) -> Vec<Effect> {
        if self.in_flight.is_none() && self.queue.is_empty() {
            self.refuse("no test batch is running");
            return Vec::new();
        }
        let count = self.queued_tests();
        self.abandon_tests();
        self.set_status(StatusKind::Warning, format!("cancelled {count} test(s)"));
        vec![Effect::CancelTests]
    }

    /// Put every queued or in-flight test back to pending.
    pub(super) fn abandon_tests(&mut self) {
        let running = self.in_flight.take();
        for index in running.into_iter().chain(std::mem::take(&mut self.queue)) {
            self.set_test_result(index, TestResult::Pending);
        }
    }

    pub(super) fn clear_test_results(&mut self) {
        self.abandon_tests();
        for index in 0..self.tests.items().len() {
            self.set_test_result(index, TestResult::Pending);
        }
    }

    /// Set one test's result, keeping the cursor on the row it was on.
    pub(super) fn set_test_result(&mut self, index: usize, result: TestResult) {
        let key = self
            .tests
            .selected_source_index()
            .and_then(|i| self.tests.items().get(i))
            .map(|row| (row.kind, row.target.clone()));
        let mut items = self.tests.items().to_vec();
        if let Some(row) = items.get_mut(index) {
            row.result = result;
        }
        self.tests.set_items(items);
        if let Some((kind, target)) = key
            && let Some(position) = self
                .tests
                .items()
                .iter()
                .position(|row| row.kind == kind && row.target == target)
        {
            self.tests.select(position);
        }
    }

    pub(super) fn on_test_result(
        &mut self,
        kind: TestKind,
        target: &str,
        result: TestResult,
    ) -> Vec<Effect> {
        let started = matches!(result, TestResult::Running);
        let index = self
            .tests
            .items()
            .iter()
            .position(|row| row.kind == kind && row.target == target);
        if let Some(index) = index {
            self.set_test_result(index, result);
            if started {
                return Vec::new();
            }
            self.queue.retain(|queued| *queued != index);
            if self.in_flight == Some(index) {
                self.in_flight = None;
            }
        }
        // The next queued test starts as soon as the previous one answers,
        // which is what keeps a batch from opening every node at once.
        while let Some(next) = self.queue.pop_front() {
            if let Some(row) = self.tests.items().get(next) {
                let (kind, target) = (row.kind, row.target.clone());
                self.in_flight = Some(next);
                return vec![Effect::RunTest {
                    kind,
                    target,
                    mode: self.probe_mode,
                }];
            }
        }
        Vec::new()
    }
}
