//! Profiles responsibilities of the application state machine.

use super::{App, Effect, StatusKind};
use crate::row::ProfileRow;

impl App {
    // -- profiles -----------------------------------------------------------

    pub(super) fn activate_profile(&mut self) -> Vec<Effect> {
        let Some(row) = self.profiles.selected_item() else {
            self.refuse("no profile selected");
            return Vec::new();
        };
        if row.current {
            let name = row.name.clone();
            self.set_status(StatusKind::Info, format!("`{name}` is already current"));
            return Vec::new();
        }
        if let Some(reason) = &row.unsupported {
            let reason = reason.clone();
            self.refuse(reason);
            return Vec::new();
        }
        vec![Effect::SwitchProfile {
            uid: row.uid.clone(),
        }]
    }

    pub(super) fn update_profile(&mut self) -> Vec<Effect> {
        let Some(row) = self.profiles.selected_item() else {
            self.refuse("no profile selected");
            return Vec::new();
        };
        if row.url.is_none() {
            let name = row.name.clone();
            self.refuse(format!("`{name}` is local; there is nothing to download"));
            return Vec::new();
        }
        vec![Effect::UpdateProfiles {
            uids: vec![row.uid.clone()],
        }]
    }

    pub(super) fn delete_profile(&mut self) -> Vec<Effect> {
        let Some(row) = self.profiles.selected_item() else {
            self.refuse("no profile selected");
            return Vec::new();
        };
        vec![Effect::DeleteProfile {
            uid: row.uid.clone(),
        }]
    }

    pub(super) fn edit_profile(&mut self) -> Vec<Effect> {
        let Some(row) = self.profiles.selected_item() else {
            self.refuse("no profile selected");
            return Vec::new();
        };
        vec![Effect::EditProfile {
            uid: row.uid.clone(),
        }]
    }

    pub(super) fn toggle_in_chain(&mut self) -> Vec<Effect> {
        let Some(row) = self.profiles.selected_item().cloned() else {
            self.refuse("no profile selected");
            return Vec::new();
        };
        if row.base_scope.is_some() {
            self.refuse("this override follows its subscription automatically");
            return Vec::new();
        }
        if row.kind.is_base() {
            self.refuse(format!(
                "`{}` supplies the base document, so it is always applied; only patches can be chained",
                row.name
            ));
            return Vec::new();
        }
        if let Some(reason) = &row.unsupported {
            let reason = reason.clone();
            self.refuse(reason);
            return Vec::new();
        }
        let text = if self.chain.contains(&row.uid) {
            self.chain.retain(|uid| *uid != row.uid);
            format!("`{}` removed from the chain", row.name)
        } else {
            self.chain.push(row.uid.clone());
            format!("`{}` added to the chain", row.name)
        };
        self.mark_chain();
        self.set_status(StatusKind::Info, text);
        vec![Effect::SetChain {
            uids: self.chain.clone(),
        }]
    }

    /// Reflect the local chain list in the rows, so the list updates at once
    /// instead of waiting for the store to be re-read.
    pub(super) fn mark_chain(&mut self) {
        let chain = self.chain.clone();
        let key = self.profiles.selected_item().map(|row| row.uid.clone());
        self.map_profiles(|row| row.in_chain = chain.contains(&row.uid));
        if let Some(key) = key {
            self.profiles.select_by_key(key, |row| row.uid.clone());
        }
    }

    pub(super) fn map_profiles(&mut self, mut f: impl FnMut(&mut ProfileRow)) {
        let key = self.profiles.selected_item().map(|row| row.uid.clone());
        let mut items = self.profiles.items().to_vec();
        for row in &mut items {
            f(row);
        }
        self.profiles.set_items(items);
        if let Some(key) = key {
            self.profiles.select_by_key(key, |row| row.uid.clone());
        }
    }
}
