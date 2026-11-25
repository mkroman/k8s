//! Reusable test fakes for Astrarium.
//!
//! The fakes implement [`astrarium_core::command::CommandRunner`] and
//! [`astrarium_core::clock::Clock`], so they can be passed to the production
//! orchestrator without adapters. They live in `astrarium_core` behind the
//! `testing` feature because a separate crate cannot implement the core traits
//! for the same `astrarium_core` instance that the crate's own unit tests use.
//! This crate enables that feature and re-exports them as the stable,
//! dependency-light entry point for other test crates.
//!
//! ```
//! use astrarium_test_support::{FakeClock, FakeCommandRunner};
//!
//! let runner = FakeCommandRunner::new();
//! runner.push_output_for("template", "apiVersion: v1\nkind: ConfigMap\n");
//! let clock = FakeClock::default();
//! assert!(clock.sleeps().is_empty());
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub use astrarium_core::testing::{FakeClock, FakeCommandRunner, transient_error};
