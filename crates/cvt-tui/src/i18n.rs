//! Interface text shared by renderers, the key reference and settings rows.
//!
//! Setting keys, actions and context-specific labels have stable identities.
//! Remaining shared copy uses the English text as a compatibility lookup.
//! Profile names, rule payloads, log messages and controller errors are data.

use cvt_core::mihomo::supervisor::CoreStatus;
use cvt_core::profile::item::ProfileType;
use cvt_core::settings::Language;

use crate::action::Screen;
use crate::row::ProfileRow;

/// Explain observable ownership and the exact local-only change being offered.
pub(crate) fn dns_conflict(
    language: Language,
    conflict: &cvt_core::mihomo::listeners::DnsConflict,
) -> String {
    use cvt_core::mihomo::listeners::OwnerKind;
    let chinese = language == Language::Chinese;
    let mut lines = vec![if chinese {
        format!("DNS 监听地址 {} 存在冲突：", conflict.requested)
    } else {
        format!("DNS listener {} conflicts with:", conflict.requested)
    }];
    for owner in &conflict.owners {
        let kind = match (language, owner.kind) {
            (Language::Chinese, OwnerKind::SystemDns) => "系统 DNS 服务",
            (Language::Chinese, OwnerKind::ProxyCore) => "其他代理内核",
            (Language::Chinese, OwnerKind::Unknown) => "未知或无法读取身份的程序",
            (_, OwnerKind::SystemDns) => "system DNS service",
            (_, OwnerKind::ProxyCore) => "another proxy core",
            (_, OwnerKind::Unknown) => "unknown or inaccessible process",
        };
        let process = owner.process.as_deref().unwrap_or("?");
        let pid = owner
            .pid
            .map_or_else(|| "?".to_owned(), |pid| pid.to_string());
        let inferred = if owner.inferred {
            if chinese {
                "（根据 resolved 活跃状态和 stub 地址推断，未确认 PID）"
            } else {
                " (inferred from the active resolved stub; PID unverified)"
            }
        } else {
            ""
        };
        lines.push(format!(
            "{} {} — {kind}: {process}, PID {pid}{inferred}",
            owner.protocol, owner.address
        ));
    }
    if conflict.owners.is_empty() {
        lines.push(
            if chinese {
                "地址被占用，但无法读取占用者身份。"
            } else {
                "Address in use; owner information is unavailable."
            }
            .to_owned(),
        );
    }
    lines.insert(
        1,
        if chinese {
            format!("是否使用 {} 并重试？", conflict.replacement)
        } else {
            format!("Use {} and retry?", conflict.replacement)
        },
    );
    lines.push(if chinese {
        "保存为默认 DNS 监听地址，仅本机可访问。".to_owned()
    } else {
        "Save as the default DNS listener. Local clients only.".to_owned()
    });
    lines.join("\n")
}

/// Messages whose values are supplied by the running application.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Message<'a> {
    CoreAuthorization {
        capabilities: &'a str,
        binary: &'a str,
    },
    ProfilesTitle {
        shown: usize,
        total: usize,
    },
    ProxiesTitle {
        groups: usize,
        shown: usize,
        total: usize,
    },
    ConnectionsTitle {
        count: usize,
        sort: &'a str,
    },
    ConnectionTotal {
        shown: usize,
        total: usize,
        bytes: &'a str,
    },
    HomeTraffic {
        down: &'a str,
        up: &'a str,
        total: &'a str,
        connections: usize,
    },
    RunningTests(usize),
    TestsTitle(usize),
    LogLines(usize),
    LogNew(usize),
    LogDropped(u64),
    RulesTitle {
        shown: usize,
        hidden: usize,
    },
    RuleCounters {
        hits: u64,
        misses: u64,
    },
    RuleNeverMatched(u64),
}

/// Render a formatted message by semantic identity.
#[must_use]
pub(crate) fn message(language: Language, message: Message<'_>) -> String {
    match (language, message) {
        (
            Language::English,
            Message::CoreAuthorization {
                capabilities,
                binary,
            },
        ) => {
            let dns = if capabilities.contains("cap_net_admin") {
                " With systemd-resolved, also authorize DNS management on cvt-mihomo."
            } else {
                ""
            };
            format!("Grant network permissions to {binary}? Authorization persists.{dns}")
        }
        (
            Language::Chinese,
            Message::CoreAuthorization {
                capabilities,
                binary,
            },
        ) => {
            let dns = if capabilities.contains("cap_net_admin") {
                "使用 systemd-resolved 时，还将授权管理 cvt-mihomo 的 DNS。"
            } else {
                ""
            };
            format!("为 {binary} 授予网络权限？授权持续生效。{dns}")
        }
        (Language::English, Message::ProfilesTitle { shown, total }) => {
            if shown == total {
                " profiles ".to_owned()
            } else {
                format!(" profiles ({shown}/{total}) ")
            }
        }
        (Language::Chinese, Message::ProfilesTitle { shown, total }) => {
            if shown == total {
                " 配置 ".to_owned()
            } else {
                format!(" 配置（{shown}/{total}） ")
            }
        }
        (
            Language::English,
            Message::ProxiesTitle {
                groups,
                shown,
                total,
            },
        ) => {
            if shown == total {
                format!(" proxies ({groups} groups) ")
            } else {
                format!(" proxies ({groups} groups · {shown}/{total}) ")
            }
        }
        (
            Language::Chinese,
            Message::ProxiesTitle {
                groups,
                shown,
                total,
            },
        ) => {
            if shown == total {
                format!(" 代理（{groups} 组） ")
            } else {
                format!(" 代理（{groups} 组 · {shown}/{total}） ")
            }
        }
        (Language::English, Message::ConnectionsTitle { count, sort }) => {
            format!(" connections ({count}) · sorted by {sort} ")
        }
        (Language::Chinese, Message::ConnectionsTitle { count, sort }) => {
            format!(" 连接（{count}）· 排序：{sort} ")
        }
        (
            Language::English,
            Message::ConnectionTotal {
                shown,
                total,
                bytes,
            },
        ) => format!("{shown} shown of {total}, {bytes} transferred"),
        (
            Language::Chinese,
            Message::ConnectionTotal {
                shown,
                total,
                bytes,
            },
        ) => format!("显示 {shown} / {total}，已传输 {bytes}"),
        (
            Language::English,
            Message::HomeTraffic {
                down,
                up,
                total,
                connections,
            },
        ) => format!("↓ {down}   ↑ {up}   total {total}   {connections} connection(s)"),
        (
            Language::Chinese,
            Message::HomeTraffic {
                down,
                up,
                total,
                connections,
            },
        ) => format!("↓ {down}   ↑ {up}   总计 {total}   {connections} 个连接"),
        (Language::English, Message::RunningTests(count)) => format!("{count} test(s) running"),
        (Language::Chinese, Message::RunningTests(count)) => format!("{count} 项测试运行中"),
        (Language::English, Message::TestsTitle(queued)) if queued > 0 => {
            format!(" tests ({queued} queued or running) ")
        }
        (Language::Chinese, Message::TestsTitle(queued)) if queued > 0 => {
            format!(" 测试（{queued} 项等待或运行中） ")
        }
        (language, Message::TestsTitle(_)) => format!(" {} ", text(language, "tests")),
        (Language::English, Message::LogLines(count)) => format!("{count} line(s)"),
        (Language::Chinese, Message::LogLines(count)) => format!("{count} 行"),
        (Language::English, Message::LogNew(count)) => format!("{count} new"),
        (Language::Chinese, Message::LogNew(count)) => format!("{count} 行新日志"),
        (Language::English, Message::LogDropped(count)) => format!("{count} dropped"),
        (Language::Chinese, Message::LogDropped(count)) => format!("丢弃 {count} 行"),
        (Language::English, Message::RulesTitle { shown, hidden }) if hidden > 0 => {
            format!(" rules ({shown} shown, {hidden} disabled hidden) ")
        }
        (Language::English, Message::RulesTitle { shown, .. }) => {
            format!(" rules ({shown} shown) ")
        }
        (Language::Chinese, Message::RulesTitle { shown, hidden }) if hidden > 0 => {
            format!(" 规则（显示 {shown}，隐藏 {hidden} 条已禁用） ")
        }
        (Language::Chinese, Message::RulesTitle { shown, .. }) => format!(" 规则（显示 {shown}） "),
        (Language::English, Message::RuleCounters { hits, misses }) => {
            format!("{hits} hit(s), {misses} miss(es)")
        }
        (Language::Chinese, Message::RuleCounters { hits, misses }) => {
            format!("命中 {hits} 次，未命中 {misses} 次")
        }
        (Language::English, Message::RuleNeverMatched(count)) => {
            format!("evaluated {count} times and never matched — an earlier rule probably wins")
        }
        (Language::Chinese, Message::RuleNeverMatched(count)) => {
            format!("已检查 {count} 次但从未命中；可能被前面的规则覆盖")
        }
    }
}

