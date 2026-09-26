//! Interface state: tables, buffers and gauges.
//!
//! These are the pieces of the interface that are pure data and pure logic, so
//! they are separated from rendering and from the side effects they trigger.
//! That is what lets the behaviour a user actually notices — where the cursor
//! lands after a filter, whether the view follows new input, which rows survive
//! a search — be tested without a terminal.
//!
//! [`Table`] is the centrepiece. Every list in the application is one, and it
//! solves the same three problems every time: keeping a cursor and a scroll
//! offset consistent under movement, under a changing viewport, and under a
//! filter that can shrink the list beneath the cursor.

use std::collections::VecDeque;

/// Anything a table can filter on.
pub trait Filterable {
    /// `true` when this row should remain visible for `needle`.
    ///
    /// An empty needle must match everything, so that clearing the search
    /// restores the full list.
    fn matches_filter(&self, needle: &str) -> bool;
}

/// `true` when `needle` appears in `haystack`, ignoring case.
///
/// The one comparison used by every implementation, so that filter behaviour
/// cannot differ between screens.
#[must_use]
pub fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// A selectable, scrollable, filterable list.
///
/// `T` owns the rows; the visible set is a list of indices into it, so
/// filtering never copies the data and clearing a filter restores the original
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table<T> {
    items: Vec<T>,
    visible: Vec<usize>,
    /// Index into `visible`, not into `items`.
    selected: usize,
    /// Index of the first visible row in `visible`.
    offset: usize,
    filter: String,
}

impl<T> Default for Table<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            offset: 0,
            filter: String::new(),
        }
    }
}

impl<T> Table<T> {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            offset: 0,
            filter: String::new(),
        }
    }
}

impl<T: Filterable> Table<T> {
    /// Build a table from rows.
    #[must_use]
    pub fn from_items(items: Vec<T>) -> Self {
        let mut table = Self::new();
        table.set_items(items);
        table
    }

    /// Replace every row, preserving the filter and clamping the cursor.
    ///
    /// A refresh must not lose the user's place: the cursor stays on the same
    /// *position* in the visible set, clamped to what still exists. Callers
    /// that need to follow a specific row should re-select it explicitly.
    pub fn set_items(&mut self, items: Vec<T>) {
        self.items = items;
        self.rebuild();
    }

    /// Every row, filtered or not.
    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Number of rows that pass the filter.
    #[must_use]
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    /// `true` when nothing is visible.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// Number of rows, filtered or not.
    #[must_use]
    pub fn total(&self) -> usize {
        self.items.len()
    }

    /// The current filter.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// `true` when a filter is active and hiding rows.
    #[must_use]
    pub fn is_filtered(&self) -> bool {
        !self.filter.is_empty() && self.visible.len() != self.items.len()
    }

    /// Apply a filter, keeping the cursor on the same row when it survives.
    pub fn set_filter(&mut self, needle: &str) {
        // Remember the row itself, by its position in the unfiltered list, so a
        // narrowing filter keeps the cursor on the same node instead of
        // silently moving the selection somewhere else. Working in terms of the
        // source index avoids needing `T: Clone`.
        let previously_selected = self.selected_source_index();
        needle.clone_into(&mut self.filter);
        self.rebuild();
        if let Some(source) = previously_selected
            && let Some(position) = self.visible.iter().position(|i| *i == source)
        {
            self.selected = position;
        }
        self.clamp();
    }

    /// Clear the filter.
    pub fn clear_filter(&mut self) {
        self.set_filter("");
    }

    /// Position of the cursor within the visible rows.
    #[must_use]
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Index of the cursor within the unfiltered rows, if any.
    #[must_use]
    pub fn selected_source_index(&self) -> Option<usize> {
        self.visible.get(self.selected).copied()
    }

    /// The row under the cursor.
    #[must_use]
    pub fn selected_item(&self) -> Option<&T> {
        self.visible.get(self.selected).map(|i| &self.items[*i])
    }

    /// The row under the cursor, for editing.
    pub fn selected_item_mut(&mut self) -> Option<&mut T> {
        let index = self.selected_source_index()?;
        self.items.get_mut(index)
    }

