//! The command line surface.
//!
//! Everything a user can type is declared here and nowhere else: the parser is
//! the only place that knows about flags, and the command modules receive
//! plain argument structs. That keeps `--help` honest by construction — there
//! is no second list of options to keep in step.
//!
//! Plain `clap` derive, with the exit-code block in `--help` so a script author
//! finds the contract where they look first.

use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args, Parser, Subcommand, ValueEnum};
use cvt_core::ReloadMode;

use crate::exit::EXIT_CODES_HELP;

/// Manage the mihomo (Clash.Meta) core and its configuration.
#[derive(Debug, Parser)]
#[command(
    name = "clash-verge-tui",
    version,
    about = "Manage the mihomo (Clash.Meta) core from a terminal UI or from scripts",
    after_help = EXIT_CODES_HELP,
    disable_help_subcommand = true,
    max_term_width = 100,
)]
pub struct Cli {
    /// Application home directory [env: CVT_HOME]
    #[arg(long, global = true, value_name = "DIR")]
    pub home: Option<PathBuf>,

    /// Machine-readable output on stdout
    #[arg(long, global = true)]
    pub json: bool,

    /// More diagnostics on stderr; repeat for debug and trace
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Never emit ANSI colour
    #[arg(long, global = true)]
    pub no_color: bool,

    /// What to do. With no subcommand, the terminal interface starts.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Every subcommand.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Report core state, the current profile, the generated configuration and
    /// the controller endpoint
    Status,

    /// Diagnose the installation: run every check in order, and exit non-zero
    /// if one of them failed
    Doctor,

    /// Manage subscription and local profiles
    Profiles {
        /// Which profile operation to run
        #[command(subcommand)]
        command: ProfilesCommand,
    },

    /// Show the address and location the traffic comes out at
    Geo(GeoArgs),

    /// Check whether the exit can reach the services people ask about
    Unlock(UnlockArgs),

    /// Save or restore the profiles, settings and overrides
    Backup {
        /// Which backup operation to run
        #[command(subcommand)]
        command: BackupCommand,
    },

    /// Work with the generated runtime configuration
    Config {
        /// Which configuration operation to run
        #[command(subcommand)]
        command: ConfigCommand,
    },

    /// Inspect and steer proxy groups
    Proxies {
        /// Which proxy operation to run
        #[command(subcommand)]
        command: ProxiesCommand,
    },

    /// Inspect and close live connections
    Connections {
        /// Which connection operation to run
        #[command(subcommand)]
        command: ConnectionsCommand,
    },

    /// Inspect rules and rule providers
    Rules {
        /// Which rule operation to run
        #[command(subcommand)]
        command: RulesCommand,
    },

    /// Run connectivity tests through the core
    Test {
        /// Which test to run
        #[command(subcommand)]
        command: TestCommand,
    },

    /// Manage the mihomo process
    Core {
        /// Which core operation to run
        #[command(subcommand)]
        command: CoreCommand,
    },

    /// Print the core log, or follow it live
    Logs(LogsArgs),

    /// Print the tab titles and key bindings as a plain-text reference
    Theme,
}

/// Profile operations.
#[derive(Debug, Subcommand)]
pub enum ProfilesCommand {
    /// List every profile
    List,

    /// Add a subscription, download it, and leave it ready to switch to
    Add(AddArgs),

    /// Remove a profile and its document
    Remove {
        /// Profile uid, as printed by `profiles list`
        uid: String,
    },

    /// Rename a profile; its uid and document do not change
    Rename {
        /// Profile uid
        uid: String,
        /// New display name
        name: String,
    },

    /// Point a remote profile at a different subscription URL
    EditUrl {
        /// Profile uid
        uid: String,
        /// The new subscription URL
        url: String,

        /// Change the URL without fetching it
        #[arg(long)]
        no_fetch: bool,
    },

    /// Make a profile the base of the generated configuration
    Switch {
        /// Profile uid
        uid: String,
    },

    /// Refresh a profile from its provider
    Update(UpdateArgs),

    /// Import profiles from a clash-verge-rev home directory
    Import {
        /// Directory holding a `profiles.yaml`; the clash-verge-rev data
        /// directory is usually the right one
        dir: PathBuf,
    },

    /// Show the patch chain, or replace it
    Chain(ChainArgs),

    /// Print a profile's document
    Show {
        /// Profile uid
        uid: String,
    },
}