/// Full tab label selected by screen identity.
#[must_use]
pub(crate) fn tab_title(language: Language, screen: Screen) -> &'static str {
    if language == Language::English {
        return screen.title();
    }
    match screen {
        Screen::Home => "首页",
        Screen::Profiles => "配置",
        Screen::Proxies => "代理",
        Screen::Connections => "连接",
        Screen::Logs => "日志",
        Screen::Rules => "规则",
        Screen::Tests => "测试",
        Screen::Settings => "设置",
        Screen::Help => "帮助",
    }
}

/// Compact tab text for terminals that cannot fit full tab names.
#[must_use]
pub(crate) const fn short_tab(language: Language, screen: Screen) -> &'static str {
    match (language, screen) {
        (Language::English, Screen::Home) => "Hm",
        (Language::English, Screen::Profiles) => "Pf",
        (Language::English, Screen::Proxies) => "Px",
        (Language::English, Screen::Connections) => "Cn",
        (Language::English, Screen::Logs) => "Lg",
        (Language::English, Screen::Rules) => "Rl",
        (Language::English, Screen::Tests) => "Ts",
        (Language::English, Screen::Settings) => "St",
        (Language::English, Screen::Help) => "Hp",
        (Language::Chinese, Screen::Home) => "首",
        (Language::Chinese, Screen::Profiles) => "配",
        (Language::Chinese, Screen::Proxies) => "代",
        (Language::Chinese, Screen::Connections) => "连",
        (Language::Chinese, Screen::Logs) => "日",
        (Language::Chinese, Screen::Rules) => "规",
        (Language::Chinese, Screen::Tests) => "测",
        (Language::Chinese, Screen::Settings) => "设",
        (Language::Chinese, Screen::Help) => "帮",
    }
}

/// A profile's role, preserving its type and chain state as separate data.
#[must_use]
pub(crate) fn profile_role(language: Language, profile: &ProfileRow) -> String {
    if profile.base_scope.is_some() {
        return match language {
            Language::English => "override · per-profile",
            Language::Chinese => "覆写 · 订阅专属",
        }
        .to_owned();
    }
    if language == Language::English {
        return profile.role_label();
    }
    let base = match profile.kind {
        ProfileType::Remote => "远程",
        ProfileType::Local => "本地",
        ProfileType::Merge => "合并",
        ProfileType::Override => "覆写",
        ProfileType::Rules => "规则",
        ProfileType::Proxies => "代理",
        ProfileType::Groups => "代理组",
        ProfileType::Script => "脚本",
    };
    let suffix = match (profile.current, profile.in_chain) {
        (true, _) => " · 当前",
        (false, true) => " · 已加入配置链",
        (false, false) if profile.kind.is_patch() => " · 未加入配置链",
        _ => "",
    };
    format!("{base}{suffix}")
}

/// Stable identities for interface text whose meaning cannot be inferred from
/// its English spelling. The same English word can have different translations
/// in different contexts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextKey {
    /// Move the list cursor down.
    CursorDown,
    /// A proxy node is unavailable.
    ProxyUnavailable,
    /// Download throughput gauge.
    DownloadRate,
    /// Upload throughput gauge.
    UploadRate,
    /// Truncated status line overflow indicator.
    StatusOverflowMore,
    /// Confirmation before installing the managed Mihomo binary.
    ManagedCoreConfirmation,
    /// Application update dialog heading.
    ApplicationUpdate,
    /// Install the offered application release.
    UpdateNow,
    /// Postpone a reminder.
    UpdateLater,
    /// Skip the offered release.
    UpdateSkip,
}

/// Look up a message by its semantic identity.
#[must_use]
pub(crate) const fn label(language: Language, key: TextKey) -> &'static str {
    match (language, key) {
        (Language::English, TextKey::ApplicationUpdate) => "Application update",
        (Language::Chinese, TextKey::ApplicationUpdate) => "程序更新",
        (Language::English, TextKey::UpdateNow) => "Update now",
        (Language::Chinese, TextKey::UpdateNow) => "立即更新",
        (Language::English, TextKey::UpdateLater) => "Remind me in one hour",
        (Language::Chinese, TextKey::UpdateLater) => "稍后提醒（一小时后）",
        (Language::English, TextKey::UpdateSkip) => "Skip this version",
        (Language::Chinese, TextKey::UpdateSkip) => "跳过此版本",
        (
            Language::English,
            TextKey::CursorDown | TextKey::ProxyUnavailable | TextKey::DownloadRate,
        ) => "down",
        (Language::Chinese, TextKey::CursorDown) => "下移",
        (Language::Chinese, TextKey::ProxyUnavailable) => "不可用",
        (Language::Chinese, TextKey::DownloadRate) => "下载",
        (Language::English, TextKey::UploadRate) => "up",
        (Language::Chinese, TextKey::UploadRate) => "上传",
        (Language::English, TextKey::StatusOverflowMore) => " … [m: more]",
        (Language::Chinese, TextKey::StatusOverflowMore) => " … [m: 详情]",
        (Language::English, TextKey::ManagedCoreConfirmation) => {
            "download and install the latest managed core?"
        }
        (Language::Chinese, TextKey::ManagedCoreConfirmation) => "是否下载并安装最新的托管内核？",
    }
}

