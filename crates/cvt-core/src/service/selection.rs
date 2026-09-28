//! Persist and replay selected nodes with bounded per-call deadlines.

use super::{Client, Result, Service};

/// How long one look at one group may take, how long the whole replay may
/// take, and how often to look again.
///
/// Three attempts got this wrong in three different ways, and the shape of the
/// answer is the third: a deadline per group multiplies (twenty groups that are
/// gone cost twenty waits); a single deadline for the whole replay hands
/// everything to the first group that is gone; and waiting on each group *in
/// turn* inside that deadline does the same thing more slowly — six groups the
/// subscription has removed cost six waits, and the seventh choice, the one
/// that would have worked, is never reached.
///
/// So nothing waits on a group. The ones that are there are replayed first, and
/// the rest are polled together until they answer or the total runs out, which
/// makes a group that is gone cost one read per round rather than a share of
/// the budget.
const REPLAY_PER_GROUP: std::time::Duration = std::time::Duration::from_millis(250);
const REPLAY_TOTAL: std::time::Duration = std::time::Duration::from_millis(2000);
const REPLAY_STEP: std::time::Duration = std::time::Duration::from_millis(25);

/// How long this group may take, given how much of the total is left.
fn group_budget(overall: std::time::Instant, limit: std::time::Duration) -> std::time::Instant {
    (std::time::Instant::now() + limit).min(overall)
}

/// Point a group at a member and make sure it took.
async fn replay_one(
    client: &Client,
    name: &str,
    member: &str,
    deadline: std::time::Instant,
) -> bool {
    if !select_within(client, name, member, deadline).await {
        return false;
    }
    confirm_selection(client, name, member, deadline).await
}

/// Choose a member, without letting the call outlive the budget.
///
/// The reads were given a deadline and this was not, which is the same mistake
/// one call further down: a core that answers `GET /group/…` and never answers
/// the `PUT` costs the *client's* timeout — ten seconds by default —
/// which is not the replay's budget and cannot be enforced from here.
async fn select_within(
    client: &Client,
    name: &str,
    member: &str,
    deadline: std::time::Instant,
) -> bool {
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        return false;
    }
    matches!(
        tokio::time::timeout(left, client.select(name, member)).await,
        Ok(Ok(()))
    )
}

/// Read a group, without letting one slow request outlive the budget.
///
/// The client has its own ten-second timeout;
/// a deadline this function cannot enforce is a deadline in name only.
async fn read_group(
    client: &Client,
    group: &str,
    deadline: std::time::Instant,
) -> Option<crate::mihomo::types::ProxyView> {
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        return None;
    }
    tokio::time::timeout(left, client.group(group))
        .await
        .ok()?
        .ok()
}

/// Wait until the core reports the member that was asked for.
///
/// A `select` group reports the choice as `now`. A `url-test` or `fallback`
/// group *pins* it instead and reports `fixed`, keeping `now` for whatever the
/// test last picked — so checking only `now` reported a pin that had taken as a
/// failure, which is how the first version of this managed to apply a choice
/// and count zero. Either field is the choice having taken.
async fn confirm_selection(
    client: &Client,
    group: &str,
    member: &str,
    deadline: std::time::Instant,
) -> bool {
    loop {
        match read_group(client, group, deadline).await {
            Some(view)
                if view.now.as_deref() == Some(member) || view.fixed.as_deref() == Some(member) =>
            {
                return true;
            }
            // Not there at all: this document does not have the group, and the
            // next look would fail the same way.
            None if std::time::Instant::now() >= deadline => return false,
            _ => {}
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(REPLAY_STEP).await;
    }
}

impl Service {
    /// Record a node choice on the current profile.
    ///
    /// # Errors
    /// Whatever reading or writing the profile index returns.
    pub fn remember_selection(&self, group: &str, member: &str) -> Result<()> {
        let mut store = self.store()?;
        store.remember_selection(group, member)?;
        store.save()
    }

    /// Forget a group's choice, which is what unpinning means.
    ///
    /// # Errors
    /// Whatever reading or writing the profile index returns.
    pub fn forget_selection(&self, group: &str) -> Result<()> {
        let mut store = self.store()?;
        store.forget_selection(group)?;
        store.save()
    }

    /// Replay the current profile's remembered choices onto the core.
    ///
    /// Best effort per group: a group the subscription has renamed, or a member
    /// it has dropped, is skipped rather than failing the others — the choice
    /// was made against a document that no longer exists, and the remaining
    /// ones are still good.
    ///
    /// # Errors
    /// [`crate::error::Error::ControllerUnreachable`] when there is no core to talk to.
    pub async fn restore_selections(&self) -> Result<usize> {
        let selections = self.store()?.selections();
        if selections.is_empty() {
            return Ok(0);
        }
        let client = self.client()?;
        let overall = std::time::Instant::now() + REPLAY_TOTAL;
        let mut applied = 0;
        let mut late = Vec::new();

        // Two passes, and both are load-bearing.
        //
        // A group that is not there yet is not necessarily gone: a reload
        // rebuilds every group and the core applies it in the background, so
        // the whole reason for waiting is that the window is real. But a group
        // the subscription has *removed* is also not there, and it never will
        // be — and one pass with a budget per group let it spend that budget
        // and starve every choice after it. So the ones that are present are
        // replayed first, and only what is left is waited for.
        for selection in selections {
            if std::time::Instant::now() >= overall {
                tracing::debug!("the replay budget is spent");
                break;
            }
            let deadline = group_budget(overall, REPLAY_PER_GROUP);
            match read_group(&client, &selection.name, deadline).await {
                Some(_) => {
                    if replay_one(&client, &selection.name, &selection.now, deadline).await {
                        applied += 1;
                    }
                }
                None => late.push(selection),
            }
        }

        // Everything that was not there, polled *together* rather than one
        // after another. Waiting on each in turn gives the first group that is
        // gone the whole of its own budget and then the next one the same:
        // six groups a subscription has removed cost six waits, the total runs
        // out, and the seventh choice — the one that would have worked — is
        // never reached. A round costs one read per group still pending, and a
        // group that answers 404 costs almost nothing, so the live one is
        // replayed on the round after it appears however many dead ones precede
        // it.
        let mut pending = late;
        while !pending.is_empty() && std::time::Instant::now() < overall {
            let mut still = Vec::new();
            for selection in pending {
                let deadline = group_budget(overall, REPLAY_PER_GROUP);
                if read_group(&client, &selection.name, deadline)
                    .await
                    .is_none()
                {
                    still.push(selection);
                    continue;
                }
                if replay_one(&client, &selection.name, &selection.now, deadline).await {
                    applied += 1;
                } else {
                    tracing::debug!(
                        group = %selection.name,
                        member = %selection.now,
                        "the choice did not take; the configuration may have replaced the group"
                    );
                }
            }
            if still.is_empty() {
                break;
            }
            if std::time::Instant::now() >= overall {
                tracing::debug!(
                    groups = still.len(),
                    "the replay budget is spent before these groups came back"
                );
                break;
            }
            pending = still;
            tokio::time::sleep(REPLAY_STEP).await;
        }

        Ok(applied)
    }
}
