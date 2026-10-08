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
    /// Run the cron-shaped foreground bootstrap loop: poll the board, run
    /// one campaign pass, supervise the in-flight workers, repeat (issue
    /// #314; the §9.36 watch daemon deepens this in E8).
    Loop(LoopArgs),
    /// Run the rule-driven pre-landing simplify pass (fail-open quality
    /// tooling; spec §9.5 normalization classes).
    #[command(subcommand)]
    Simplify(SimplifyCommand),
}

/// Arguments for `orc loop`.
#[derive(Debug, Clone, Args)]
pub struct LoopArgs {
    /// Emit the end-of-run summary (counts, stop reason, event journal) as
    /// JSON.
    #[arg(long)]
    pub json: bool,
    /// Stop after this many board polls (cycles). A QA/evidence bound;
    /// without it the loop runs until a budget stop or shutdown signal.
    #[arg(long)]
    pub max_cycles: Option<u64>,
    /// Alternate fixture task directory for tests.
    #[arg(long, env = "ORCHESTRAITOR_WORKER_TASKS_DIR", hide = true)]
    pub worker_tasks_dir: Option<PathBuf>,
    /// Alternate provider base URL for simulator-backed tests.
    #[arg(long, env = "ORCHESTRAITOR_WORKER_PROVIDER_ENDPOINT", hide = true)]
    pub worker_provider_endpoint: Option<String>,
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
    /// key=value` pairs (always string values, like `gh api -f`; on GET/DELETE
    /// the pairs ride the URL query string). Prints the response body verbatim
    /// on stdout; exits 0 on 2xx, non-zero otherwise. The minted token is
    /// never printed.
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
    /// Land a local branch's tree as ONE App-signed squashed commit on the
    /// remote branch.
    ///
    /// Computes the branch's tree versus the remote head (or the base
    /// branch for a new branch), creates or force-moves the remote branch
    /// via the GitHub GraphQL API (`createCommitOnBranch` + ref update),
    /// and verifies tree equality and `verified = true` before reporting
    /// success — fail-closed with a typed error when the landed commit is
    /// not GitHub-signed. Never uses plain `git push`.
    PushBranch(PushBranchArgs),
    /// Verify the ambient git commit identity of a checkout against the
    /// App-derived canonical commit identity.
    ///
    /// The Rust twin of the `commit-identity` skill script: compares the
    /// ambient `git config user.name`/`user.email` in the target path with
    /// the service-identity author (`commit-author` derivation, never
    /// hardcoded) and exits with a typed error carrying the exact
    /// `git -c user.name=… -c user.email=…` fix on mismatch — so a fresh
    /// worktree inheriting a personal global gitconfig cannot stamp
    /// personal attribution onto agent commits.
    VerifyIdentity(VerifyIdentityArgs),
}