/// Arguments of `profiles add`.
#[derive(Debug, Args)]
pub struct AddArgs {
    /// Subscription URL to download from
    pub url: String,

    /// Display name; defaults to the host of the URL
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
}

/// Arguments of `profiles update`.
#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Profile to refresh; defaults to the current one
    pub uid: Option<String>,

    /// Refresh every remote profile whose interval has elapsed
    #[arg(long, conflicts_with = "uid")]
    pub all_due: bool,
}

/// Arguments of `profiles chain`.
#[derive(Debug, Args)]
pub struct ChainArgs {
    /// Patch profiles, in the order they should be applied
    pub uids: Vec<String>,

    /// Drop the explicit chain and go back to the automatic order
    #[arg(long, conflicts_with = "uids")]
    pub clear: bool,
}

/// Backup operations.
#[derive(Debug, Subcommand)]
pub enum BackupCommand {
    /// Copy the profiles, settings and overrides into a timestamped directory
    Create,

    /// List the backups, newest first
    List,

    /// Put a backup back, keeping a copy of what it replaces
    Restore {
        /// The backup's name, from `backup list`; the newest when omitted
        name: Option<String>,
    },
}

/// Configuration operations.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Generate the runtime configuration
    Generate(GenerateArgs),

    /// Print the deployed runtime configuration
    Show,

    /// Validate what would be generated, without writing anything
    Validate,

    /// Show how the generated configuration differs from the deployed one
    Diff,

    /// Restore the most recent snapshot of the runtime configuration
    Rollback,

    /// List the snapshots, newest first
    Snapshots,

    /// Open the runtime configuration in `$VISUAL` or `$EDITOR`
    Edit,

    /// Print the paths the application uses
    Path,
}

/// Arguments of `config generate`.
#[derive(Debug, Args)]
pub struct GenerateArgs {
    /// Write the configuration and hand it to the core
    #[arg(long)]
    pub apply: bool,

    /// Commit even though validation reported errors
    #[arg(long, requires = "apply")]
    pub force: bool,

    /// How the change reaches the core
    #[arg(long, value_enum, default_value_t = Mode::Auto, requires = "apply")]
    pub mode: Mode,
}

/// How a configuration change reaches the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Try the API, restarting only if that fails. The default.
    Auto,
    /// Only ever use the API; a failure is reported rather than worked around.
    Hot,
    /// Always restart the process.
    Restart,
}

impl From<Mode> for ReloadMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Auto => Self::Auto,
            Mode::Hot => Self::HotReload,
            Mode::Restart => Self::Restart,
        }
    }
}

/// Proxy operations.
#[derive(Debug, Subcommand)]
pub enum ProxiesCommand {
    /// List every group, or the members of one group
    List {
        /// Group to expand; omit to list the groups themselves
        group: Option<String>,
    },

    /// Show which proxies a proxy dials through, and where that ends
    Chain {
        /// Proxy name
        node: String,
    },

    /// Pin a group's selection to one of its nodes
    Select {
        /// Group name
        group: String,
        /// Node name; must be a member of the group
        node: String,
    },

    /// Measure latency for every node of one group
    Test(TestArgs),

    /// Measure latency for every node of every group
    TestAll(NodeTestArgs),

    /// Clear a pinned selection, handing control back to the group's strategy
    Unpin {
        /// Group name
        group: String,
    },
}

/// Arguments of `proxies test`.
#[derive(Debug, Args)]
pub struct TestArgs {
    /// Group whose members should be tested
    pub group: String,

    /// Which arguments apply to a node test
    #[command(flatten)]
    pub node: NodeTestArgs,
}

/// The knobs every latency test shares.
#[derive(Debug, Args)]
pub struct NodeTestArgs {
    /// URL to fetch through each node
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,

    /// Timeout and concurrency, shared with every other latency command
    #[command(flatten)]
    pub limits: TestLimits,
}

/// The two flags every latency command takes.
///
/// One struct rather than two copies of two fields, because the ceilings they
/// are held to live in the settings and a ceiling that reaches three commands
/// out of four is the pattern this project has now fixed five times. A command
/// that takes these flags takes the checks with them.
#[derive(Debug, Args)]
pub struct TestLimits {
    /// Per-request timeout in milliseconds
    #[arg(long, value_name = "MS")]
    pub timeout: Option<u32>,

    /// How many are measured at once
    #[arg(long, value_name = "N")]
    pub concurrency: Option<usize>,
}

