//! Core library for Astrarium, a test orchestrator for Kubernetes GitOps.
//!
//! The library is split into domain modules rather than technical dumping
//! grounds. The binary crate owns the CLI, terminal diagnostics and process
//! setup; this crate owns manifests, selection, rendering, the Kubernetes and
//! Argo CD clients, cluster lifecycle, telemetry and diagnostics.
//!
//! Module files follow the `foo.rs` + `foo/` layout. A `foo/mod.rs` file is
//! never used in this workspace.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::future::Future;
use std::pin::Pin;

pub mod argocd;
pub mod clock;
pub mod cluster;
pub mod command;
pub mod diagnostics;
pub mod kube;
pub mod manifest;
pub mod plan;
pub mod probe;
pub mod render;
pub mod run;
pub mod telemetry;
pub mod types;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use argocd::{AppProject, AppProjectSpec, Application, ApplicationSpec};
pub use clock::{Clock, SystemClock};
pub use cluster::{ClusterError, ClusterHandle, ClusterOptions, ClusterProvider};
pub use command::{
    CommandError, CommandOutput, CommandRunner, CommandSpec, RealCommandRunner, Tool,
};
pub use manifest::{AppSpec, Manifest, ManifestError, NamespaceSpec, ProjectSpec, RendererKind};
pub use plan::{
    Plan, PlannedApp, PreviousResults, PreviousRunStatus, SelectionReason, SelectionRequest,
};
pub use render::{ChartRenderer, RenderError, RenderRequest, RenderedApp};
pub use types::{
    AppName, ChartVersion, ContentHash, HumanDuration, NamespaceName, ProjectName, Revision,
    ValueError,
};

/// A boxed, sendable future used by object-safe async traits.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