    /// Move the cursor by `delta` rows, clamping at both ends.
    ///
    /// Clamping rather than wrapping is deliberate: in a long node list,
    /// wrapping from the last row to the first is disorienting.
    pub fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.visible.len() - 1;
        let next = self.selected.saturating_add_signed(delta).min(last);
        self.selected = next;
    }

    /// Move the cursor to the first row.
    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    /// Move the cursor to the last row.
    pub fn select_last(&mut self) {
        self.selected = self.visible.len().saturating_sub(1);
    }

    /// Move by a viewport's worth, keeping one row of overlap for continuity.
    pub fn page(&mut self, direction: isize, viewport: usize) {
        let step = viewport.saturating_sub(1).max(1);
        let delta = if direction < 0 {
            -isize::try_from(step).unwrap_or(isize::MAX)
        } else {
            isize::try_from(step).unwrap_or(isize::MAX)
        };
        self.move_by(delta);
    }

    /// Move the cursor to a specific visible position.
    pub fn select(&mut self, index: usize) {
        self.selected = index.min(self.visible.len().saturating_sub(1));
    }

    /// Select the row with `key`, if present.
    ///
    /// The key is taken by value because callers naturally have one to hand
    /// (`row.uid.clone()`), and borrowing it would push an awkward `&` onto
    /// every call site for no benefit.
    #[allow(clippy::needless_pass_by_value)]
    pub fn select_by_key<K: PartialEq>(&mut self, key: K, of: impl Fn(&T) -> K) {
        if let Some(position) = self.visible.iter().position(|i| of(&self.items[*i]) == key) {
            self.selected = position;
        }
    }

    /// Move the cursor to the next row matching `predicate`, wrapping once.
    pub fn select_next_matching(&mut self, predicate: impl Fn(&T) -> bool) {
        if self.visible.is_empty() {
            return;
        }
        let count = self.visible.len();
        for step in 1..=count {
            let index = (self.selected + step) % count;
            if predicate(&self.items[self.visible[index]]) {
                self.selected = index;
                return;
            }
        }
    }

    /// Scroll so the cursor is inside a viewport of `height` rows.
    pub fn scroll_into_view(&mut self, height: usize) {
        if height == 0 {
            self.offset = self.selected;
            return;
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + height {
            self.offset = self.selected + 1 - height;
        }
        // Never leave a gap at the bottom after the list shrinks.
        let max_offset = self.visible.len().saturating_sub(height);
        self.offset = self.offset.min(max_offset);
    }

    /// The scroll offset, in visible rows.
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Recompute the visible set after the source rows or the filter changed.
    fn rebuild(&mut self) {
        self.visible = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.matches_filter(&self.filter))
            .map(|(index, _)| index)
            .collect();
        self.clamp();
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        self.offset = self.offset.min(self.visible.len().saturating_sub(1));
    }
}

/// How a list is ordered, cycled by an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    /// As the core or the file lists them.
    #[default]
    Natural,
    /// Smallest latency first; unmeasured rows last.
    LatencyAscending,
    /// Largest latency first; unmeasured rows first.
    LatencyDescending,
    /// Largest traffic first.
    TrafficDescending,
}

impl SortOrder {
    /// Every order, for cycling and for the settings pane.
    #[must_use]
    pub fn all() -> [Self; 4] {
        [
            Self::Natural,
            Self::LatencyAscending,
            Self::LatencyDescending,
            Self::TrafficDescending,
        ]
    }

    /// The next order in the cycle.
    #[must_use]
    pub fn next(self) -> Self {
        let all = Self::all();
        let index = all.iter().position(|s| *s == self).unwrap_or(0);
        all[(index + 1) % all.len()]
    }

    /// A short label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Natural => "natural",
            Self::LatencyAscending => "fastest",
            Self::LatencyDescending => "slowest",
            Self::TrafficDescending => "busiest",
        }
    }
}

/// The rolling log buffer.
///
/// Bounded on purpose: a core at `debug` level emits thousands of lines a
/// second, and an unbounded buffer would grow until the process died. The
/// oldest lines are dropped, which is the correct trade for a terminal view.
#[derive(Debug, Clone)]
pub struct LogBuffer {
    lines: VecDeque<crate::row::LogRow>,
    capacity: usize,
    /// Sequence numbers dropped so far, so the footer can say so.
    dropped: u64,
    filter: String,
    /// Whether the view stays at the newest line.
    pub follow: bool,
}

