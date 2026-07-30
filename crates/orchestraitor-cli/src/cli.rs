//! Typed `orc` command-line arguments.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Orchestraitor command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "orc",
    author,
    version,
    about = "Orchestraitor local control-plane CLI",
    long_about = "Orchestraitor - an agent harness with trust issues.",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Parsed config paths.
    #[command(flatten)]
    pub paths: ConfigPaths,
    /// Command to execute.
    #[command(subcommand)]
    pub command: Commands,
}

/// Shared config path options.
#[derive(Debug, Clone, Args)]
pub struct ConfigPaths {
    /// Root used for non-project config layers.
    #[arg(
        long,
        env = "ORCHESTRAITOR_CONFIG_DIR",
        default_value = ".orchestraitor",
        global = true
    )]
    pub config_dir: PathBuf,
    /// Project directory containing `orchestraitor.toml`.
    #[arg(
        long,
        env = "ORCHESTRAITOR_PROJECT_DIR",
        default_value = ".",
        global = true
    )]
    pub project_dir: PathBuf,
    /// Alternate models.dev catalog endpoint for mirrors and tests.
    #[arg(
        long,
        env = "ORCHESTRAITOR_MODELS_DEV_ENDPOINT",
        hide = true,
        global = true
    )]
    pub models_dev_endpoint: Option<String>,
}

/// Top-level `orc` subcommands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Detect the local project and propose `.orchestraitor/orchestraitor.toml`.
    Init(InitArgs),
    /// Inspect, edit, validate, diff, and migrate configuration.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Manage the cached models.dev catalog.
    #[command(subcommand)]
    Models(ModelsCommand),
    /// Run project-configured verification commands (spec MVP-8, §9.5).
    Verify(VerifyArgs),
    /// Evaluate Arbitraitor policy against a plan or session (spec MVP-8).
    #[command(subcommand)]
    Policy(PolicyCommand),
    /// Execute a task, optionally without TUI interaction (spec MVP-8).
    Run(RunArgs),
    /// Export session evidence in a privacy-preserving archive (spec MVP-8).
    #[command(subcommand)]
    Evidence(EvidenceCommand),
}

/// Arguments for `orc init`.
#[derive(Debug, Clone, Args)]
pub struct InitArgs {
    /// Show the proposed configuration without writing any files.
    #[arg(long)]
    pub dry_run: bool,

    /// Project root to inspect.
    #[arg(long, default_value = ".")]
    pub project: PathBuf,
}

/// `orc config` subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print a resolved value.
    Get(KeyArgs),
    /// Print a resolved value and its provenance.
    Explain(KeyArgs),
    /// Set a key at the selected layer.
    Set(SetArgs),
    /// Remove a key from the selected layer.
    Unset(LayeredKeyArgs),
    /// Validate all known config layers.
    Validate,
    /// Show effective-vs-defaults or layer-specific differences.
    Diff(DiffArgs),
    /// Apply forward-only comment-preserving migrations.
    Migrate,
}

/// Key-only command arguments.
#[derive(Debug, Clone, Args)]
pub struct KeyArgs {
    /// Dotted config key.
    pub key: String,
}

/// Arguments for commands that target a layer.
#[derive(Debug, Clone, Args)]
pub struct LayeredKeyArgs {
    /// Dotted config key.
    pub key: String,
    /// Layer to mutate.
    #[arg(long, value_enum, default_value_t = CliLayer::Project)]
    pub layer: CliLayer,
}

/// `orc config set` arguments.
#[derive(Debug, Clone, Args)]
pub struct SetArgs {
    /// Dotted config key.
    pub key: String,
    /// TOML scalar/array/object literal, or a string when not valid TOML.
    pub value: String,
    /// Layer to mutate.
    #[arg(long, value_enum, default_value_t = CliLayer::Project)]
    pub layer: CliLayer,
}

