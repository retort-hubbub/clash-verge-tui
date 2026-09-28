//! Settings descriptors: labels, editable values and allowed choices.

use crate::state::{Filterable, contains_ignore_case};
use cvt_core::settings::{Language, Settings};

// ---------------------------------------------------------------------------
// settings rows
// ---------------------------------------------------------------------------

/// How a setting's value changes when it is toggled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    /// A switch that flips between yes and no.
    Bool,
    /// A number that steps through a fixed set of sensible values.
    ///
    /// Free-form numbers would need validation feedback for every keystroke;
    /// a list of values the underlying validator accepts cannot be wrong.
    Number {
        /// The values, ascending.
        presets: &'static [i64],
    },
    /// One of a fixed set of strings.
    Choice {
        /// The values, in picker display order.
        options: &'static [&'static str],
    },
    /// Free text, edited in a prompt.
    Text,
}

/// One editable row of the settings screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    /// Dotted key, e.g. `ui.refresh_ms`.
    pub key: &'static str,
    /// Human-readable name.
    pub label: &'static str,
    /// The current value, rendered for the value column.
    pub value: String,
    /// The same value in the form the prompt should be seeded with.
    ///
    /// Separate from `value` because they are not the same string: a setting
    /// that is unset shows `(none)`, and one that falls back to the base
    /// profile shows `from the base profile`. Seeding the prompt with the
    /// *rendering* turned the two keystrokes that open and close it — `s`, then
    /// Enter — into a silent edit that wrote `from the base profile` into
    /// `core.external_controller`, which the settings accepted and the
    /// generator then refused every configuration for.
    ///
    /// Empty means "unset", and the setters read it that way.
    pub editable: String,
    /// What changing it does.
    pub help: &'static str,
    /// How it changes.
    pub kind: SettingKind,
}

impl Filterable for SettingRow {
    fn matches_filter(&self, needle: &str) -> bool {
        contains_ignore_case(self.key, needle)
            || contains_ignore_case(self.label, needle)
            || contains_ignore_case(&self.value, needle)
    }
}

const REFRESH_PRESETS: &[i64] = &[250, 500, 1000, 2000, 5000];
const TIMEOUT_PRESETS: &[i64] = &[1000, 2000, 5000, 10_000, 30_000];
const CONCURRENCY_PRESETS: &[i64] = &[1, 2, 4, 8, 16, 32, 64];

/// Log sizes a person might actually pick, in bytes.
///
/// Zero is first because it is the off switch, and the rest step by a factor of
/// four: a log that is being rotated too often and one that is never rotated are
/// both obvious from the file, unlike a wrong refresh interval.
const LOG_SIZE_PRESETS: &[i64] = &[0, 64 * 1024, 256 * 1024, 1024 * 1024, 8 * 1024 * 1024];

/// How many rotated copies to keep. The validator refuses more than 64.
const LOG_KEEP_PRESETS: &[i64] = &[1, 2, 4, 8, 16, 32, 64];

/// How long a rotated copy may sit before it is deleted.
const LOG_DAYS_PRESETS: &[i64] = &[0, 1, 3, 7, 14, 30, 90];
const LOG_LEVELS: &[&str] = &["silent", "error", "warning", "info", "debug"];
const LANGUAGES: &[&str] = &["en", "zh-CN"];
const LOGIN_AUTOSTART: &[&str] = &["off", "systemd", "KDE", "GNOME"];
const TUN_CHOICES: &[&str] = &["profile", "on", "off"];