impl LogBuffer {
    /// A buffer holding at most `capacity` lines.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(capacity.min(4096)),
            capacity: capacity.max(1),
            dropped: 0,
            filter: String::new(),
            follow: true,
        }
    }

    /// Append a line, dropping the oldest when full.
    pub fn push(&mut self, row: crate::row::LogRow) {
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.lines.push_back(row);
    }

    /// Every buffered line.
    #[must_use]
    pub fn lines(&self) -> &VecDeque<crate::row::LogRow> {
        &self.lines
    }

    /// Lines matching the filter, newest last.
    #[must_use]
    pub fn filtered(&self) -> Vec<&crate::row::LogRow> {
        self.lines
            .iter()
            .filter(|l| l.matches_filter(&self.filter))
            .collect()
    }

    /// Number of buffered lines.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// `true` when nothing is buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// How many lines were discarded to stay within capacity.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The current filter.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Replace the filter.
    pub fn set_filter(&mut self, needle: &str) {
        needle.clone_into(&mut self.filter);
    }

    /// Discard everything.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.dropped = 0;
    }

    /// Render the buffer as text, for export.
    #[must_use]
    pub fn export(&self) -> String {
        let mut out = String::with_capacity(self.lines.len() * 80);
        for line in &self.lines {
            out.push_str(line.at.as_str());
            out.push(' ');
            out.push_str(&line.level);
            out.push(' ');
            out.push_str(&line.message);
            out.push('\n');
        }
        out
    }

    /// Longest line length, for horizontal scrolling.
    #[must_use]
    pub fn max_width(&self) -> usize {
        self.lines
            .iter()
            .map(super::row::LogRow::display_width)
            .max()
            .unwrap_or(0)
    }
}

/// Rolling gauges for the dashboard.
///
/// Holds a fixed number of samples so a sparkline needs no allocation per
/// frame, and so memory use does not depend on how long the application runs.
#[derive(Debug, Clone)]
pub struct Metrics {
    /// Bytes per second downloaded, oldest first.
    pub down: VecDeque<u64>,
    /// Bytes per second uploaded, oldest first.
    pub up: VecDeque<u64>,
    /// Resident memory in bytes, oldest first.
    pub memory: VecDeque<u64>,
    /// The most recent sample.
    pub latest: Option<crate::row::Live>,
    capacity: usize,
}

impl Metrics {
    /// Gauges holding `capacity` samples.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            down: VecDeque::with_capacity(capacity),
            up: VecDeque::with_capacity(capacity),
            memory: VecDeque::with_capacity(capacity),
            latest: None,
            capacity,
        }
    }

    /// Record a traffic sample.
    pub fn push_traffic(&mut self, traffic: cvt_core::mihomo::types::Traffic) {
        push_capped(&mut self.down, traffic.down, self.capacity);
        push_capped(&mut self.up, traffic.up, self.capacity);
        let live = self.latest.get_or_insert_default();
        live.down_rate = traffic.down;
        live.up_rate = traffic.up;
        live.down_total = traffic.down_total;
        live.up_total = traffic.up_total;
    }

    /// Record a memory sample.
    pub fn push_memory(&mut self, memory: u64) {
        push_capped(&mut self.memory, memory, self.capacity);
        self.latest.get_or_insert_default().memory = memory;
    }

    /// Record connection totals.
    pub fn set_connections(&mut self, count: usize) {
        self.latest.get_or_insert_default().connections = count;
    }

    /// Samples held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.down.len()
    }

    /// `true` when nothing has been sampled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.down.is_empty()
    }

    /// Forget every sample.
    pub fn clear(&mut self) {
        self.down.clear();
        self.up.clear();
        self.memory.clear();
        self.latest = None;
    }
}

fn push_capped(queue: &mut VecDeque<u64>, value: u64, capacity: usize) {
    if queue.len() == capacity {
        queue.pop_front();
    }
    queue.push_back(value);
}

/// Format a byte count for a narrow column.
///
/// Binary units with one decimal: `1.4 MiB`. A terminal column is too narrow
/// for more, and the distinction between MiB and MB never matters here.
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    #[allow(clippy::cast_precision_loss)] // display only
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Format a rate, e.g. `1.4 MiB/s`.
#[must_use]
pub fn human_rate(bytes_per_second: u64) -> String {
    format!("{}/s", human_bytes(bytes_per_second))
}

/// Format a latency reading.
#[must_use]
pub fn human_delay(millis: Option<u16>) -> String {
    match millis {
        None => "-".to_owned(),
        Some(0) => "?".to_owned(),
        Some(d) => format!("{d} ms"),
    }
}

