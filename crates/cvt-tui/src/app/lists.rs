//! Lists responsibilities of the application state machine.

use super::{App, Effect, Overlay, StatusKind};
use cvt_core::mihomo::types::LogLevel;

impl App {
    // -- connections --------------------------------------------------------

    pub(super) fn close_connection(&mut self) -> Vec<Effect> {
        let Some(id) = self.connections.selected_item().map(|row| row.id.clone()) else {
            self.refuse("no connection selected");
            return Vec::new();
        };
        if !self.require_core("closing a connection") {
            return Vec::new();
        }
        vec![Effect::CloseConnection { id }]
    }

    // -- logs ---------------------------------------------------------------

    pub(super) fn cycle_log_level(&mut self) -> Vec<Effect> {
        let all = LogLevel::all();
        let position = all.iter().position(|l| *l == self.log_level).unwrap_or(0);
        let next = all[(position + 1) % all.len()];
        self.log_level = next;
        self.settings.ui.log_level = next;
        self.settings_dirty = true;
        self.rebuild_settings_rows();
        let label = next.as_str();
        if self.core.is_running() {
            self.set_status(StatusKind::Info, format!("core log level is now {label}"));
            vec![Effect::SetCoreLogLevel(next)]
        } else {
            self.set_status(
                StatusKind::Info,
                format!("showing {label} and above; the core is not running"),
            );
            Vec::new()
        }
    }

    // -- rules --------------------------------------------------------------

    pub(super) fn toggle_rule(&mut self) -> Vec<Effect> {
        let Some(row) = self
            .rules
            .selected_item()
            .map(|row| (row.index, row.disabled))
        else {
            self.refuse("no rule selected");
            return Vec::new();
        };
        if !self.require_core("changing a rule") {
            return Vec::new();
        }
        vec![Effect::ToggleRule {
            index: row.0,
            disabled: !row.1,
        }]
    }

    pub(super) fn update_rule_provider(&mut self) -> Vec<Effect> {
        if !self.require_core("updating a rule set") {
            return Vec::new();
        }
        if self.rule_providers.is_empty() {
            // Rules carry no provider name, so there is nothing to narrow the
            // request to; say so rather than pretending a single set was meant.
            self.set_status(
                StatusKind::Info,
                "the core lists no rule providers; updating every set",
            );
            return vec![Effect::UpdateRuleProviders { names: Vec::new() }];
        }
        if self.rule_providers.len() == 1 {
            let names = vec![self.rule_providers[0].clone()];
            return vec![Effect::UpdateRuleProviders { names }];
        }
        self.overlay = Some(Overlay::Picker {
            title: "update rule set".to_owned(),
            items: self.rule_providers.clone(),
            selected: 0,
        });
        Vec::new()
    }
}