/// `orc config diff` arguments.
#[derive(Debug, Clone, Args)]
pub struct DiffArgs {
    /// Optional layer whose contribution should be isolated.
    #[arg(long, value_enum)]
    pub layer: Option<CliLayer>,
    /// Emit stable JSON.
    #[arg(long)]
    pub json: bool,
}

/// Config layers exposed by the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliLayer {
    /// Project `orchestraitor.toml`.
    Project,
    /// User config layer.
    User,
    /// Organization/team config layer.
    Org,
    /// Directory/domain config layer.
    Dir,
}

/// `orc models` subcommands.
#[derive(Debug, Clone, Copy, Subcommand)]
pub enum ModelsCommand {
    /// Force a live models.dev catalog refresh.
    Refresh,
    /// Roll back to the previous cached models.dev catalog.
    Rollback,
}

/// Arguments for `orc verify` (spec MVP-8, §9.5).
#[derive(Debug, Clone, Args)]
pub struct VerifyArgs {
    /// Emit stable JSON for CI automation.
    #[arg(long)]
    pub json: bool,
    /// Suppress non-essential output.
    #[arg(long, short = 'q')]
    pub quiet: bool,
}

/// `orc policy` subcommands (spec MVP-8).
#[derive(Debug, Clone, Subcommand)]
pub enum PolicyCommand {
    /// Evaluate policy against a plan or recorded session.
    Check(PolicyCheckArgs),
}

/// Arguments for `orc policy check` (spec MVP-8).
#[derive(Debug, Clone, Args)]
pub struct PolicyCheckArgs {
    /// Path to an Arbitraitor policy TOML file.
    #[arg(long)]
    pub policy: Option<PathBuf>,
    /// Session id to evaluate against (for shadow evaluation).
    #[arg(long)]
    pub session: Option<String>,
    /// Evaluate in shadow mode — report what would have happened without enforcement.
    #[arg(long)]
    pub shadow: bool,
    /// Emit stable JSON for CI automation.
    #[arg(long)]
    pub json: bool,
    /// Suppress non-essential output.
    #[arg(long, short = 'q')]
    pub quiet: bool,
}

/// Non-interactive approval policy (spec MVP-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NonInteractiveApprovalMode {
    /// Block all operations requiring approval (default).
    Block,
    /// Allow operations that would prompt in interactive mode.
    Allow,
}

/// Arguments for `orc run` (spec MVP-8).
#[derive(Debug, Clone, Args)]
pub struct RunArgs {
    /// Task description or prompt to execute.
    pub task: Option<String>,
    /// Run without TUI interaction. Approvals follow the non-interactive policy.
    #[arg(long)]
    pub non_interactive: bool,
    /// Approval policy when running non-interactively (default: block).
    #[arg(long, value_enum, default_value_t = NonInteractiveApprovalMode::Block)]
    pub approval: NonInteractiveApprovalMode,
    /// Emit stable JSON for CI automation.
    #[arg(long)]
    pub json: bool,
    /// Suppress non-essential output.
    #[arg(long, short = 'q')]
    pub quiet: bool,
}

/// `orc evidence` subcommands (spec MVP-8).
#[derive(Debug, Clone, Subcommand)]
pub enum EvidenceCommand {
    /// Export session evidence in a privacy-preserving archive.
    Export(EvidenceExportArgs),
}

/// Arguments for `orc evidence export` (spec MVP-8).
#[derive(Debug, Clone, Args)]
pub struct EvidenceExportArgs {
    /// Session id whose evidence should be exported.
    #[arg(long)]
    pub session: Option<String>,
    /// Output file path. Defaults to stdout.
    #[arg(long, short = 'o')]
    pub output: Option<PathBuf>,
    /// Emit full (non-redacted) payloads. Default is redacted.
    #[arg(long)]
    pub full: bool,
    /// Emit stable JSON for CI automation.
    #[arg(long)]
    pub json: bool,
    /// Suppress non-essential output.
    #[arg(long, short = 'q')]
    pub quiet: bool,
}
