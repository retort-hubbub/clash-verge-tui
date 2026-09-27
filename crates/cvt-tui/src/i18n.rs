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

/// Messages whose values are supplied by the running application.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Message<'a> {
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
    QueuedTests(usize),
    RunningTests(usize),
    QueuedTestsHint(usize),
    TestsTitle(usize),
    HelpTitle(usize),
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
        (Language::English, Message::ProfilesTitle { shown, total }) => {
            format!(" profiles ({shown} shown of {total}) ")
        }
        (Language::Chinese, Message::ProfilesTitle { shown, total }) => {
            format!(" 配置（显示 {shown} / {total}） ")
        }
        (
            Language::English,
            Message::ProxiesTitle {
                groups,
                shown,
                total,
            },
        ) => format!(" proxies ({groups} group(s) · {shown} of {total} rows shown) "),
        (
            Language::Chinese,
            Message::ProxiesTitle {
                groups,
                shown,
                total,
            },
        ) => format!(" 代理（{groups} 组 · 显示 {shown} / {total} 行） "),
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
        (Language::English, Message::QueuedTests(count)) => format!("{count} queued or running"),
        (Language::Chinese, Message::QueuedTests(count)) => format!("{count} 项等待或运行中"),
        (Language::English, Message::RunningTests(count)) => format!("{count} test(s) running"),
        (Language::Chinese, Message::RunningTests(count)) => format!("{count} 项测试运行中"),
        (Language::English, Message::QueuedTestsHint(count)) => {
            format!("{count} check(s) queued or running — s stops the batch")
        }
        (Language::Chinese, Message::QueuedTestsHint(count)) => {
            format!("{count} 项等待或运行中 — 按 s 停止批次")
        }
        (Language::English, Message::TestsTitle(queued)) if queued > 0 => {
            format!(" tests ({queued} queued or running) ")
        }
        (Language::Chinese, Message::TestsTitle(queued)) if queued > 0 => {
            format!(" 测试（{queued} 项等待或运行中） ")
        }
        (language, Message::TestsTitle(_)) => format!(" {} ", text(language, "tests")),
        (Language::English, Message::HelpTitle(columns)) => {
            format!(" key reference ({columns} column(s)) ")
        }
        (Language::Chinese, Message::HelpTitle(columns)) => format!(" 快捷键 ({columns} 栏) "),
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
}

