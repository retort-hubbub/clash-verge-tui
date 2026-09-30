//! Application release prompts and deferred reminders.
use super::{App, Effect, Overlay, StatusKind, UpdateRelease};
use std::time::{Duration, Instant};

pub(crate) const UPDATE_TITLE: &str = "application update";
impl App {
    pub(super) fn offer_app_update(&mut self, release: UpdateRelease) {
        self.app_update = Some(release);
        self.show_pending_app_update();
    }

    pub(super) fn show_pending_app_update(&mut self) {
        if self.app_update.is_some() && self.overlay.is_none() {
            self.overlay = Some(Overlay::Picker {
                title: UPDATE_TITLE.to_owned(),
                items: vec![
                    "update now".to_owned(),
                    "remind me later".to_owned(),
                    "skip this version".to_owned(),
                ],
                selected: 0,
            });
        }
    }

    pub(super) fn choose_app_update(&mut self, selected: usize) -> Vec<Effect> {
        let Some(release) = self.app_update.take() else {
            return Vec::new();
        };
        self.app_update_next_check = Instant::now() + Duration::from_secs(3600);
        if selected == 0 {
            self.set_status(
                StatusKind::Info,
                self.tr("downloading application update…").to_owned(),
            );
            vec![Effect::InstallAppUpdate { tag: release.tag }]
        } else {
            self.set_status(
                StatusKind::Info,
                self.tr(if selected == 2 {
                    "application version skipped"
                } else {
                    "application update postponed for one hour"
                })
                .to_owned(),
            );
            vec![Effect::DismissAppUpdate {
                tag: release.tag,
                skip: selected == 2,
            }]
        }
    }
}
