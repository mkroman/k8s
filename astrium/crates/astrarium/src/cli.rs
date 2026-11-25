//! Command-line interface definitions.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

/// Clockwork test orchestration for Kubernetes GitOps.
#[derive(Debug, Parser)]
#[command(
    name = "astrarium",
    version,
    about,
    long_about = None,
    propagate_version = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// The suite manifest.
    #[arg(
        long,
        global = true,
        env = "ASTRARIUM_CONFIG",
        default_value = "astrarium.toml",
        value_name = "PATH"
    )]
    pub config: PathBuf,

    /// Log output format.
    #[arg(
        long,
        global = true,
        env = "ASTRARIUM_LOG_FORMAT",
        value_enum,
        default_value_t = LogFormat::Text
    )]
    pub log_format: LogFormat,

    /// Disable OpenTelemetry even when the manifest enables it.
    #[arg(long, global = true, env = "ASTRARIUM_NO_TELEMETRY")]
    pub no_telemetry: bool,

    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Log output format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum LogFormat {
    /// Human-readable logs on stderr.
    #[default]
    Text,
    /// Newline-delimited JSON logs on stderr.
    Json,
}

/// Top-level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Select the apps affected by a change.
    Plan(PlanArgs),
    /// Deploy one or more apps into a test cluster.
    Run(RunArgs),
    /// Deploy an app, then upgrade it in place.
    Upgrade(UpgradeArgs),
    /// Render an app and write the packaged chart.
    Render(RenderArgs),
    /// Manage test clusters.
    Cluster(ClusterArgs),
    /// Check tools, configuration and connectivity.
    Doctor(DoctorArgs),
    /// Work with the suite manifest.
    Manifest(ManifestArgs),
    /// Print the Astrarium version.
    Version,
    /// Generate shell completions.
    Completions {
        /// The target shell.
        shell: Shell,
    },
}

/// Arguments for `astrarium plan`.
#[derive(Debug, Args)]
pub struct PlanArgs {
    /// The diff base revision. Defaults to the previous-run head when it is an
    /// ancestor, then `project.base_branch`, then `HEAD^`.
    #[arg(long, value_name = "REV")]
    pub base: Option<String>,
    /// A previous-run JSON file from CI. Apps that failed, were cancelled or
    /// were skipped are selected again.
    #[arg(long, value_name = "JSON")]
    pub previous_results: Option<PathBuf>,
    /// Select a specific app. Repeat for several apps.
    #[arg(long = "app", value_name = "NAME")]
    pub apps: Vec<String>,
    /// Select every app.
    #[arg(long)]
    pub all: bool,
    /// Emit the machine-readable plan and matrix instead of the human summary.
    #[arg(long)]
    pub json: bool,
    /// Write `matrix` and `has_apps` to this file. Defaults to `$GITHUB_OUTPUT`
    /// when that environment variable is set.
    #[arg(long, value_name = "PATH")]
    pub github_output: Option<PathBuf>,
    /// Append a Markdown summary to this file. Defaults to
    /// `$GITHUB_STEP_SUMMARY` when that environment variable is set.
    #[arg(long, value_name = "PATH")]
    pub step_summary: Option<PathBuf>,
}

/// Arguments for `astrarium run`.
#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
pub struct RunArgs {
    /// The app to deploy. Repeat for several apps.
    #[arg(long = "app", value_name = "NAME")]
    pub apps: Vec<String>,
    /// Deploy every app in the suite.
    #[arg(long)]
    pub all: bool,
    /// Keep the cluster after a successful run.
    #[arg(long)]
    pub keep_cluster: bool,
    /// Reuse an existing cluster instead of creating one.
    #[arg(long)]
    pub reuse_cluster: bool,
    /// Fail as soon as a container is in a fatal crash loop.
    #[arg(long)]
    pub fast_fail: bool,
}

/// Arguments for `astrarium upgrade`.
#[derive(Debug, Args)]
pub struct UpgradeArgs {
    /// The app to deploy and upgrade.
    #[arg(long = "app", value_name = "NAME")]
    pub app: String,
    /// Keep the cluster after a successful run.
    #[arg(long)]
    pub keep_cluster: bool,
}

/// Arguments for `astrarium render`.
#[derive(Debug, Args)]
pub struct RenderArgs {
    /// The app to render.
    #[arg(long = "app", value_name = "NAME")]
    pub app: String,
    /// The output directory. Defaults to a temporary directory.
    #[arg(long, value_name = "DIR")]
    pub output: Option<PathBuf>,
}

/// Arguments for `astrarium cluster`.
#[derive(Debug, Args)]
pub struct ClusterArgs {
    /// The cluster command.
    #[command(subcommand)]
    pub command: ClusterCommand,
}

/// Cluster subcommands.
#[derive(Debug, Subcommand)]
pub enum ClusterCommand {
    /// Create a test cluster.
    Create {
        /// Override the cluster name.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Override the kubeconfig path.
        ///
        /// Defaults to the first `KUBECONFIG` entry, then to
        /// `cluster.kubeconfig` from the manifest.
        #[arg(long, value_name = "PATH")]
        kubeconfig: Option<PathBuf>,
        /// Reuse an existing cluster instead of creating one.
        #[arg(long)]
        reuse: bool,
        /// Override the kind node image.
        #[arg(long, value_name = "IMAGE")]
        node_image: Option<String>,
    },
    /// Delete a test cluster.
    Delete {
        /// The cluster name.
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// List test clusters.
    List,
}

/// Arguments for `astrarium doctor`.
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
    /// Treat a missing manifest as an error.
    #[arg(long)]
    pub require_config: bool,
    /// Skip the optional Jaeger and registry mirror reachability checks.
    #[arg(long)]
    pub skip_services: bool,
}

/// Arguments for `astrarium manifest`.
#[derive(Debug, Args)]
pub struct ManifestArgs {
    /// The manifest subcommand.
    #[command(subcommand)]
    pub command: ManifestCommand,
}

/// Manifest subcommands.
#[derive(Debug, Subcommand)]
pub enum ManifestCommand {
    /// Parse and validate the manifest and its referenced paths.
    Validate,
    /// Print the JSON schema for `astrarium.toml`.
    Schema,
}
