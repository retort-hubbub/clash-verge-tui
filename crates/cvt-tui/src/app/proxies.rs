//! Proxies responsibilities of the application state machine.

use super::{App, Effect};

impl App {
    // -- proxies ------------------------------------------------------------

    pub(super) fn toggle_group(&mut self, group: &str) -> Vec<Effect> {
        if let Some(position) = self.expanded.iter().position(|g| g == group) {
            self.expanded.remove(position);
        } else {
            self.expanded.push(group.to_owned());
        }
        self.rebuild_nodes();
        Vec::new()
    }

    pub(super) fn select_node(&mut self) -> Vec<Effect> {
        let Some(row) = self.nodes.selected_item().cloned() else {
            self.refuse("no node selected");
            return Vec::new();
        };
        if row.is_group {
            if row.members == 0 {
                self.refuse(format!("`{}` has no members to show", row.name));
                return Vec::new();
            }
            return self.toggle_group(&row.name);
        }
        let Some(group) = row.group.clone() else {
            self.refuse(format!("`{}` is not a member of a group", row.name));
            return Vec::new();
        };
        if !self.require_core("selecting a node") {
            return Vec::new();
        }
        vec![Effect::SelectNode {
            group,
            member: row.name,
        }]
    }

    /// The group a highlighted row belongs to: itself when it is a group.
    pub(super) fn highlighted_group(&self) -> Option<String> {
        self.nodes
            .selected_item()
            .map(|row| row.group.clone().unwrap_or_else(|| row.name.clone()))
    }

    pub(super) fn test_node(&mut self) -> Vec<Effect> {
        let Some(row) = self.nodes.selected_item().cloned() else {
            self.refuse("no node selected");
            return Vec::new();
        };
        let Some(_group) = row.group.clone() else {
            let name = row.name.clone();
            return if row.is_group {
                self.test_group_named(name)
            } else {
                self.refuse(format!("`{name}` is not part of a group"));
                Vec::new()
            };
        };
        if !self.require_core("testing a node") {
            return Vec::new();
        }
        vec![Effect::TestNode {
            name: row.name,
            mode: self.probe_mode,
        }]
    }

    pub(super) fn test_group(&mut self) -> Vec<Effect> {
        let Some(group) = self.highlighted_group() else {
            self.refuse("no group selected");
            return Vec::new();
        };
        self.test_group_named(group)
    }

    pub(super) fn test_group_named(&mut self, group: String) -> Vec<Effect> {
        if !self.require_core("testing a group") {
            return Vec::new();
        }
        vec![Effect::TestGroup {
            group,
            mode: self.probe_mode,
        }]
    }

    pub(super) fn clear_node_pin(&mut self) -> Vec<Effect> {
        let Some(row) = self.nodes.selected_item().cloned() else {
            self.refuse("no node selected");
            return Vec::new();
        };
        let group = row.group.clone().unwrap_or_else(|| row.name.clone());
        if row.is_group && !row.selectable {
            self.refuse(format!("`{group}` does not choose a node by hand"));
            return Vec::new();
        }
        if !self.require_core("clearing a pin") {
            return Vec::new();
        }
        vec![Effect::ClearNodePin { group }]
    }
}
