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
    /// Alternate GitHub API base URL for token-mint tests.
    #[arg(
        long,
        env = "ORCHESTRAITOR_GITHUB_API_ENDPOINT",
        hide = true,
        global = true
    )]
    pub github_api_endpoint: Option<String>,
    /// Alternate GitHub GraphQL endpoint for GHES and tests.
    #[arg(
        long,
        env = "ORCHESTRAITOR_GITHUB_GRAPHQL_ENDPOINT",
        hide = true,
        global = true
    )]
    pub github_graphql_endpoint: Option<String>,
    /// Alternate board node-id cache path for tests.
    #[arg(
        long,
        env = "ORCHESTRAITOR_BOARD_CACHE_PATH",
        hide = true,
        global = true
    )]
    pub board_cache_path: Option<PathBuf>,
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
    /// GitHub App service-identity operations.
    #[command(subcommand, name = "github")]
    GitHub(GitHubCommand),
    /// Resolve role model routing and inspect decision records.
    #[command(subcommand)]
    Routing(RoutingCommand),
    /// Read and update the shared GitHub Projects v2 board.
    #[command(subcommand)]
    Board(BoardCommand),
    /// Run the headless one-shot bootstrap worker.
    #[command(subcommand)]
    Worker(WorkerCommand),
    /// Run one campaign pass: select at most one task and record the decision.
    #[command(subcommand)]
    Campaign(CampaignCommand),
}

/// `orc campaign` subcommands.
#[derive(Debug, Subcommand)]
pub enum CampaignCommand {
    /// Run exactly one campaign pass and exit (the loop runner lands with the
    /// bootstrap-loop task; only `--once` exists in this slice).
    Run(CampaignRunArgs),
}

/// Arguments for `orc campaign run`.
#[derive(Debug, Clone, Args)]
pub struct CampaignRunArgs {
    /// One-shot pass (the only mode in this slice; the cron-shaped loop
    /// runner arrives with the bootstrap loop).
    #[arg(long)]
    pub once: bool,
    /// Emit the decision record (and worker result, when one ran) as JSON.
    #[arg(long)]
    pub json: bool,
    /// Alternate fixture task directory for tests.
    #[arg(long, env = "ORCHESTRAITOR_WORKER_TASKS_DIR", hide = true)]
    pub worker_tasks_dir: Option<PathBuf>,
    /// Alternate provider base URL for simulator-backed tests.
    #[arg(long, env = "ORCHESTRAITOR_WORKER_PROVIDER_ENDPOINT", hide = true)]
    pub worker_provider_endpoint: Option<String>,
}

/// `orc github` subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum GitHubCommand {
    /// Mint one installation access token and print only non-secret metadata
    /// (expiry, installation id, SHA-256 fingerprint prefix — never the token).
    MintToken,
    /// Execute one authenticated GitHub REST API call as the App installation.
    ///
    /// Mirrors `gh api` minimally: METHOD plus a repository-relative API path,
    /// an optional JSON body from `--input FILE` (`-` = stdin) or `--field
    /// key=value` pairs. Prints the response body verbatim on stdout; exits 0
    /// on 2xx, non-zero otherwise. The minted token is never printed.
    Api(ApiArgs),
    /// Run one child command with `GH_TOKEN` set to a freshly minted
    /// installation token (never printed by orc; the child's environment is
    /// the child's responsibility). Child stdout/stderr pass through and the
    /// child's exit code propagates.
    GhEnv(GhEnvArgs),
    /// Print the App's canonical commit identity as `name=…` / `email=…`
    /// lines, derived from the authenticated App (`GET /app`) — never
    /// hardcoded.
    CommitAuthor,
}

/// Arguments for `orc github api`.
#[derive(Debug, Clone, Args)]
pub struct ApiArgs {
    /// HTTP method: GET, POST, PATCH, PUT, or DELETE.
    pub method: String,
    /// API path relative to the configured base URL (e.g.
    /// `/repos/OWNER/REPO/pulls`).
    pub path: String,
    /// Read the JSON request body from a file (`-` reads stdin).
    #[arg(long)]
    pub input: Option<String>,
    /// Add a JSON body field `key=value` (repeatable; values are raw JSON
    /// when they parse as such, else strings — like `gh api -f`).
    #[arg(long = "field", short = 'f')]
    pub fields: Vec<String>,
}