/// Look up a message by its semantic identity.
#[must_use]
pub(crate) const fn label(language: Language, key: TextKey) -> &'static str {
    match (language, key) {
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
        Action::ToggleInChain => "配置链",
        Action::PreviewConfig => "预览",
        Action::ApplyConfig => "应用",
        Action::RollbackConfig => "回滚",
        Action::SelectNode => "选择",
        Action::TestGroup => "测试组",
        Action::TestNode => "测试节点",
        Action::TestAllNodes => "全部测试",
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
        Action::ToggleDisabledRules => "显示已禁用",
        Action::RunTests => "运行",
        Action::CancelTests => "停止测试",
        Action::ClearTestResults => "清空结果",
        Action::StartCore => "启动内核",
        Action::StopCore => "停止内核",
        Action::RestartCore => "重启内核",
        Action::CycleCoreMode => "切换模式",
        Action::UpgradeCore => "下载/更新托管内核",
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
        // Action descriptions.
        "leave clash-verge-tui (the core keeps running)" => "退出界面（内核继续运行）",
        "dashboard: core status, throughput, quick actions" => "查看内核状态、流量和快捷操作",
        "subscriptions and the configuration chain" => "管理订阅和配置链",
        "proxy groups and node selection" => "查看代理组并选择节点",
        "connections the core is currently proxying" => "查看当前代理连接",
        "live log stream from the core" => "查看内核实时日志",
        "routing rules and rule providers" => "查看路由规则和规则集",
        "latency tests" => "运行延迟测试",
        "application and core settings" => "修改程序和内核设置",
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
        "include disabled rules in the list" => "在列表中显示已禁用规则",
        "run the highlighted test" => "运行当前测试",
        "stop the running batch" => "停止当前批次",
        "forget cached latency results" => "清除已缓存的测试结果",
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
        "net" => "网络",
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
        "keys" => "按键",
        "applies" => "适用范围",
        "action" => "操作",
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
        "chain and selection" => "配置链与选择",
        "settings file" => "设置文件",
        "rule sets" => "规则集",
        "local document" => "本地文件",
        "base only (no patches are chained)" => "仅基础配置（未串联扩展）",
        "no profiles yet — press `a` to add one" => "暂无配置，按 a 添加",
        "no proxies yet — apply a profile, or start the core to see its groups" => {
            "暂无代理，应用配置或启动内核后查看"
        }
        "no connections — the core reports them only while it is running" => {
            "暂无连接；仅在内核运行时显示"
        }
        "no rules — apply a profile, or press h to include the disabled ones" => {
            "暂无规则；请应用配置，或按 h 显示已禁用规则"
        }
        "no checks are available yet — load a profile so there is something to test" => {
            "暂无可用测试；请先加载配置"
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
        "Enter on a member pins it; x clears the choice" => "对节点按 Enter 固定，按 x 取消固定",
        "the core picks the member itself; it cannot be pinned" => {
            "此组由内核自动选择，无法固定节点"
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
        "group latency" => "代理组延迟",
        "node latency" => "节点延迟",
        "core health" => "内核状态",
        "dns lookup" => "DNS 查询",
        "download speed" => "下载速度",
        "current route" => "当前路由",
        "test mode" => "测试模式",
        "change between proxy URL, direct TCP and direct ICMP probes" => {
            "切换代理 URL、直连 TCP 和直连 ICMP 探测"
        }
        "order members within each group by source order or measured latency" => {
            "按配置顺序或测速结果排列组内节点"
        }
        "method" => "方式",
        "CONNECT uses the named proxy; v cycles test methods" => {
            "CONNECT 经指定代理测试；按 v 切换方式"
        }
        "TCP and ICMP probe the server directly; v cycles test methods" => {
            "TCP 与 ICMP 直测服务器；按 v 切换方式"
        }
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
        "downloads up to 4 MB through the current route; this does not measure an individual node" => {
            "通过当前路由下载最多 4 MB；此项并非单节点测速"
        }
        " · unsaved changes" => " · 有未保存的修改",
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
        "Enter pins this node in its group, x lets the group choose again" => {
            "Enter 固定此节点，x 恢复自动选择"
        }
        "apply a profile or start the core; t tests, T tests the group, a tests everything" => {
            "应用配置或启动内核；t 测试节点，T 测试组，a 测试全部"
        }
        "Enter toggles the highlighted rule; h includes disabled rules" => {
            "Enter 切换规则状态，h 显示已禁用规则"
        }
        "Enter runs the highlighted check" => "按 Enter 运行当前检查",
        "not running; latency checks need it, so start it from Home" => {
            "内核未运行；延迟测试需要内核，请在首页启动"
        }
        "Enter flips this switch" => "按 Enter 切换此开关",
        "Enter or Space cycles this value" => "按 Enter 或空格切换此值",
        "Enter opens a prompt for this value" => "按 Enter 输入此值",
        "press a to add a profile from the Profiles screen" => "请在配置页按 a 添加配置",
        "the file is out of date — press s to write it" => "设置尚未保存；按 s 写入文件",
        "everything here is on disk" => "所有设置均已保存",
        "an invalid value is refused before it can be written" => "无效值不会写入文件",
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
        "stop the core? nothing will be proxied" => "是否停止内核？停止后将无法代理流量",
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
        "download or update the managed mihomo core" => "下载或更新托管的 mihomo 内核",
        "switch rule, global and direct routing modes" => "切换规则、全局和直连模式",
        "downloading latest mihomo core..." => "正在下载最新的 mihomo 内核...",
        "managed" => "托管",
        "local" => "本地",
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
}