/// Action labels are selected by the action itself, not by their English text.
#[must_use]
pub(crate) fn action_label(language: Language, action: &crate::action::Action) -> &'static str {
    use crate::action::{Action, Screen};
    if language == Language::English {
        return action.label();
    }
    match action {
        Action::Quit => "退出",
        Action::Goto(Screen::Help) => "帮助",
        Action::Goto(_) => "切换页面",
        Action::NextScreen => "下一页",
        Action::PreviousScreen => "上一页",
        Action::Refresh => "刷新",
        Action::Cancel => "取消",
        Action::ShowLastMessage => "消息详情",
        Action::InspectSelection => "完整详情",
        Action::CopySelection => "复制",
        Action::Up => "上移",
        Action::Down => label(language, TextKey::CursorDown),
        Action::PageUp => "上翻页",
        Action::PageDown => "下翻页",
        Action::Top => "顶部",
        Action::Bottom => "底部",
        Action::Search => "搜索",
        Action::SearchNext => "下一结果",
        Action::ActivateProfile => "切换",
        Action::UpdateProfile => "更新",
        Action::UpdateAllProfiles => "全部更新",
        Action::NewProfile => "新建",
        Action::DeleteProfile => "删除",
        Action::RenameProfile => "重命名",
        Action::ImportProfiles => "导入",
        Action::EditProfile => "编辑",
        Action::EditProfileSource => "编辑来源",
        Action::EditProfileOverride => "订阅覆写",
        Action::AddRule => "添加规则",
        Action::AuthorizeCore => "授权内核",
        Action::ResolveDnsConflict => "DNS 监听冲突",
        Action::ToggleInChain => "配置链",
        Action::PreviewConfig => "预览",
        Action::ApplyConfig => "应用",
        Action::RollbackConfig => "回滚",
        Action::SelectNode => "选择",
        Action::TestGroup => "测试组",
        Action::TestNode => "测试节点",
        Action::TestRouteSpeed => "路由测速",
        Action::InstallSpeedtestGo => "安装 speedtest-go",
        Action::TestAllNodes | Action::RunAllTests => "全部测试",
        Action::CycleTestMode => "测试模式",
        Action::ClearNodeSelection => "取消固定",
        Action::CloseConnection => "关闭",
        Action::CloseAllConnections => "全部关闭",
        Action::CycleNodeSort | Action::CycleConnectionSort => "排序",
        Action::ToggleLogFollow => "跟随",
        Action::CycleLogLevel => "级别",
        Action::ClearLogs => "清空",
        Action::ExportLogs => "导出",
        Action::ToggleRule => "切换状态",
        Action::UpdateRuleProvider => "更新规则集",
        Action::UpdateAllRuleProviders => "更新全部规则集",
        Action::ToggleDisabledRules => "显示/隐藏已禁用",
        Action::RunTests => "运行",
        Action::CancelTests => "停止测试",
        Action::ClearTestResults => "清空结果",
        Action::StartCore => "启动内核",
        Action::StopCore => "停止内核",
        Action::RestartCore => "重启内核",
        Action::CycleCoreMode => "切换模式",
        Action::UpgradeCore => "下载/更新托管内核",
        Action::CheckAppUpdate => "检查程序更新",
        Action::UpdateGeo => "更新地理数据",
        Action::FlushCaches => "清空缓存",
        Action::EditRuntimeConfig => "编辑配置",
        Action::SaveSettings => "保存",
        Action::ToggleSetting => "修改",
    }
}

/// Translated setting label and help, selected by the persisted setting key.
/// The English copy lives with the setting declaration in `app::setting_rows`.
#[must_use]
pub(crate) fn setting_text(language: Language, key: &str) -> Option<(&'static str, &'static str)> {
    if language == Language::English {
        return None;
    }
    let pair = match key {
        "core.binary" => ("内核文件", "指定 mihomo 文件路径；留空则自动查找"),
        "core.external_controller" => ("控制器地址", "内核 API 地址；优先于基础配置"),
        "core.secret" => ("控制器密钥", "内核 API 密钥；与其他设置一样以明文保存"),
        "core.auto_start" => ("启动时运行内核", "启动界面时立即启动内核"),
        "core.rollback_on_failure" => ("配置失败时回滚", "内核拒绝新配置时恢复上一个快照"),
        "core.use_managed" => (
            "使用托管内核",
            "优先使用数据目录下由程序下载托管的内核，而非本地内核",
        ),
        "core.login_autostart" => (
            "登录时启动内核",
            "使用用户级 systemd、KDE 或 GNOME 登录启动项；保存后生效",
        ),
        "core.tun_enabled" => (
            "TUN 模式",
            "覆盖配置中的 TUN 开关；启用需要网络权限，并可能改变系统路由",
        ),
        "ui.language" => ("界面语言", "界面语言；立即生效，保存设置后写入磁盘"),
        "ui.refresh_ms" => ("刷新间隔", "界面重绘频率"),
        "ui.log_level" => ("日志级别", "显示并向内核请求的最低日志级别"),
        "ui.show_footer" => ("显示快捷键提示", "在底部显示当前可用快捷键"),
        "ui.color" => ("彩色显示", "在单色终端中关闭彩色显示"),
        "test.url" => ("延迟测试地址", "需返回无正文的 204 响应，以测量连接耗时"),
        "test.timeout_ms" => ("测试超时", "单次测量的最长等待时间"),
        "logs.max_size_bytes" => ("日志轮转阈值", "启动内核时轮转内核和程序日志；零表示关闭"),
        "logs.keep" => ("保留轮转文件数", "每份日志保留的旧文件数，先删除最旧的"),
        "logs.keep_days" => ("删除旧日志天数", "删除超过此天数的轮转日志；零表示保留全部"),
        "test.concurrency" => ("并行测试数", "同时测量的节点数量"),
        "test.expected_status" => ("预期状态码", "接受此状态码表达式；`*` 接受所有状态"),
        "stream.traffic" => ("订阅流量数据", "首页的流量仪表"),
        "stream.memory" => ("订阅内存数据", "内核的常驻内存数据"),
        "stream.logs" => ("订阅日志", "实时日志流"),
        "stream.connections" => ("订阅连接", "实时连接列表"),
        "update.update_on_start" => ("启动时更新订阅", "启动时更新所有到期订阅"),
        "update.close_connections_on_apply" => {
            ("应用配置时关闭连接", "避免现有连接继续使用已被移除的节点")
        }
        "update.prefer_hot_reload" => ("优先热重载", "优先通过 API 应用配置，避免重启"),
        _ => return None,
    };
    Some(pair)
}