/// Connection operations.
#[derive(Debug, Subcommand)]
pub enum ConnectionsCommand {
    /// List live connections
    List {
        /// Show at most this many, most recent first
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },

    /// Close one connection, or every connection
    Close(CloseArgs),
}

/// Arguments of `connections close`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("target").required(true).args(["id", "all"])))]
pub struct CloseArgs {
    /// Connection id, as printed by `connections list`
    pub id: Option<String>,

    /// Close every connection
    #[arg(long)]
    pub all: bool,
}

/// Rule operations.
#[derive(Debug, Subcommand)]
pub enum RulesCommand {
    /// List the active rule set
    List {
        /// Only rules that are currently disabled
        #[arg(long)]
        disabled: bool,

        /// Include per-rule hit counts
        #[arg(long)]
        stats: bool,
    },

    /// Flip one rule's enabled state
    Toggle {
        /// Rule index, as printed by `rules list`
        index: u32,
    },

    /// List rule providers
    Providers,

    /// Refresh one provider, or every provider
    UpdateProviders {
        /// Provider name; omit to refresh all of them
        name: Option<String>,
    },
}

/// Connectivity tests.
#[derive(Debug, Subcommand)]
pub enum TestCommand {
    /// Measure latency for a group, or for every group
    Delay(DelayArgs),

    /// Test one node against every configured URL, or list them
    Urls(UrlsArgs),

    /// Resolve a name through the core's DNS
    Dns(DnsArgs),
}

/// Arguments of `test delay`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("scope").required(true).args(["group", "all"])))]
pub struct DelayArgs {
    /// One group to test
    #[arg(long, value_name = "G")]
    pub group: Option<String>,

    /// Every group
    #[arg(long)]
    pub all: bool,

    /// Which arguments apply to a node test
    #[command(flatten)]
    pub node: NodeTestArgs,
}

/// Arguments of `unlock`.
#[derive(Debug, Args)]
pub struct UnlockArgs {
    /// How long to wait per service, in milliseconds
    #[arg(long, value_name = "MS")]
    pub timeout: Option<u64>,
}

/// Arguments of `geo`.
#[derive(Debug, Args)]
pub struct GeoArgs {
    /// Ask without the proxy, for this machine's own address
    #[arg(long)]
    pub direct: bool,

    /// How long to wait for an answer, in milliseconds
    #[arg(long, value_name = "MS")]
    pub timeout: Option<u64>,
}

/// Arguments of `test urls`.
#[derive(Debug, Args)]
pub struct UrlsArgs {
    /// The node to test every URL through
    #[arg(long, value_name = "NODE")]
    pub node: Option<String>,

    /// Just list the configured URLs
    #[arg(long)]
    pub list: bool,

    /// Timeout and concurrency, shared with every other latency command
    #[command(flatten)]
    pub limits: TestLimits,
}

/// Arguments of `test dns`.
#[derive(Debug, Args)]
pub struct DnsArgs {
    /// Name to resolve
    pub name: String,

    /// Record type: A, AAAA, CNAME, MX, TXT, ...
    #[arg(long = "type", value_name = "TYPE", default_value = "A")]
    pub record_type: String,
}

/// Core process operations.
#[derive(Debug, Subcommand)]
pub enum CoreCommand {
    /// Report the process state and the binary that would run
    Status,

    /// Start the core with the deployed configuration
    Start,

    /// Stop the core
    Stop,

    /// Stop and start the core
    Restart,

    /// Print the binary version, and the running core's version when reachable
    Version,

    /// Ask the core to replace its own binary
    Upgrade(UpgradeArgs),

    /// Refresh the geo databases
    Geo,

    /// Ask the core to run a garbage collection
    Gc,
}

/// Arguments of `core upgrade`.
#[derive(Debug, Args)]
pub struct UpgradeArgs {
    /// Release channel to follow
    #[arg(long, value_enum, default_value_t = Channel::Release)]
    pub channel: Channel,

    /// Upgrade even when the core believes it is current
    #[arg(long)]
    pub force: bool,
}

/// Release channel of a self-upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Channel {
    /// Tagged releases.
    Release,
    /// The rolling alpha builds.
    Alpha,
}

impl Channel {
    /// The string the core's `/upgrade` endpoint expects.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Alpha => "alpha",
        }
    }
}

/// Arguments of `logs`.
#[derive(Debug, Args)]
pub struct LogsArgs {
    /// Show this level and anything more severe: silent, error, warning, info or debug
    #[arg(long, value_name = "LEVEL")]
    pub level: Option<String>,