/// Every setting the screen offers, with its current value.
#[must_use]
pub fn setting_rows(settings: &Settings) -> Vec<SettingRow> {
    let yes_no = |value: bool| if value { "yes" } else { "no" }.to_owned();
    let mut rows = vec![
        SettingRow {
            key: "core.binary",
            label: "core binary",
            value: settings
                .core
                .binary
                .as_ref()
                .map_or_else(|| "discovered".to_owned(), |p| p.display().to_string()),
            editable: settings
                .core
                .binary
                .as_ref()
                .map_or_else(String::new, |p| p.display().to_string()),
            help: "an explicit path to the mihomo binary; empty means search for one",
            kind: SettingKind::Text,
        },
        SettingRow {
            key: "core.external_controller",
            label: "controller address",
            value: settings
                .core
                .external_controller
                .clone()
                .unwrap_or_else(|| "from the base profile".to_owned()),
            editable: settings
                .core
                .external_controller
                .clone()
                .unwrap_or_default(),
            help: "where the core API listens; overrides the base profile's address",
            kind: SettingKind::Text,
        },
        SettingRow {
            key: "core.secret",
            label: "controller secret",
            value: settings
                .core
                .secret
                .as_ref()
                .map_or_else(|| "none".to_owned(), |_| "set".to_owned()),
            editable: settings.core.secret.clone().unwrap_or_default(),
            help: "the bearer token the core requires; stored in plain text, like the \
                   rest of this file",
            kind: SettingKind::Text,
        },
        SettingRow {
            key: "core.auto_start",
            label: "start the core on launch",
            value: yes_no(settings.core.auto_start),
            editable: yes_no(settings.core.auto_start),
            help: "launch the core as soon as the application starts",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "core.login_autostart",
            label: "start core on login",
            value: settings.core.login_autostart.label().to_owned(),
            editable: settings.core.login_autostart.label().to_owned(),
            help: "register a user-level systemd, KDE or GNOME login startup entry; save to apply",
            kind: SettingKind::Choice {
                options: LOGIN_AUTOSTART,
            },
        },
        SettingRow {
            key: "core.tun_enabled",
            label: "TUN mode",
            value: match settings.core.tun_enabled {
                None => "profile",
                Some(true) => "on",
                Some(false) => "off",
            }
            .to_owned(),
            editable: match settings.core.tun_enabled {
                None => "profile",
                Some(true) => "on",
                Some(false) => "off",
            }
            .to_owned(),
            help: "override the profile's TUN setting; enabling needs network privileges and may change system routes",
            kind: SettingKind::Choice {
                options: TUN_CHOICES,
            },
        },
        SettingRow {
            key: "core.rollback_on_failure",
            label: "roll back a bad configuration",
            value: yes_no(settings.core.rollback_on_failure),
            editable: yes_no(settings.core.rollback_on_failure),
            help: "restore the last snapshot when the core refuses the new one",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "core.use_managed",
            label: "use managed core",
            value: yes_no(settings.core.use_managed),
            editable: yes_no(settings.core.use_managed),
            help: "use TUI-managed core in ~/.config/clash-verge-tui/core/mihomo instead of local/system core",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "ui.language",
            label: "language",
            value: settings.ui.language.native_name().to_owned(),
            editable: settings.ui.language.code().to_owned(),
            help: "interface language; changes immediately and is saved with settings",
            kind: SettingKind::Choice { options: LANGUAGES },
        },
        SettingRow {
            key: "ui.refresh_ms",
            label: "refresh interval",
            value: format!("{} ms", settings.ui.refresh_ms),
            editable: settings.ui.refresh_ms.to_string(),
            help: "how often the interface redraws",
            kind: SettingKind::Number {
                presets: REFRESH_PRESETS,
            },
        },
        SettingRow {
            key: "ui.log_level",
            label: "log level",
            value: settings.ui.log_level.as_str().to_owned(),
            editable: settings.ui.log_level.as_str().to_owned(),
            help: "the minimum level shown, and requested from the core",
            kind: SettingKind::Choice {
                options: LOG_LEVELS,
            },
        },
        SettingRow {
            key: "ui.show_footer",
            label: "show the key hints",
            value: yes_no(settings.ui.show_footer),
            editable: yes_no(settings.ui.show_footer),
            help: "the footer line naming the keys that apply here",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "ui.color",
            label: "colour",
            value: yes_no(settings.ui.color),
            editable: yes_no(settings.ui.color),
            help: "turn colours off for a monochrome terminal",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "test.url",
            label: "latency test URL",
            value: settings.test.url.clone(),
            editable: settings.test.url.clone(),
            help: "must answer 204 without a body, so setup time is measured",
            kind: SettingKind::Text,
        },
        SettingRow {
            key: "test.timeout_ms",
            label: "test timeout",
            value: format!("{} ms", settings.test.timeout_ms),
            editable: settings.test.timeout_ms.to_string(),
            help: "how long a single measurement may take",
            kind: SettingKind::Number {
                presets: TIMEOUT_PRESETS,
            },
        },
        SettingRow {
            key: "logs.max_size_bytes",
            label: "rotate a log after",
            value: crate::state::human_bytes(settings.logs.max_size_bytes),
            editable: settings.logs.max_size_bytes.to_string(),
            help: "the core's log and this program's are rotated when the core is \
                   started; zero turns rotation off",
            kind: SettingKind::Number {
                presets: LOG_SIZE_PRESETS,
            },
        },
        SettingRow {
            key: "logs.keep",
            label: "rotated copies kept",
            value: settings.logs.keep.to_string(),
            editable: settings.logs.keep.to_string(),
            help: "how many older copies to keep per log, oldest dropped first",
            kind: SettingKind::Number {
                presets: LOG_KEEP_PRESETS,
            },
        },
        SettingRow {
            key: "logs.keep_days",
            label: "delete copies after",
            value: format!("{} days", settings.logs.keep_days),
            editable: settings.logs.keep_days.to_string(),
            help: "rotated copies older than this are removed; zero keeps them all",
            kind: SettingKind::Number {
                presets: LOG_DAYS_PRESETS,
            },
        },
        SettingRow {
            key: "test.concurrency",
            label: "parallel tests",
            value: settings.test.concurrency.to_string(),
            editable: settings.test.concurrency.to_string(),
            help: "how many nodes are measured at once",
            kind: SettingKind::Number {
                presets: CONCURRENCY_PRESETS,
            },
        },
        SettingRow {
            key: "test.expected_status",
            label: "expected status",
            value: settings.test.expected_status.clone(),
            editable: settings.test.expected_status.clone(),
            help: "accept this status expression; `*` accepts anything",
            kind: SettingKind::Text,
        },
        SettingRow {
            key: "stream.traffic",
            label: "subscribe to traffic",
            value: yes_no(settings.stream.traffic),
            editable: yes_no(settings.stream.traffic),
            help: "the dashboard's throughput gauges",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "stream.memory",
            label: "subscribe to memory",
            value: yes_no(settings.stream.memory),
            editable: yes_no(settings.stream.memory),
            help: "the core's resident memory reading",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "stream.logs",
            label: "subscribe to logs",
            value: yes_no(settings.stream.logs),
            editable: yes_no(settings.stream.logs),
            help: "the live log stream",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "stream.connections",
            label: "subscribe to connections",
            value: yes_no(settings.stream.connections),
            editable: yes_no(settings.stream.connections),
            help: "the live connection table",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "update.update_on_start",
            label: "update profiles on launch",
            value: yes_no(settings.update.update_on_start),
            editable: yes_no(settings.update.update_on_start),
            help: "download every subscription that is due at start-up",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "update.close_connections_on_apply",
            label: "close connections when applying",
            value: yes_no(settings.update.close_connections_on_apply),
            editable: yes_no(settings.update.close_connections_on_apply),
            help: "so nothing keeps using a node the new configuration dropped",
            kind: SettingKind::Bool,
        },
        SettingRow {
            key: "update.prefer_hot_reload",
            label: "prefer hot reload",
            value: yes_no(settings.update.prefer_hot_reload),
            editable: yes_no(settings.update.prefer_hot_reload),
            help: "hand the configuration to the API instead of restarting",
            kind: SettingKind::Bool,
        },
    ];
    if settings.ui.language == Language::Chinese {
        for row in &mut rows {
            if let Some((label, help)) = crate::i18n::setting_text(settings.ui.language, row.key) {
                row.label = label;
                row.help = help;
            }
            if row.kind == SettingKind::Bool
                || row.key == "core.secret"
                || (row.key == "core.binary" && settings.core.binary.is_none())
                || (row.key == "core.external_controller"
                    && settings.core.external_controller.is_none())
                || row.key == "core.login_autostart"
                || row.key == "core.tun_enabled"
            {
                row.value = crate::i18n::text(settings.ui.language, &row.value).to_owned();
            }
        }
    }
    rows
}