/// Arguments for `orc github gh-env`.
#[derive(Debug, Clone, Args)]
pub struct GhEnvArgs {
    /// Child command and arguments, after `--`.
    #[arg(last = true)]
    pub command: Vec<String>,
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

/// `orc routing` subcommands.
#[derive(Debug, Subcommand)]
pub enum RoutingCommand {
    /// Resolve one built-in role to its `(provider, model)` route and persist
    /// the decision record.
    Resolve(ResolveArgs),
}

/// Arguments for `orc routing resolve`.
#[derive(Debug, Clone, Args)]
pub struct ResolveArgs {
    /// Built-in orchestration role id (explore, research, plan, implement,
    /// review, verify).
    #[arg(long)]
    pub role: String,
    /// Emit stable JSON.
    #[arg(long)]
    pub json: bool,
}

/// `orc board` subcommands.
#[derive(Debug, Subcommand)]
pub enum BoardCommand {
    /// List MVP-ready leaf board items (Target=MVP, Status=Ready, unblocked).
    Ready(BoardReadyArgs),
    /// Move a board item to a new Status value, verified by read-back.
    Move(BoardMoveArgs),
    /// Run the `board.query` decision tool against the in-memory fixture
    /// board (spec `10-orchestrator.md` §9.39; issue #332): typed search or
    /// the transitive blocked graph. The sqlite/GitHub provider wiring is a
    /// separate follow-up (#318 split), so this slice reads the deterministic
    /// fixture board.
    Query(BoardQueryArgs),
}

/// Arguments for `orc board ready`.
#[derive(Debug, Clone, Copy, Args)]
pub struct BoardReadyArgs {
    /// Emit stable JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `orc board move`.
#[derive(Debug, Clone, Args)]
pub struct BoardMoveArgs {
    /// Issue number on a configured board repository.
    pub item: u64,
    /// Target Status option name, e.g. "In Progress".
    #[arg(long)]
    pub status: String,
}

/// Arguments for `orc board query`.
#[derive(Debug, Clone, Args)]
pub struct BoardQueryArgs {
    /// Blocked-graph mode: return the transitive blocked set for this item
    /// id instead of a filter search.
    #[arg(long)]
    pub blocked_by: Option<String>,
    /// Restrict to one item type (`task`, `bug`, `epic`, `feature`).
    #[arg(long)]
    pub item_type: Option<String>,
    /// Restrict to one status name (exact match).
    #[arg(long)]
    pub status: Option<String>,
    /// Required field value as `name=option` (single-select; repeatable).
    #[arg(long = "field", value_name = "NAME=VALUE")]
    pub fields: Vec<String>,
    /// Emit the typed result as stable JSON.
    #[arg(long)]
    pub json: bool,
}

/// `orc worker` subcommands.
#[derive(Debug, Subcommand)]
pub enum WorkerCommand {
    /// Run one leaf task through the headless bootstrap worker loop.
    Run(WorkerRunArgs),
}

/// Arguments for `orc worker run`.
#[derive(Debug, Clone, Args)]
pub struct WorkerRunArgs {
    /// Task id resolved through the fixture task source
    /// (`<config-dir>/worker-tasks/<id>.json`).
    #[arg(long)]
    pub task: String,
    /// Emit the structured worker result as stable JSON.
    #[arg(long)]
    pub json: bool,
    /// Alternate fixture task directory for tests.
    #[arg(long, env = "ORCHESTRAITOR_WORKER_TASKS_DIR", hide = true)]
    pub worker_tasks_dir: Option<PathBuf>,
    /// Alternate provider base URL for simulator-backed tests.
    #[arg(long, env = "ORCHESTRAITOR_WORKER_PROVIDER_ENDPOINT", hide = true)]
    pub worker_provider_endpoint: Option<String>,
}