    /// Only lines containing this text
    #[arg(long, value_name = "TEXT")]
    pub filter: Option<String>,

    /// How many lines to print when not following
    #[arg(long, value_name = "N", default_value_t = 200)]
    pub lines: usize,

    /// Keep printing as the core logs, until interrupted
    #[arg(long)]
    pub follow: bool,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap()
    }

    fn error(args: &[&str]) -> clap::Error {
        Cli::try_parse_from(args).unwrap_err()
    }

    #[test]
    fn no_subcommand_means_the_interface() {
        let cli = parse(&["cvt"]);
        assert!(cli.command.is_none());
        assert!(!cli.json);
        assert_eq!(cli.verbose, 0);
    }

    #[test]
    fn global_flags_are_accepted_before_and_after_the_subcommand() {
        let before = parse(&[
            "cvt",
            "--json",
            "--no-color",
            "-vv",
            "--home",
            "/tmp/h",
            "status",
        ]);
        assert!(before.json);
        assert!(before.no_color);
        assert_eq!(before.verbose, 2);
        assert_eq!(before.home.as_deref(), Some(std::path::Path::new("/tmp/h")));

        let after = parse(&["cvt", "status", "--json"]);
        assert!(after.json, "global flags must work on either side");
    }

    #[test]
    fn every_subcommand_parses() {
        let cases: &[&[&str]] = &[
            &["cvt", "status"],
            &["cvt", "doctor"],
            &["cvt", "profiles", "list"],
            &["cvt", "profiles", "add", "https://e/x"],
            &["cvt", "profiles", "add", "https://e/x", "--name", "A"],
            &["cvt", "profiles", "remove", "R1"],
            &["cvt", "profiles", "rename", "R1", "New"],
            &["cvt", "profiles", "switch", "R1"],
            &["cvt", "profiles", "update"],
            &["cvt", "profiles", "update", "R1"],
            &["cvt", "profiles", "update", "--all-due"],
            &["cvt", "profiles", "import", "/tmp/verge"],
            &["cvt", "profiles", "chain"],
            &["cvt", "profiles", "chain", "m1", "o1"],
            &["cvt", "profiles", "chain", "--clear"],
            &["cvt", "profiles", "show", "R1"],
            &["cvt", "config", "generate"],
            &["cvt", "config", "generate", "--apply"],
            &["cvt", "config", "generate", "--apply", "--force"],
            &["cvt", "config", "generate", "--apply", "--mode", "restart"],
            &["cvt", "config", "show"],
            &["cvt", "config", "validate"],
            &["cvt", "config", "diff"],
            &["cvt", "config", "rollback"],
            &["cvt", "config", "snapshots"],
            &["cvt", "config", "edit"],
            &["cvt", "config", "path"],
            &["cvt", "proxies", "list"],
            &["cvt", "proxies", "list", "PROXY"],
            &["cvt", "proxies", "select", "PROXY", "JP 01"],
            &["cvt", "proxies", "test", "PROXY"],
            &[
                "cvt",
                "proxies",
                "test",
                "PROXY",
                "--url",
                "https://g/generate_204",
            ],
            &[
                "cvt",
                "proxies",
                "test-all",
                "--concurrency",
                "8",
                "--timeout",
                "3000",
            ],
            &["cvt", "proxies", "unpin", "PROXY"],
            &["cvt", "connections", "list"],
            &["cvt", "connections", "list", "--limit", "20"],
            &["cvt", "connections", "close", "abc"],
            &["cvt", "connections", "close", "--all"],
            &["cvt", "rules", "list"],
            &["cvt", "rules", "list", "--disabled", "--stats"],
            &["cvt", "rules", "toggle", "3"],
            &["cvt", "rules", "providers"],
            &["cvt", "rules", "update-providers"],
            &["cvt", "rules", "update-providers", "reject"],
            &["cvt", "test", "delay", "--group", "PROXY"],
            &["cvt", "test", "delay", "--all"],
            &["cvt", "test", "dns", "example.com"],
            &["cvt", "test", "dns", "example.com", "--type", "AAAA"],
            &["cvt", "core", "status"],
            &["cvt", "core", "start"],
            &["cvt", "core", "stop"],
            &["cvt", "core", "restart"],
            &["cvt", "core", "version"],
            &["cvt", "core", "upgrade"],
            &["cvt", "core", "upgrade", "--channel", "alpha", "--force"],
            &["cvt", "core", "geo"],
            &["cvt", "core", "gc"],
            &["cvt", "logs"],
            &[
                "cvt", "logs", "--level", "warning", "--filter", "dns", "--lines", "10",
            ],
            &["cvt", "logs", "--follow"],
            &["cvt", "theme"],
        ];
        for case in cases {
            let cli = Cli::try_parse_from(*case)
                .unwrap_or_else(|e| panic!("{case:?} did not parse: {e}"));
            assert!(cli.command.is_some(), "{case:?} produced no command");
        }
    }

    #[test]
    fn the_documented_defaults_are_applied() {
        let Cli {
            command: Some(Command::Logs(args)),
            ..
        } = parse(&["cvt", "logs"])
        else {
            panic!("expected logs");
        };
        assert_eq!(args.lines, 200);
        assert!(!args.follow);
        assert!(args.level.is_none());

        let Cli {
            command: Some(Command::Config { command }),
            ..
        } = parse(&["cvt", "config", "generate"])
        else {
            panic!("expected config generate");
        };
        let ConfigCommand::Generate(args) = command else {
            panic!("expected generate");
        };
        assert!(!args.apply);
        assert!(!args.force);
        assert_eq!(args.mode, Mode::Auto);

        let Cli {
            command: Some(Command::Core { command }),
            ..
        } = parse(&["cvt", "core", "upgrade"])
        else {
            panic!("expected core upgrade");
        };
        let CoreCommand::Upgrade(args) = command else {
            panic!("expected upgrade");
        };
        assert_eq!(args.channel, Channel::Release);
        assert!(!args.force);
    }

    #[test]
    fn the_reload_mode_maps_onto_the_library_enum() {
        assert_eq!(ReloadMode::from(Mode::Auto), ReloadMode::Auto);
        assert_eq!(ReloadMode::from(Mode::Hot), ReloadMode::HotReload);
        assert_eq!(ReloadMode::from(Mode::Restart), ReloadMode::Restart);
        assert_eq!(Channel::Release.as_str(), "release");
        assert_eq!(Channel::Alpha.as_str(), "alpha");
    }

    #[test]
    fn a_force_without_apply_is_rejected_before_anything_runs() {
        let e = error(&["cvt", "config", "generate", "--force"]);
        assert_eq!(e.exit_code(), 2, "usage errors exit 2");
        let e = error(&["cvt", "config", "generate", "--mode", "hot"]);
        assert_eq!(e.exit_code(), 2);
    }

    #[test]
    fn closing_connections_needs_a_target() {
        let e = error(&["cvt", "connections", "close"]);
        assert_eq!(e.exit_code(), 2);
        let e = error(&["cvt", "connections", "close", "abc", "--all"]);
        assert_eq!(e.exit_code(), 2, "an id and --all together are ambiguous");
    }

    #[test]
    fn a_delay_test_needs_a_scope() {
        assert_eq!(error(&["cvt", "test", "delay"]).exit_code(), 2);
        assert_eq!(
            error(&["cvt", "test", "delay", "--group", "G", "--all"]).exit_code(),
            2
        );
    }

    #[test]
    fn conflicting_profile_updates_are_rejected() {
        assert_eq!(
            error(&["cvt", "profiles", "update", "R1", "--all-due"]).exit_code(),
            2
        );
        assert_eq!(
            error(&["cvt", "profiles", "chain", "m1", "--clear"]).exit_code(),
            2
        );
    }

    #[test]
    fn a_mistyped_mode_names_the_valid_ones() {
        let e = error(&["cvt", "config", "generate", "--apply", "--mode", "fast"]);
        assert_eq!(e.exit_code(), 2);
        let text = e.to_string();
        assert!(text.contains("auto") && text.contains("hot"), "{text}");
    }

    #[test]
    fn help_and_version_are_answerable() {
        let e = error(&["cvt", "--help"]);
        assert_eq!(e.kind(), clap::error::ErrorKind::DisplayHelp);
        let text = e.to_string();
        assert!(
            text.contains("Exit codes:"),
            "the contract belongs in --help"
        );
        assert!(text.contains("--json"));

        let e = error(&["cvt", "--version"]);
        assert_eq!(e.kind(), clap::error::ErrorKind::DisplayVersion);
        assert!(e.to_string().contains(env!("CARGO_PKG_VERSION")));
    }
}