/// Format a duration in seconds as an age, e.g. `3h ago`.
#[must_use]
pub fn human_age(seconds: i64) -> String {
    if seconds < 0 {
        return "never".to_owned();
    }
    match seconds {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86_399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Row {
        name: String,
        size: u64,
    }

    impl Row {
        fn new(name: &str, size: u64) -> Self {
            Self {
                name: name.to_owned(),
                size,
            }
        }
    }

    impl Filterable for Row {
        fn matches_filter(&self, needle: &str) -> bool {
            contains_ignore_case(&self.name, needle)
        }
    }

    fn table(names: &[&str]) -> Table<Row> {
        Table::from_items(names.iter().map(|n| Row::new(n, 1)).collect())
    }

    #[test]
    fn a_new_table_is_empty_and_safe_to_use() {
        let mut t: Table<Row> = Table::new();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert_eq!(t.total(), 0);
        assert!(t.selected_item().is_none());
        // Movement on an empty table must not panic.
        t.move_by(-5);
        t.move_by(5);
        t.page(-1, 10);
        t.select_last();
        t.scroll_into_view(10);
        t.select_next_matching(|_| true);
        assert!(t.selected_item().is_none());
        assert_eq!(t.selected_index(), 0);
    }

    #[test]
    fn movement_clamps_rather_than_wrapping() {
        let mut t = table(&["a", "b", "c"]);
        assert_eq!(t.selected_item().unwrap().name, "a");
        t.move_by(-1);
        assert_eq!(
            t.selected_item().unwrap().name,
            "a",
            "cannot go above the first row"
        );
        t.move_by(2);
        assert_eq!(t.selected_item().unwrap().name, "c");
        t.move_by(10);
        assert_eq!(
            t.selected_item().unwrap().name,
            "c",
            "cannot go past the last row"
        );
        t.select_first();
        assert_eq!(t.selected_index(), 0);
        t.select_last();
        assert_eq!(t.selected_index(), 2);
    }

    #[test]
    fn paging_keeps_one_row_of_overlap() {
        let mut t = table(&["a", "b", "c", "d", "e", "f", "g", "h"]);
        t.page(1, 3);
        assert_eq!(
            t.selected_index(),
            2,
            "two rows forward for a 3-row viewport"
        );
        t.page(1, 3);
        assert_eq!(t.selected_index(), 4);
        t.page(-1, 3);
        assert_eq!(t.selected_index(), 2);
        t.page(-1, 3);
        assert_eq!(t.selected_index(), 0, "clamped at the top");
    }

    #[test]
    fn a_viewport_of_zero_or_one_row_does_not_loop_forever() {
        let mut t = table(&["a", "b", "c"]);
        t.page(1, 0);
        assert_eq!(t.selected_index(), 1, "still advances by at least one row");
        t.page(1, 1);
        assert_eq!(t.selected_index(), 2);
    }

    #[test]
    fn filtering_narrows_the_visible_set_and_clearing_restores_it() {
        let mut t = table(&["google.com", "github.com", "taobao.com"]);
        t.set_filter("goo");
        assert_eq!(t.len(), 1);
        assert!(t.is_filtered());
        assert_eq!(t.selected_item().unwrap().name, "google.com");
        assert_eq!(t.total(), 3, "the source rows are untouched");
        t.clear_filter();
        assert_eq!(t.len(), 3);
        assert!(!t.is_filtered());
    }

    #[test]
    fn filtering_is_case_insensitive_and_an_empty_filter_matches_all() {
        let mut t = table(&["Google", "github"]);
        t.set_filter("GOOG");
        assert_eq!(t.len(), 1);
        t.set_filter("");
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn a_filter_that_matches_nothing_leaves_a_usable_empty_table() {
        let mut t = table(&["a", "b"]);
        t.set_filter("zzz");
        assert!(t.is_empty());
        assert!(t.selected_item().is_none());
        assert_eq!(t.selected_source_index(), None);
        t.move_by(1);
        t.scroll_into_view(5);
        // Clearing brings rows back with a valid cursor.
        t.clear_filter();
        assert_eq!(t.len(), 2);
        assert!(t.selected_item().is_some());
    }

    #[test]
    fn the_cursor_stays_on_the_same_row_when_a_filter_shrinks_the_list() {
        // "ta" matches beta and delta only, so this genuinely narrows.
        let mut t = table(&["alpha", "beta", "gamma", "delta"]);
        t.select(1); // beta
        assert_eq!(t.selected_item().unwrap().name, "beta");
        t.set_filter("ta");
        assert_eq!(
            t.selected_item().unwrap().name,
            "beta",
            "a filter must not silently move the selection to another row"
        );
        assert_eq!(t.selected_index(), 0, "its position within the view shifts");
    }

    #[test]
    fn the_cursor_clamps_when_the_filter_hides_the_selected_row() {
        let mut t = table(&["alpha", "beta", "gamma", "delta"]);
        t.select(2); // gamma, which "ta" excludes
        t.set_filter("ta");
        assert_eq!(t.len(), 2);
        assert_eq!(t.selected_index(), 1, "clamped into the shorter view");
        assert_eq!(t.selected_item().unwrap().name, "delta");
        // Clearing restores every row and keeps a valid cursor.
        t.clear_filter();
        assert_eq!(t.len(), 4);
        assert!(t.selected_item().is_some());
    }

    #[test]
    fn refreshing_keeps_the_cursor_position_and_clamps_it() {
        let mut t = table(&["a", "b", "c", "d"]);
        t.select(3);
        t.set_items(vec![Row::new("a", 1), Row::new("b", 2)]);
        assert_eq!(t.selected_index(), 1, "clamped to the new last row");
        assert_eq!(t.selected_item().unwrap().name, "b");
    }

    #[test]
    fn selecting_by_key_finds_the_row_and_tolerates_a_missing_one() {
        let mut t = table(&["a", "b", "c"]);
        t.select_by_key("c".to_owned(), |r| r.name.clone());
        assert_eq!(t.selected_item().unwrap().name, "c");
        t.select_by_key("zzz".to_owned(), |r| r.name.clone());
        assert_eq!(
            t.selected_item().unwrap().name,
            "c",
            "an absent key changes nothing"
        );
    }

    #[test]
    fn next_matching_wraps_around_and_stops_when_nothing_matches() {
        let mut t = Table::from_items(vec![
            Row::new("a", 1),
            Row::new("bb", 2),
            Row::new("c", 3),
            Row::new("bb2", 2),
        ]);
        t.select_first();
        t.select_next_matching(|r| r.size == 2);
        assert_eq!(t.selected_item().unwrap().name, "bb");
        t.select_next_matching(|r| r.size == 2);
        assert_eq!(t.selected_item().unwrap().name, "bb2");
        t.select_next_matching(|r| r.size == 2);
        assert_eq!(t.selected_item().unwrap().name, "bb", "wraps around");
        t.select_next_matching(|r| r.size == 99);
        assert_eq!(
            t.selected_item().unwrap().name,
            "bb",
            "no match leaves the cursor alone"
        );
    }

    #[test]
    fn scrolling_follows_the_cursor_and_never_leaves_a_gap() {
        let mut t = table(&["a", "b", "c", "d", "e", "f"]);
        // Moving down past the viewport scrolls it.
        for _ in 0..4 {
            t.move_by(1);
        }
        assert_eq!(t.selected_index(), 4);
        t.scroll_into_view(3);
        assert_eq!(
            t.offset(),
            2,
            "rows 2..4 are visible with the cursor at the bottom"
        );
        // Moving back up scrolls back.
        t.select_first();
        t.scroll_into_view(3);
        assert_eq!(t.offset(), 0);
    }

    #[test]
    fn shrinking_the_list_pulls_the_scroll_offset_back() {
        let mut t = table(&["a", "b", "c", "d", "e", "f"]);
        t.select(5);
        t.scroll_into_view(3);
        assert_eq!(t.offset(), 3);
        t.set_items(vec![Row::new("a", 1), Row::new("b", 2)]);
        t.scroll_into_view(3);
        assert_eq!(
            t.offset(),
            0,
            "a short list must not be scrolled off screen"
        );
    }

    #[test]
    fn a_zero_height_viewport_does_not_underflow() {
        let mut t = table(&["a", "b", "c"]);
        t.select(2);
        t.scroll_into_view(0);
        assert_eq!(t.offset(), 2);
        assert_eq!(t.selected_index(), 2);
    }

    #[test]
    fn sort_orders_cycle_through_every_variant() {
        let mut order = SortOrder::default();
        assert_eq!(order, SortOrder::Natural);
        let mut seen = vec![order];
        for _ in 0..SortOrder::all().len() - 1 {
            order = order.next();
            seen.push(order);
        }
        assert_eq!(seen.len(), SortOrder::all().len());
        assert_eq!(order.next(), SortOrder::Natural, "the cycle closes");
        for o in SortOrder::all() {
            assert!(!o.label().is_empty());
        }
    }

    #[test]
    fn the_log_buffer_is_bounded_and_reports_what_it_dropped() {
        use crate::row::LogRow;
        let mut b = LogBuffer::new(3);
        for i in 0..5 {
            b.push(LogRow::new("info", format!("line {i}")));
        }
        assert_eq!(b.len(), 3, "never grows past its capacity");
        assert_eq!(b.dropped(), 2);
        assert_eq!(
            b.lines().front().unwrap().message,
            "line 2",
            "the oldest are dropped"
        );
        assert_eq!(b.lines().back().unwrap().message, "line 4");
    }

    #[test]
    fn a_log_buffer_with_a_zero_capacity_still_works() {
        use crate::row::LogRow;
        let mut b = LogBuffer::new(0);
        b.push(LogRow::new("info", "x"));
        assert_eq!(b.len(), 1, "capacity is forced to at least one");
    }

    #[test]
    fn the_log_buffer_filters_and_exports_preserving_order() {
        use crate::row::LogRow;
        let mut b = LogBuffer::new(10);
        b.push(LogRow::new("info", "started"));
        b.push(LogRow::new("error", "dial failed"));
        b.push(LogRow::new("info", "started again"));
        assert_eq!(b.filtered().len(), 3);
        b.set_filter("FAILED");
        assert_eq!(b.filtered().len(), 1, "log search is case-insensitive");
        assert_eq!(b.filtered()[0].level, "error");
        assert!(b.export().contains("error dial failed"));
        assert!(b.export().lines().count() == 3, "export ignores the filter");
        b.clear();
        assert!(b.is_empty());
        assert_eq!(b.dropped(), 0);
    }

    #[test]
    fn metrics_keep_a_fixed_number_of_samples() {
        use cvt_core::mihomo::types::Traffic;
        let mut m = Metrics::new(3);
        for i in 0..5u64 {
            m.push_traffic(Traffic {
                up: i,
                down: i * 2,
                up_total: i,
                down_total: i * 2,
            });
        }
        assert_eq!(m.len(), 3, "the gauge is bounded");
        assert_eq!(m.down.front(), Some(&4), "oldest dropped");
        assert_eq!(m.down.back(), Some(&8));
        let live = m.latest.unwrap();
        assert_eq!(live.down_rate, 8);
        assert_eq!(live.up_rate, 4);
    }

    #[test]
    fn metrics_accumulate_memory_and_connection_counts() {
        let mut m = Metrics::new(2);
        m.push_memory(1024);
        m.set_connections(7);
        assert_eq!(m.latest.unwrap().memory, 1024);
        assert_eq!(m.latest.unwrap().connections, 7);
        // A later traffic sample must not clear the other readings.
        m.push_traffic(cvt_core::mihomo::types::Traffic {
            up: 1,
            down: 1,
            ..Default::default()
        });
        assert_eq!(m.latest.unwrap().memory, 1024);
        assert_eq!(m.latest.unwrap().connections, 7);
        m.clear();
        assert!(m.is_empty());
        assert!(m.latest.is_none());
    }

    #[test]
    fn byte_and_rate_formatting_is_readable_at_every_magnitude() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
        assert_eq!(human_rate(2048), "2.0 KiB/s");
    }

    #[test]
    fn delay_formatting_distinguishes_unmeasured_from_failed() {
        assert_eq!(human_delay(Some(42)), "42 ms");
        assert_eq!(human_delay(Some(0)), "?", "zero means not measured");
        assert_eq!(human_delay(None), "-", "absent means it failed");
    }

    #[test]
    fn age_formatting_covers_every_bucket() {
        assert_eq!(human_age(-1), "never");
        assert_eq!(human_age(0), "just now");
        assert_eq!(human_age(59), "just now");
        assert_eq!(human_age(60), "1m ago");
        assert_eq!(human_age(7200), "2h ago");
        assert_eq!(human_age(86_400 * 3), "3d ago");
    }
}