/// Render the supervisor state in the selected language.
#[must_use]
pub fn core_status(language: Language, status: &CoreStatus) -> String {
    if language == Language::English {
        return status.label();
    }
    match status {
        CoreStatus::NotInstalled => "未找到内核文件".to_owned(),
        CoreStatus::Stopped => "已停止".to_owned(),
        CoreStatus::Running { pid, .. } => format!("运行中 (pid {pid})"),
        CoreStatus::StalePid { pid } => format!("失效的 pid {pid}"),
    }
}

/// Translate one interface-owned English phrase.
#[must_use]
pub fn text(language: Language, english: &str) -> &str {
    if language == Language::English {
        return english;
    }
    match english {
        "copy" => "复制",
        "copy the selected value or the complete open details" => "复制选中值或完整详情",
        "settings saved; controller secret is empty: API access is unauthenticated" => {
            "设置已保存；控制器密钥为空，API 无需认证即可访问"
        }
        "controller secret is empty: API access is unauthenticated" => {
            "控制器密钥为空，API 无需认证即可访问"
        }
        "nothing to copy" => "没有可复制的内容",
        "copied to clipboard" => "已复制到剪贴板",
        "clipboard request sent; your terminal must allow OSC 52" => {
            "已发送复制请求；终端需要允许 OSC 52"
        }
        // Tabs and help groups.
        "Home" | "home" => "首页",
        "Profiles" | "profiles" => "配置",
        "Proxies" | "proxies" => "代理",
        "Connections" | "connections" => "连接",
        "Logs" | "logs" => "日志",
        "Rules" | "rules" | "rule" => "规则",
        "Tests" | "tests" => "测试",
        "Settings" | "settings" => "设置",
        "Help" | "help" => "帮助",
        "General" => "通用",
        "Navigation" => "导航",
        "Core" | "core" => "内核",
        "anywhere" | "global" => "全局",
        "lists" | "table" => "列表",
        "edit source" => "编辑来源",
        "profile override" => "订阅覆写",
        "add rule" => "添加规则",
        "authorize core" => "授权内核",
        "edit the subscription source URL and download it" => "修改订阅来源 URL 并下载配置",
        "edit the override for this profile in $EDITOR" => "在编辑器中修改此订阅专属的覆写",
        "prepend a rule to the current profile override" => "为当前订阅添加优先匹配的覆写规则",
        "grant only the network capabilities required by the core" => {
            "认证并授予内核所需的网络权限"
        }
        "new rule (TYPE,payload,policy)" => "新增规则（类型,匹配内容,策略）",
        "invalid rule; use TYPE,payload,policy" => "规则格式无效，请输入 类型,匹配内容,策略",
        "a local profile has no subscription URL" => "本地配置没有订阅 URL",
        "select a base profile first" => "请先选择一个基础配置",
        "profile override saved" => "订阅覆写已保存",
        "core startup timed out before configuration synchronization" => {
            "等待内核启动超时，配置尚未同步"
        }
        "belongs to" => "所属订阅",
        "this override follows its subscription automatically" => {
            "此覆写随所属订阅自动启用，无需加入全局配置链"
        }
        // Action descriptions.
        "leave clash-verge-tui (the core keeps running)" => "退出界面（内核继续运行）",
        "dashboard: core status, throughput, quick actions" => "查看内核状态、流量和快捷操作",
        "subscriptions and the configuration chain" => "管理订阅和配置链",
        "proxy groups and node selection" => "查看代理组并选择节点",
        "connections the core is currently proxying" => "查看当前代理连接",
        "live log stream from the core" => "查看内核实时日志",
        "routing rules and rule providers" => "查看路由规则和规则集",
        "application and core settings" => "修改程序和内核设置",
        "check the application release and choose update, later or skip" => {
            "检查程序新版本，可选择更新、稍后提醒或跳过此版本"
        }
        "downloading application update…" => "正在下载程序更新…",
        "application version skipped" => "已跳过此版本",
        "application update postponed for one hour" => "将在一小时后再次提醒更新",
        "no newer application update is available" => "暂无更新的程序版本",
        "an application update is already in progress" => "正在检查或安装程序更新，请稍候",
        "application updated; restart to use the new version" => "程序更新完成，重启后使用新版本",
        "previous executable:" => "旧程序备份：",
        "this reference" => "查看快捷键说明",
        "move to the next tab" => "切换到下一页",
        "move to the previous tab" => "切换到上一页",
        "re-read everything from the core" => "重新读取内核状态",
        "close a prompt, or clear the search" => "关闭弹窗或清除搜索",
        "move the cursor up" => "光标上移",
        "move the cursor down" => "光标下移",
        "move up by a screenful" => "向上翻一页",
        "move down by a screenful" => "向下翻一页",
        "jump to the first row" => "跳到首行",
        "jump to the last row" => "跳到末行",
        "filter the current list" => "筛选当前列表",
        "jump to the next match" => "跳到下一个匹配项",
        "make this profile the base and regenerate" => "设为基础配置并重新生成",
        "download this subscription again" => "重新下载当前订阅",
        "download every subscription that is due" => "更新所有到期订阅",
        "add a subscription by URL, or a blank local profile" => "添加订阅地址或本地配置",
        "delete this profile and its document" => "删除当前配置及其文件",
        "give this profile a new name" => "重命名当前配置",
        "copy profiles out of an existing clash-verge-rev home" => {
            "从现有 clash-verge-rev 目录导入配置"
        }
        "open this profile's document in $EDITOR" => "用 $EDITOR 打开配置文件",
        "include or exclude this patch in the chain" => "在配置链中加入或移除此扩展",
        "show what regenerating would change, without applying it" => "预览重新生成的变化",
        "generate the configuration and hand it to the core" => "生成配置并应用到内核",
        "restore the most recent snapshot" => "恢复最近的配置快照",
        "pin this node in the selected group" => "在当前组固定此节点",
        "measure every member of this group" => "测试当前组的所有节点",
        "measure this node" => "测试当前节点",
        "measure every node the core knows about" => "测试内核中的所有节点",
        "let an automatic group choose again" => "恢复自动选择",
        "drop this connection; the client will reconnect" => "关闭当前连接，客户端可重新连接",
        "drop every connection" => "关闭所有连接",
        "change the ordering of the connection list" => "切换连接排序方式",
        "stop or resume scrolling with new lines" => "暂停或恢复日志跟随",
        "raise or lower the minimum level, on the core too" => "调整最低日志级别并同步到内核",
        "discard the buffered lines" => "清空已缓存的日志",
        "write the buffered lines to a timestamped file" => "将日志导出到带时间戳的文件",
        "enable or disable this rule in the running core" => "在运行中的内核里启用或禁用此规则",
        "download this rule set again" => "重新下载当前规则集",
        "download every rule set again" => "重新下载所有规则集",
        "show or hide disabled rules in the list" => "显示或隐藏列表中的已禁用规则",
        "run the highlighted test" => "运行当前测试",
        "run all unlock checks" => "运行全部解锁测试",
        "stop the running batch" => "停止当前批次",
        "clear unlock check results" => "清除已缓存的测试结果",
        "launch the core with the generated configuration" => "用生成的配置启动内核",
        "stop the core process" => "停止内核进程",
        "stop and start the core" => "重启内核进程",
        "download and install a newer core from its release channel" => "下载并安装更新的内核",
        "download fresh GeoIP and GeoSite databases" => "更新 GeoIP 和 GeoSite 数据库",
        "clear the fake-IP and DNS caches" => "清空 Fake-IP 和 DNS 缓存",
        "open the generated configuration in $EDITOR" => "用 $EDITOR 打开生成的配置",
        "write the settings to disk" => "保存设置到磁盘",
        "change the highlighted setting" => "修改当前设置",
        // Settings values. Labels and help use stable setting keys above.
        "yes" => "是",
        "no" => "否",
        "set" => "已设置",
        "none" => "无",
        "discovered" => "自动查找",
        "from the base profile" => "来自基础配置",
        // Shared labels, empty states and on-screen guidance.
        "name" => "名称",
        "role" => "用途",
        "updated" => "更新于",
        "remaining" => "剩余额度",
        "node" => "节点",
        "group" => "组",
        "type" => "类型",
        "delay" => "延迟",
        "destination" => "目标地址",
        "net" | "network" => "网络",
        "process" => "进程",
        "traffic" => "流量",
        "since" | "opened" => "开始时间",
        "value" => "值",
        "policy" => "策略",
        "hits" => "命中",
        "check" => "检查项目",
        "target" => "目标",
        "result" => "结果",
        "setting" => "设置项",
        "what it does" => "作用",
        "selected action" => "当前操作",
        "selected profile" => "当前配置",
        "keys" => "按键",
        "applies" => "适用范围",
        "action" | "actions" => "操作",
        "state" => "状态",
        "mode" => "模式",
        "direct" => "直连",
        "version" => "版本",
        "data directory" => "数据目录",
        "memory" => "内存",
        "downloaded" => "已下载",
        "uploaded" => "已上传",
        "samples" => "样本数",
        "last preview" => "上次预览",
        "filter" => "筛选",
        "source" => "来源",
        "uid" | "id" => "标识",
        "edits" => "编辑数",
        "cannot run" => "无法运行",
        "hint" | "info" => "提示",
        "behaviour" => "行为",
        "members" => "成员",
        "expanded" => "展开",
        "pinning" => "固定节点",
        "health" => "健康状态",
        "counters" => "计数",
        "note" => "备注",
        "batch" => "批次",
        "file" => "文件",
        "validation" => "验证",
        "changing a row" => "修改设置",
        "throughput" => "吞吐量",
        "readings" => "运行数据",
        "attention" => "注意",
        "selection" => "节点选择",
        "selected connection" => "选中的连接",
        "selected rule" => "选中的规则",
        "selected check" => "选中的检查",
        "settings file" => "设置文件",
        "rule sets" => "规则集",
        "local document" => "本地文件",
        "no profiles yet — press `a` to add one" => "暂无配置，按 a 添加",
        "no proxies yet — apply a profile, or start the core to see its groups" => {
            "暂无代理，应用配置或启动内核后查看"
        }
        "no connections — the core reports them only while it is running" => {
            "暂无连接；仅在内核运行时显示"
        }
        "no rules — apply a profile, or press h to show hidden rules" => {
            "暂无规则；请应用配置，或按 h 显示隐藏规则"
        }
        "no setting matches the filter" => "没有设置项匹配当前筛选",
        "no log lines yet — start the core, and check that stream.logs is on" => {
            "暂无日志；请启动内核并检查 stream.logs 设置"
        }
        "no buffered line matches the filter" => "缓存中没有匹配的日志",
        "no traffic samples yet — start the core, and check that stream.traffic is on" => {
            "暂无流量数据；请启动内核并检查 stream.traffic 设置"
        }
        "the key map is empty" => "快捷键列表为空",
        "none reported by the core" => "内核未报告任何条目",
        "Enter switches profile, c chains a patch, p previews" => {
            "Enter 切换配置，c 加入扩展，p 预览"
        }
        "yes — Enter collapses it" => "是；按 Enter 折叠",
        "no — Enter opens it" => "否；按 Enter 展开",
        "alive" => "可用",
        "not answering" => "无响应",
        "active" => "当前使用",
        "disabled" => "已禁用",
        "enabled" => "已启用",
        "pending" => "待运行",
        "ok" | "success" => "成功",
        "failed" => "失败",
        "running…" => "运行中…",
        "IP" => "IP 信息",
        "IP / system" => "IP / 系统",
        "Clash" => "Clash 信息",
        "Clash / node" => "Clash / 节点",
        "system" => "系统信息",
        "current node" => "当前节点",
        "address" => "地址",
        "country" => "国家或地区",
        "platform" => "平台",
        "processors" => "处理器",
        "selected" => "已选节点",
        "route speed" => "路由测速",
        "route bandwidth" => "路由带宽测速",
        "download sample · 4 MB" => "下载样本 · 4 MB",
        "download sample · 20 MB" => "下载样本 · 20 MB",
        "download sample · 100 MB" => "下载样本 · 100 MB",
        "speedtest-go" => "speedtest-go",
        "install speedtest-go" => "安装 speedtest-go",
        "refreshing IP…" => "正在刷新 IP…",
        "IP lookup failed · r to retry" => "IP 查询失败 · 按 r 重试",
        "r to refresh IP" => "按 r 刷新 IP",
        "Enter one · a all · c clear · s stop" => "Enter 单项 · a 全部 · c 清空 · s 停止",
        "route speed (b)" => "路由测速 (b)",
        "bandwidth mode" => "带宽测速方式",
        "measure current-route download through Mihomo" => "通过 Mihomo 测量当前路由下载速度",
        "download speedtest-go into the application directory" => "下载 speedtest-go 到应用目录",
        "not tested" => "尚未测试",
        "route" => "路径",
        "current route" => "当前路由",
        "requests go through Mihomo’s local proxy" => "请求经过 Mihomo 本地代理",
        "start the core from Home to check service availability" => {
            "请先在首页启动内核再检测服务可用性"
        }
        "checks regional availability through the current Mihomo route" => {
            "通过当前 Mihomo 路由检测地区可用性"
        }
        "group latency" => "代理组延迟",
        "node latency" => "节点延迟",
        "unlock tests" => "解锁测试",
        "test mode" => "测试模式",
        "change between proxy URL, direct TCP and direct ICMP probes" => {
            "切换代理 URL、直连 TCP 和直连 ICMP 探测"
        }
        "order members within each group by source order or measured latency" => {
            "按配置顺序或测速结果排列组内节点"
        }
        "method" => "方式",
        "CONNECT via selected proxy" => "CONNECT 通过指定代理",
        "TCP and ICMP to the server" => "TCP 与 ICMP 直测服务器",
        "manual selection" => "手动选择",
        "automatic selection" => "自动选择",
        "v cycles CONNECT, TCP and ICMP; speed uses the current route" => {
            "按 v 切换 CONNECT、TCP、ICMP；速度测试走当前路由"
        }
        "tests each member with the selected probe method and updates results as they arrive" => {
            "按当前测试方式逐个检测组内节点，并随结果更新"
        }
        "measures one node with the selected probe method" => "按当前测试方式测量单个节点",
        "reads the core's version and reports which optional routes exist" => {
            "读取内核版本并检查可用接口"
        }
        "resolves a name through the core's resolver, so fake-IP mode shows the synthetic address" => {
            "通过内核 DNS 解析域名；Fake-IP 模式会显示虚拟地址"
        }
        " · unsaved changes" => " · 未保存",
        "d closes the highlighted connection; s changes the sort order" => {
            "按 d 关闭当前连接，按 s 切换排序方式"
        }
        "the core reports no counters for this rule" => "内核未提供此规则的计数",
        "following" => "自动跟随",
        "frozen" => "已暂停",
        "nothing running" => "没有运行中的测试",
        "no core binary" => "未找到内核文件",
        "stopped" => "已停止",
        "unknown" => "未知",
        "no mihomo binary " => "未找到 mihomo 文件 ",
        "the core is not running " => "内核未运行 ",
        "no profiles yet " => "暂无配置 ",
        "every live stream is switched off " => "所有实时数据流均已关闭 ",
        "nothing needs attention" => "没有需要处理的问题",
        "press ? for every key" => "按 ? 查看全部快捷键",
        "— set core.binary in Settings, or put one in the core directory" => {
            "— 在设置中指定 core.binary，或把内核放入 core 目录"
        }
        "— start the core again" => "— 请重新启动内核",
        "— press s to start it" => "— 按 s 启动内核",
        "— press 2, then a, to add a subscription" => "— 按 2，再按 a 添加订阅",
        "— the dashboard will stay empty" => "— 首页将不会显示实时数据",
        "Enter toggles the highlighted rule; h shows or hides disabled rules" => {
            "Enter 切换规则状态，h 显示或隐藏已禁用规则"
        }
        "Enter runs the highlighted check" => "按 Enter 运行当前检查",
        "not running; latency checks need it, so start it from Home" => {
            "内核未运行；延迟测试需要内核，请在首页启动"
        }
        "Enter flips this switch" => "按 Enter 切换此开关",
        "Enter or Space cycles this value" => "按 Enter 或空格切换此值",
        "Enter opens a prompt for this value" => "按 Enter 输入此值",
        "press a to add a profile from the Profiles screen" => "请在配置页按 a 添加配置",
        "type to narrow the list · Enter keep · Esc clear" => {
            "输入文字筛选 · Enter 保留 · Esc 清除"
        }
        "Enter accept · Esc cancel" => "Enter 确认 · Esc 取消",
        " [y] yes " => " [y] 是 ",
        "[n] no " => "[n] 否 ",
        "new profile" => "新建配置",
        "generated configuration" => "生成的配置",
        "import from" => "从目录导入",
        "j/k scroll · PgUp/PgDn page · Esc close" => "j/k 滚动 · PgUp/PgDn 翻页 · Esc 关闭",
        "rename profile" => "重命名配置",
        "subscription URL" => "订阅地址",
        "profile name" => "配置名称",
        "cancelled" => "已取消",
        "preview closed" => "预览已关闭",
        "filter cleared" => "已清除筛选",
        "nothing to cancel" => "当前没有可取消的操作",
        "nothing to choose" => "没有可选择的项目",
        "profiles loaded" => "配置已加载",
        "chain saved" => "配置链已保存",
        "connection closed" => "连接已关闭",
        "core stopped" => "内核已停止",
        "core upgraded" => "内核已升级",
        "geo databases updated" => "地理数据库已更新",
        "caches flushed" => "缓存已清空",
        "settings saved" => "设置已保存",
        "natural" => "默认",
        "fastest" => "最快",
        "slowest" => "最慢",
        "busiest" => "流量最高",
        "from a URL" => "从 URL 导入",
        "a blank local profile" => "新建空白本地配置",
        "update rule set" => "更新规则集",
        "delete this profile?" => "是否删除此配置？",
        "stop the core?" => "是否停止内核？",
        "restore the previous generated configuration and restart the core?" => {
            "是否恢复上一次生成的配置并重启内核？"
        }
        "the core is not running" => "内核未运行",
        "offline" => "离线",
        "start the core to choose a member" => "启动内核后可选择成员",
        "there is no profile to delete" => "没有可删除的配置",
        "there is no connection to close" => "没有可关闭的连接",
        "a subscription URL must start with http:// or https://" => {
            "订阅地址必须以 http:// 或 https:// 开头"
        }
        "a profile needs a name" => "配置名称不能为空",
        "no profile selected" => "未选中任何配置",
        "no setting selected" => "未选中任何设置",
        "no clash-verge-rev installation was found to import from" => {
            "未找到可导入的 clash-verge-rev 目录"
        }
        "view the full text of the latest status message" => "查看最近一条状态信息的完整内容",
        "message" => "消息详情",
        "no message to show" => "暂无历史消息",
        "Esc/Enter close" => "Esc/Enter 关闭",
        " [Enter] accept " => " [Enter] 确认 ",
        " [Esc] cancel " => " [Esc] 取消 ",
        "warning" => "警告",
        "error" => "错误",
        "download and replace the core binary?" => "是否下载并替换内核？",
        "download and install the latest managed core?" => "是否下载并安装最新的托管内核？",
        "key reference" => "快捷键说明",
        "Enter: full details" => "Enter：完整说明",
        "view every field of the highlighted row in full" => "完整查看当前条目的全部字段",
        "full details" => "完整详情",
        "F1 details · double click to activate" => "F1 详情 · 双击执行",
        "no selected row to inspect" => "当前没有可查看详情的条目",
        "no log lines yet" => "暂无日志",
        "no node selected" => "未选中节点",
        "no group selected" => "未选中代理组",
        "no connection selected" => "未选中连接",
        "no rule selected" => "未选中规则",
        "no test selected" => "未选中测试项",
        "no test batch is running" => "没有正在运行的测试批次",
        "no unlock checks are available" => "没有可用的解锁测试",
        "the log buffer is empty" => "日志缓冲区为空",
        "downloading speedtest-go…" => "正在下载 speedtest-go…",
        "following new lines" => "正在跟随新日志",
        "follow stopped; new lines are buffered but not shown" => "已暂停跟随；新日志仍会缓存",
        "showing disabled rules" => "正在显示已禁用规则",
        "hiding disabled rules" => "已隐藏禁用规则",
        "the core lists no rule providers; updating every set" => {
            "内核未列出规则集；正在更新全部规则集"
        }
        "download and install speedtest-go in the application directory?" => {
            "是否在应用目录下载并安装 speedtest-go？"
        }
        "download or update the managed mihomo core" => "下载或更新托管的 mihomo 内核",
        "switch rule, global and direct routing modes" => "切换规则、全局和直连模式",
        "downloading latest mihomo core..." => "正在下载最新的 mihomo 内核...",
        "managed" => "托管",
        "local" => "本地",
        "profile" => "跟随配置",
        "on" => "开启",
        "off" => "关闭",
        "saving settings…" => "正在保存设置…",
        "TUN configuration applied" => "TUN 配置已应用",
        "enable TUN and grant the core network capabilities? this may change system routes; the grant persists on the core binary" => {
            "是否启用 TUN 并授予内核网络权限？这可能改变系统路由；授权会保留在内核文件上"
        }
        "core management" => "内核管理",
        "download latest managed core" => "下载最新托管内核",
        "use local core (PATH or core.binary)" => "使用本地内核 (PATH 或自定义路径)",
        "use managed core" => "使用托管内核",
        "— press U for core management, or configure in Settings" => {
            "— 按 U 进行内核管理，或在设置中配置"
        }
        "— press U to install a managed core, or set local path in Settings" => {
            "— 按 U 安装托管内核，或在设置中配置本地内核路径"
        }
        "switched to local core; configure core.binary in Settings or install mihomo in PATH" => {
            "已切换为本地内核；请在设置中配置 core.binary 或将 mihomo 放入 PATH"
        }
        "switched to managed core" => "已切换为托管内核",
        _ => english,
    }
}

