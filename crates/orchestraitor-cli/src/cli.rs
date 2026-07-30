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
    /// Observe a harness without enforcement (spec §998 MVP-2).
    Observe(ObserveArgs),
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

/// Arguments for `orc observe` (spec §998 MVP-2).
///
/// Records a normalized event stream for the target harness without claiming
/// enforcement. The output always identifies as non-protective.
#[derive(Debug, Clone, Args)]
pub struct ObserveArgs {
    /// Harness command and arguments (after `--`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
    pub harness: Vec<String>,
    /// Output directory for the recorded event stream.
    #[arg(long, default_value = ".orchestraitor/observe")]
    pub output: PathBuf,
    /// Emit machine-readable JSON to stdout.
    #[arg(long)]
    pub json: bool,
}
