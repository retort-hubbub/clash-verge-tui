//! Settings responsibilities of the application state machine.

use super::{App, Effect, Overlay, PromptKind, StatusKind};
use crate::state::Table;
use crate::theme::Theme;
use cvt_core::mihomo::types::LogLevel;
use cvt_core::settings::{Language, Settings};
use std::path::PathBuf;

mod catalog;
pub use catalog::{SettingKind, SettingRow, setting_rows};

/// Boolean switches share one toggle operation; choices are assigned directly.
pub(super) fn toggle_switch(settings: &mut Settings, key: &str) -> bool {
    let value = match key {
        "core.auto_start" => &mut settings.core.auto_start,
        "core.rollback_on_failure" => &mut settings.core.rollback_on_failure,
        "core.use_managed" => &mut settings.core.use_managed,
        "ui.show_footer" => &mut settings.ui.show_footer,
        "ui.color" => &mut settings.ui.color,
        "stream.traffic" => &mut settings.stream.traffic,
        "stream.memory" => &mut settings.stream.memory,
        "stream.logs" => &mut settings.stream.logs,
        "stream.connections" => &mut settings.stream.connections,
        "update.update_on_start" => &mut settings.update.update_on_start,
        "update.close_connections_on_apply" => &mut settings.update.close_connections_on_apply,
        "update.prefer_hot_reload" => &mut settings.update.prefer_hot_reload,
        _ => return false,
    };
    *value = !*value;
    true
}

/// Validate a picker value against its descriptor before assigning it.
pub(super) fn set_setting_choice(
    settings: &mut Settings,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let allowed = setting_rows(settings)
        .iter()
        .find(|row| row.key == key)
        .is_some_and(|row| match row.kind {
            SettingKind::Choice { options } => options.contains(&value),
            SettingKind::Number { presets } => value
                .parse::<i64>()
                .is_ok_and(|number| presets.contains(&number)),
            SettingKind::Bool | SettingKind::Text => false,
        });
    if !allowed {
        return Err(format!("invalid setting choice: {key} = {value}"));
    }
    set_setting_text(settings, key, value)
}

/// Apply an edit atomically, validating the complete settings before accepting it.
/// Invalid values leave the previous settings intact.
pub(super) fn set_setting_text(
    settings: &mut Settings,
    key: &str,
    text: &str,
) -> Result<(), String> {
    let before = settings.clone();
    let result = set_setting_text_inner(settings, key, text);
    if let Err(reason) = result {
        *settings = before;
        return Err(reason);
    }
    match settings.validate() {
        Ok(()) => Ok(()),
        Err(error) => {
            *settings = before;
            Err(error.to_string())
        }
    }
}

fn set_setting_text_inner(settings: &mut Settings, key: &str, text: &str) -> Result<(), String> {
    let text = text.trim();
    match key {
        "ui.language" => {
            settings.ui.language = match text {
                "en" => Language::English,
                "zh-CN" => Language::Chinese,
                _ => return Err("unsupported interface language".to_owned()),
            };
            Ok(())
        }
        "ui.log_level" => {
            settings.ui.log_level =
                LogLevel::parse(text).ok_or_else(|| "invalid log level".to_owned())?;
            Ok(())
        }
        "core.login_autostart" => {
            use cvt_core::settings::LoginAutostart;
            settings.core.login_autostart = match text {
                "off" => LoginAutostart::Off,
                "systemd" => LoginAutostart::Systemd,
                "KDE" => LoginAutostart::Kde,
                "GNOME" => LoginAutostart::Gnome,
                _ => return Err("unsupported login startup method".to_owned()),
            };
            Ok(())
        }
        "core.tun_enabled" => {
            settings.core.tun_enabled = match text {
                "profile" => None,
                "on" => Some(true),
                "off" => Some(false),
                _ => return Err("invalid TUN setting".to_owned()),
            };
            Ok(())
        }
        "logs.max_size_bytes" => {
            settings.logs.max_size_bytes = text
                .parse()
                .map_err(|_| "expected a number of bytes".to_owned())?;
            Ok(())
        }
        "logs.keep" => {
            settings.logs.keep = text.parse().map_err(|_| "expected a number".to_owned())?;
            Ok(())
        }
        "logs.keep_days" => {
            settings.logs.keep_days = text
                .parse()
                .map_err(|_| "expected a number of days".to_owned())?;
            Ok(())
        }
        "core.binary" => {
            settings.core.binary = if text.is_empty() {
                None
            } else {
                Some(PathBuf::from(text))
            };
            Ok(())
        }
        "core.external_controller" => {
            settings.core.external_controller = if text.is_empty() {
                None
            } else {
                Some(text.to_owned())
            };
            Ok(())
        }
        "core.secret" => {
            settings.core.secret = Some(text.to_owned());
            Ok(())
        }
        "test.url" => {
            if !text.starts_with("http://") && !text.starts_with("https://") {
                return Err("the test URL must start with http:// or https://".to_owned());
            }
            text.clone_into(&mut settings.test.url);
            Ok(())
        }
        "test.expected_status" => {
            if text.is_empty() {
                return Err(
                    "a status expression is required; use `*` to accept anything".to_owned(),
                );
            }
            text.clone_into(&mut settings.test.expected_status);
            Ok(())
        }
        "ui.refresh_ms" => {
            let value: u64 = text
                .parse()
                .map_err(|_| "expected a number of ms".to_owned())?;
            settings.ui.refresh_ms = value;
            Ok(())
        }
        "test.timeout_ms" => {
            let value: u32 = text
                .parse()
                .map_err(|_| "expected a number of ms".to_owned())?;
            settings.test.timeout_ms = value;
            Ok(())
        }
        "test.concurrency" => {
            let value: usize = text.parse().map_err(|_| "expected a number".to_owned())?;
            settings.test.concurrency = value;
            Ok(())
        }
        other => Err(format!("`{other}` is not a text setting")),
    }
}