/// Translate and format dynamic status messages.
#[must_use]
pub fn format_status(language: Language, text: &str) -> String {
    if language == Language::English {
        return text.to_owned();
    }
    if let Some(error) = text.strip_prefix("application update: ") {
        return format!("程序更新：{error}");
    }
    let direct = self::text(language, text);
    if direct != text {
        return direct.to_owned();
    }

    if let Some(mode) = text.strip_prefix("routing mode: ") {
        return format!("路由模式：{}", self::text(language, mode));
    }

    if let Some(rest) = text.strip_prefix("no mihomo binary found; put one at ") {
        if let Some((path, env_part)) = rest.split_once(" or set ") {
            return format!("未找到 mihomo 内核；请放置于 {path} 或设置环境变量 {env_part}");
        }
        return format!("未找到 mihomo 内核；请放置于 {rest}");
    }
    if let Some(rest) = text
        .strip_prefix("created `")
        .and_then(|s| s.strip_suffix('`'))
    {
        return format!("已创建“{rest}”");
    }
    if let Some(rest) = text
        .strip_prefix("deleted `")
        .and_then(|s| s.strip_suffix('`'))
    {
        return format!("已删除“{rest}”");
    }
    if let Some(rest) = text
        .strip_prefix("renamed to `")
        .and_then(|s| s.strip_suffix('`'))
    {
        return format!("已重命名为“{rest}”");
    }
    if let Some(rest) = text
        .strip_prefix("imported ")
        .and_then(|s| s.strip_suffix(" profile(s)"))
    {
        return format!("已导入 {rest} 个配置");
    }
    if let Some(rest) = text
        .strip_prefix("the core is already running as pid ")
        .and_then(|s| s.strip_suffix("; stop it first"))
    {
        return format!("内核已在运行（PID: {rest}）；请先停止");
    }
    if text.contains(" profile(s) updated, ")
        && text.ends_with(" failed")
        && let Some((up, fail_part)) = text.split_once(" profile(s) updated, ")
    {
        let fail = fail_part.trim_end_matches(" failed");
        return format!("{up} 个配置已更新，{fail} 个失败");
    }
    if let Some(rest) = text.strip_prefix("restored ") {
        return format!("已恢复快照 {rest}");
    }
    if text.starts_with('`') && text.contains("` now uses `") && text.ends_with('`') {
        let inner = &text[1..text.len() - 1];
        if let Some((grp, mem)) = inner.split_once("` now uses `") {
            return format!("“{grp}”已固定使用“{mem}”");
        }
    }
    if text.starts_with('`') && text.ends_with("` chooses automatically again") {
        let grp = &text[1..text.len() - "` chooses automatically again".len()];
        return format!("“{grp}”已恢复自动选择");
    }
    if let Some(rest) = text
        .strip_prefix("measured ")
        .and_then(|s| s.strip_suffix(" node(s)"))
    {
        return format!("已完成 {rest} 个节点的延迟测试");
    }
    if let Some(rest) = text
        .strip_prefix("closed ")
        .and_then(|s| s.strip_suffix(" connection(s)"))
    {
        return format!("已关闭 {rest} 个连接");
    }
    if let Some(rest) = text.strip_prefix("connections sorted by ") {
        return format!("连接已按 {} 排序", self::text(language, rest));
    }
    if let Some(rest) = text.strip_prefix("members sorted by ") {
        return format!("组内节点排序：{}", self::text(language, rest));
    }
    if let Some(rest) = text.strip_prefix("filtering `")
        && let Some((pat, count_part)) = rest.split_once("` — ")
        && let Some((matched, total_part)) = count_part.split_once(" of ")
    {
        let total = total_part.trim_end_matches(" rows");
        return format!("正在筛选“{pat}”——共 {total} 行中的 {matched} 行");
    }
    if text == "the settings on disk changed; your edits are kept — `s` writes them" {
        return "磁盘上的设置已更改；您的修改已保留——按 s 保存".to_owned();
    }
    if let Some(rest) = text.strip_prefix("configuration written (")
        && let Some((changed, _)) = rest.split_once(" change(s)); the core is not running")
    {
        return format!("配置已写入（{changed} 项更改）；内核未运行");
    }

    if let Some(rest) = text.strip_prefix("installed mihomo ") {
        if let Some(ver) = rest.strip_suffix(" (managed)") {
            return format!("已安装 mihomo {ver}（托管）");
        }
        return format!("已安装 mihomo {rest}");
    }

    if let Some(what) = text.strip_suffix(" needs a running core; press `s` on Home to start it") {
        let action = match what {
            "measuring route speed" => "路由测速",
            "testing nodes" => "节点测试",
            "closing connections" | "closing a connection" => "关闭连接",
            "updating rule sets" | "updating a rule set" => "更新规则集",
            "selecting a node" => "选择节点",
            "testing a node" => "测试节点",
            "testing a group" => "测试代理组",
            "clearing a pin" => "取消节点固定",
            "changing a rule" => "修改规则",
            "running a test" | "running unlock checks" => "运行解锁测试",
            _ => what,
        };
        return format!("{action}需要运行中的内核；请在首页按 s 启动");
    }
    if let Some(count) = text
        .strip_prefix("discarded ")
        .and_then(|rest| rest.strip_suffix(" log line(s)"))
    {
        return format!("已清除 {count} 行日志");
    }
    if let Some(count) = text
        .strip_prefix("cancelled ")
        .and_then(|rest| rest.strip_suffix(" test(s)"))
    {
        return format!("已取消 {count} 项测试");
    }
    if let Some(rest) = text.strip_prefix("rule ")
        && let Some((index, state)) = rest.split_once(' ')
        && matches!(state, "enabled" | "disabled")
    {
        return format!(
            "规则 {index} 已{}",
            if state == "enabled" {
                "启用"
            } else {
                "禁用"
            }
        );
    }
    if let Some(count) = text
        .strip_prefix("updated ")
        .and_then(|rest| rest.strip_suffix(" rule set(s)"))
    {
        return format!("已更新 {count} 个规则集");
    }
    if let Some(pid) = text
        .strip_prefix("core started (pid ")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return format!("内核已启动（PID {pid}）");
    }
    if let Some(pid) = text
        .strip_prefix("core restarted (pid ")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return format!("内核已重启（PID {pid}）");
    }
    if let Some(version) = text.strip_prefix("installed speedtest-go ") {
        return format!("已安装 speedtest-go {version}");
    }
    if let Some(path) = text.strip_prefix("logs written to ") {
        return format!("日志已写入 {path}");
    }
    if let Some(target) = text
        .strip_prefix("opened ")
        .and_then(|rest| rest.strip_suffix(" in $EDITOR"))
    {
        return format!("已在 $EDITOR 中打开 {target}");
    }
    if let Some(error) = text.strip_prefix("IP lookup failed: ") {
        return format!("IP 查询失败：{error}");
    }
    if let Some(speed) = text.strip_prefix("current route: ") {
        return format!("当前路由：{speed}");
    }
    if let Some(error) = text.strip_prefix("route speed: ") {
        return format!("路由测速：{error}");
    }
    if let Some(mode) = text
        .strip_prefix("measuring current route with ")
        .and_then(|rest| rest.strip_suffix('…'))
    {
        return format!("正在用 {} 测量当前路由…", self::text(language, mode));
    }
    if let Some(name) = text
        .strip_prefix("switched to `")
        .and_then(|rest| rest.strip_suffix('`'))
    {
        return format!("已切换到“{name}”");
    }
    if let Some(name) = text
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix("` is already current"))
    {
        return format!("“{name}”已是当前配置");
    }
    if let Some(name) = text
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix("` is local; there is nothing to download"))
    {
        return format!("“{name}”是本地配置，无需下载");
    }
    if let Some(name) = text
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix("` removed from the chain"))
    {
        return format!("已从配置链移除“{name}”");
    }
    if let Some(name) = text
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix("` added to the chain"))
    {
        return format!("已将“{name}”加入配置链");
    }
    if let Some(name) = text
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix("` has no members to show"))
    {
        return format!("“{name}”没有可显示的成员");
    }
    if let Some(name) = text
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix("` is not a member of a group"))
    {
        return format!("“{name}”不属于任何代理组");
    }
    if let Some(name) = text
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix("` is not part of a group"))
    {
        return format!("“{name}”不属于任何代理组");
    }
    if let Some(group) = text
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix("` does not choose a node by hand"))
    {
        return format!("“{group}”不支持手动选择节点");
    }
    if let Some(label) = text
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix("` has already run; press c to clear it"))
    {
        return format!("“{label}”已经运行；按 c 清空结果");
    }
    if let Some(key) = text
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix("` cannot be changed here"))
    {
        return format!("无法在这里修改“{key}”");
    }
    if let Some((label, count)) = text
        .strip_prefix("queued `")
        .and_then(|rest| rest.split_once("` ("))
        && let Some(count) = count.strip_suffix(" in the batch)")
    {
        return format!("“{label}”已加入队列（本批共 {count} 项）");
    }
    if let Some(level) = text.strip_prefix("core log level is now ") {
        return format!("内核日志级别已设为 {level}");
    }
    if let Some(level) = text
        .strip_prefix("showing ")
        .and_then(|rest| rest.strip_suffix(" and above; the core is not running"))
    {
        return format!("显示 {level} 及以上级别；内核未运行");
    }
    if let Some(name) = text
        .strip_prefix("delete `")
        .and_then(|rest| rest.strip_suffix("` and its document?"))
    {
        return format!("是否删除“{name}”及其配置文件？");
    }
    if let Some(count) = text
        .strip_prefix("close all ")
        .and_then(|rest| rest.strip_suffix(" connection(s)?"))
    {
        return format!("是否关闭全部 {count} 个连接？");
    }

    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reachable_action_and_tab_has_chinese_text() {
        for screen in crate::action::Screen::all() {
            assert_ne!(text(Language::Chinese, screen.title()), screen.title());
        }
        for binding in crate::keys::Keymap::new().bindings() {
            assert_ne!(
                action_label(Language::Chinese, &binding.action),
                binding.action.label(),
                "{:?}",
                binding.action
            );
            for english in [
                binding.action.help(),
                binding.action.group(),
                binding.context.label(),
            ] {
                assert_ne!(text(Language::Chinese, english), english, "{english}");
            }
        }
    }

    #[test]
    fn settings_descriptions_have_chinese_text() {
        let mut settings = cvt_core::settings::Settings::default();
        settings.ui.language = Language::Chinese;
        for row in crate::app::setting_rows(&settings) {
            assert!(!row.label.is_ascii(), "{} label", row.key);
            assert!(!row.help.is_ascii(), "{} help", row.key);
        }
    }

    #[test]
    fn one_english_word_can_have_distinct_semantic_translations() {
        assert_eq!(label(Language::English, TextKey::CursorDown), "down");
        assert_eq!(label(Language::English, TextKey::ProxyUnavailable), "down");
        assert_eq!(label(Language::Chinese, TextKey::CursorDown), "下移");
        assert_eq!(
            label(Language::Chinese, TextKey::ProxyUnavailable),
            "不可用"
        );
    }

    #[test]
    fn common_interaction_statuses_are_localized() {
        for english in [
            "download and install the latest managed core?",
            "following new lines",
            "showing disabled rules",
            "hiding disabled rules",
            "no rule selected",
            "no test batch is running",
            "the log buffer is empty",
            "rule 12 disabled",
            "rule 12 enabled",
            "closed 3 connection(s)",
            "core started (pid 123)",
            "core restarted (pid 123)",
            "changing a rule needs a running core; press `s` on Home to start it",
            "discarded 3 log line(s)",
            "installed speedtest-go v1",
            "delete `example` and its document?",
            "close all 3 connection(s)?",
        ] {
            assert_ne!(
                format_status(Language::Chinese, english),
                english,
                "{english}"
            );
        }
    }
}