/// Arguments for `orc github api`.
#[derive(Debug, Clone, Args)]
pub struct ApiArgs {
    /// HTTP method: GET, POST, PATCH, PUT, or DELETE.
    pub method: String,
    /// API path relative to the configured base URL (e.g.
    /// `/repos/OWNER/REPO/pulls`).
    pub path: String,
    /// Read the JSON request body from a file (`-` reads stdin). Not valid
    /// with GET/DELETE (GitHub ignores bodies there).
    #[arg(long)]
    pub input: Option<String>,
    /// Add a request field `key=value` (repeatable). The value is ALWAYS a
    /// string, like `gh api -f/--raw-field` (`-f body=123` sends `"123"`,
    /// never the number `123`). On GET/DELETE the pairs become URL query
    /// parameters; otherwise they merge into an `--input` object body.
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

/// Arguments for `orc github push-branch`.
#[derive(Debug, Clone, Args)]
pub struct PushBranchArgs {
    /// Local branch whose tree is landed (its HEAD tree, not a commit chain;
    /// multiple local commits squash-land as ONE remote commit with this
    /// tree — the landing is a tree operation, never a history replay).
    /// Defaults to the current branch of the working directory.
    #[arg(long)]
    pub branch: Option<String>,
    /// Branch to create or update on the remote. Defaults to the local
    /// branch name.
    #[arg(long)]
    pub remote_branch: Option<String>,
    /// Base branch the local branch was cut from; its tree is the diff base
    /// for a NEW remote branch (default: the repo default branch read from
    /// the API). The diff base for an EXISTING remote branch is always that
    /// branch's head tree, so a second landing carries only the delta.
    #[arg(long)]
    pub base: Option<String>,
    /// Commit message headline (first line).
    #[arg(long)]
    pub message: String,
    /// Read the commit message body from a file (`-` = stdin).
    #[arg(long)]
    pub body_file: Option<String>,
    /// Owner/org of the target repository (e.g. `arbsec`). Defaults to the
    /// owner resolved from the `origin` remote of the current directory.
    #[arg(long)]
    pub owner: Option<String>,
    /// Repository name (e.g. `orchestraitor`). Defaults to the repo name
    /// resolved from the `origin` remote of the current directory.
    #[arg(long)]
    pub repo: Option<String>,
    /// Re-land even when the diff against the remote head is EMPTY: land an
    /// empty App-signed commit whose parent is the remote head. This is the
    /// signed fix for a rebased PR branch whose local chain is unsigned:
    /// the tree is already correct, but the PR head commit carries an
    /// unsigned plain-push signature. LIMITATION: one signed commit on the
    /// tip verifies the head commit GitHub's `required_signatures` push rule
    /// evaluates; it does NOT rewrite unsigned ancestors into verified
    /// objects — a ruleset that range-checks every commit can still block
    /// the merge (range repair needs the manual replay in
    /// `references/verified-commit-path.md`). Refuse when the local tree
    /// does NOT match the remote tree: a re-land with a different tree is
    /// an ordinary landing, not a re-land.
    #[arg(long)]
    pub re_land: bool,
    /// Preview the landing without touching the remote: print the resolved
    /// owner/repo, branches, diff base, and the file-changes payload shape
    /// (paths and statuses only — never blob contents), then exit 0.
    #[arg(long)]
    pub dry_run: bool,
}

/// Arguments for `orc github verify-identity`.
#[derive(Debug, Clone, Args)]
pub struct VerifyIdentityArgs {
    /// Worktree or repository whose ambient git commit identity is checked
    /// (default: the current directory).
    pub path: Option<String>,
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
    /// Run the `board.move` decision tool against the in-memory fixture
    /// board (spec `10-orchestrator.md` §9.39; issue #333): a guarded
    /// status transition — workflow-policy validated, lease-checked,
    /// reconcile-visible; refusals are typed and leave the board
    /// unchanged. The sqlite/GitHub provider wiring is the #318 split.
    GuardedMove(BoardGuardedMoveArgs),
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

/// Arguments for `orc board guarded-move` (the `board.move` decision tool).
#[derive(Debug, Clone, Args)]
pub struct BoardGuardedMoveArgs {
    /// Board item id to move (fixture board ids in this slice).
    pub item: String,
    /// Target status name (exact, case-sensitive, e.g. "In Progress").
    #[arg(long)]
    pub status: String,
    /// The invoking session label (leases and attribution key on it).
    #[arg(long)]
    pub session: String,
    /// Emit the typed outcome as stable JSON.
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

/// `orc simplify` subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum SimplifyCommand {
    /// Run the rule-driven pre-landing simplify pass (fail-open: exits 0
    /// unless `--pedantic-check` finds unaddressed suggestions).
    Run(SimplifyRunArgs),
}

/// Fix policy for `orc simplify run --fix`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SimplifyFixMode {
    /// Run checks only; never modify files.
    None,
    /// Auto-apply Format-class fixes (rustfmt, rumdl) when
    /// `simplify.auto_apply_format` allows.
    Format,
    /// Additionally auto-apply clippy machine-applicable suggestions on
    /// `.rs` files when `simplify.auto_apply_safe_fixes` allows.
    Safe,
}

/// Arguments for `orc simplify run`.
#[derive(Debug, Clone, Args)]
pub struct SimplifyRunArgs {
    /// Scope the report to files staged for commit (hooks pass this).
    #[arg(long)]
    pub staged: bool,
    /// Restrict the report to these paths (repeatable).
    #[arg(long = "path", value_name = "PATH")]
    pub paths: Vec<String>,
    /// Fix policy for the pass.
    #[arg(long, value_enum, default_value_t = SimplifyFixMode::None)]
    pub fix: SimplifyFixMode,
    /// Pedantic-check mode: exit 1 when unaddressed suggestions exist
    /// (fast-feedback for the pre-push hook; never a push gate).
    #[arg(long)]
    pub pedantic_check: bool,
    /// Emit the typed report as stable JSON.
    #[arg(long)]
    pub json: bool,
}