impl App {
    // -- settings -----------------------------------------------------------

    pub(super) fn toggle_setting(&mut self) -> Vec<Effect> {
        let Some(row) = self.settings_rows.selected_item().cloned() else {
            self.refuse("no setting selected");
            return Vec::new();
        };
        let key = row.key;
        match row.kind {
            SettingKind::Text => {
                self.open_prompt(PromptKind::Text, row.editable.clone());
                return Vec::new();
            }
            SettingKind::Choice { options } => {
                self.overlay = Some(Overlay::Picker {
                    title: format!("setting:{key}"),
                    items: options.iter().map(|option| (*option).to_owned()).collect(),
                    selected: options
                        .iter()
                        .position(|option| *option == row.editable)
                        .unwrap_or(0),
                });
                return Vec::new();
            }
            SettingKind::Number { presets } => {
                self.overlay = Some(Overlay::Picker {
                    title: format!("setting:{key}"),
                    items: presets.iter().map(ToString::to_string).collect(),
                    selected: presets
                        .iter()
                        .position(|option| option.to_string() == row.editable)
                        .unwrap_or(0),
                });
                return Vec::new();
            }
            SettingKind::Bool => {}
        }
        if !toggle_switch(&mut self.settings, key) {
            self.refuse(format!("`{key}` cannot be changed here"));
            return Vec::new();
        }
        self.settings_dirty = true;
        let effects = self.after_settings_change();
        let value = self
            .settings_rows
            .items()
            .iter()
            .find(|r| r.key == key)
            .map_or_else(String::new, |r| r.value.clone());
        self.set_status(StatusKind::Info, format!("{} = {value}", row.label));
        effects
    }

    /// Apply a settings change to everything it affects.
    pub(super) fn after_settings_change(&mut self) -> Vec<Effect> {
        self.theme = Theme::from_settings(self.settings.ui.color);
        self.rebuild_settings_rows();
        if self.settings.ui.log_level == self.log_level {
            return Vec::new();
        }
        self.log_level = self.settings.ui.log_level;
        if self.core.is_running() {
            return vec![Effect::SetCoreLogLevel(self.log_level)];
        }
        Vec::new()
    }

    pub(super) fn rebuild_settings_rows(&mut self) {
        let key = self
            .settings_rows
            .selected_item()
            .map(|row| row.key.to_owned());
        let filter = self.settings_rows.filter().to_owned();
        self.settings_rows = Table::from_items(setting_rows(&self.settings));
        if !filter.is_empty() {
            self.settings_rows.set_filter(&filter);
        }
        if let Some(key) = key {
            self.settings_rows
                .select_by_key(key, |row| row.key.to_owned());
        }
    }
}
