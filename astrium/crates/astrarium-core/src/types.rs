//! Small semantic types shared across the Astrarium modules.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// An error produced while constructing a semantic name or value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid {kind} {value:?}: {reason}")]
pub struct ValueError {
    /// The kind of value that was rejected, for example `app`.
    pub kind: &'static str,
    /// The rejected value.
    pub value: String,
    /// A short explanation of the validation rule that failed.
    pub reason: &'static str,
}

impl ValueError {
    fn new(kind: &'static str, value: &str, reason: &'static str) -> Self {
        Self {
            kind,
            value: value.to_owned(),
            reason,
        }
    }
}

macro_rules! dns_label {
    ($name:ident, $kind:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a validated name.
            ///
            /// # Errors
            ///
            /// Returns [`ValueError`] when the value is not a DNS-1123 label.
            pub fn new(value: impl Into<String>) -> Result<Self, ValueError> {
                let value = value.into();
                if value.is_empty() {
                    return Err(ValueError::new($kind, &value, "must not be empty"));
                }
                if value.len() > 63 {
                    return Err(ValueError::new($kind, &value, "must be at most 63 bytes"));
                }
                if !value.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                }) {
                    return Err(ValueError::new(
                        $kind,
                        &value,
                        "must contain only lowercase ASCII letters, digits and '-'",
                    ));
                }
                if value.starts_with('-') || value.ends_with('-') {
                    return Err(ValueError::new(
                        $kind,
                        &value,
                        "must start and end with a letter or digit",
                    ));
                }
                Ok(Self(value))
            }

            /// Returns the name as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = ValueError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "pattern": "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$",
                    "minLength": 1,
                    "maxLength": 63,
                })
            }
        }
    };
}

dns_label!(
    AppName,
    "app name",
    "The name of an Argo CD Application and its Astrarium app entry."
);
dns_label!(
    NamespaceName,
    "namespace name",
    "A Kubernetes namespace name."
);
dns_label!(ProjectName, "project name", "An Argo CD AppProject name.");

/// A Git revision: a commit ID, branch, tag or other revision expression.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(String);

impl Revision {
    /// Creates a revision.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when the revision is empty or contains whitespace
    /// or control characters.
    pub fn new(value: impl Into<String>) -> Result<Self, ValueError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ValueError::new("revision", &value, "must not be empty"));
        }
        if value.len() > 1024 {
            return Err(ValueError::new(
                "revision",
                &value,
                "must be at most 1024 bytes",
            ));
        }
        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(ValueError::new(
                "revision",
                &value,
                "must not contain whitespace or control characters",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the revision as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for Revision {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl JsonSchema for Revision {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Revision".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "string", "minLength": 1, "maxLength": 1024})
    }
}

/// A content hash, for example a SHA-256 digest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    /// Creates a content hash.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when the hash is empty or not hexadecimal.
    pub fn new(value: impl Into<String>) -> Result<Self, ValueError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ValueError::new("content hash", &value, "must not be empty"));
        }
        if value.len() > 128 {
            return Err(ValueError::new(
                "content hash",
                &value,
                "must be at most 128 bytes",
            ));
        }
        if !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ValueError::new(
                "content hash",
                &value,
                "must contain only hexadecimal digits",
            ));
        }
        Ok(Self(value))
    }

    /// Returns the hash as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A semantic Helm chart version.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChartVersion(Version);

impl ChartVersion {
    /// Creates a chart version from a semantic version.
    #[must_use]
    pub fn new(version: Version) -> Self {
        Self(version)
    }

    /// Returns the semantic version.
    #[must_use]
    pub fn version(&self) -> &Version {
        &self.0
    }
}

impl fmt::Display for ChartVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for ChartVersion {
    type Err = semver::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Version::parse(value).map(Self)
    }
}

impl JsonSchema for ChartVersion {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ChartVersion".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({"type": "string", "format": "semver"})
    }
}

/// A human-readable duration such as `10m`, `30s` or `1h30m`.
///
/// The manifest uses this type for timeouts. It serializes to a compact string
/// and rejects ambiguous or negative values at parse time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HumanDuration(Duration);

impl HumanDuration {
    /// Creates a duration from [`Duration`].
    #[must_use]
    pub const fn new(duration: Duration) -> Self {
        Self(duration)
    }

    /// Returns the wrapped [`Duration`].
    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }

    /// Returns the duration in whole seconds.
    #[must_use]
    pub const fn as_secs(self) -> u64 {
        self.0.as_secs()
    }
}

impl From<Duration> for HumanDuration {
    fn from(duration: Duration) -> Self {
        Self(duration)
    }
}

impl From<HumanDuration> for Duration {
    fn from(duration: HumanDuration) -> Self {
        duration.0
    }
}

impl fmt::Display for HumanDuration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let seconds = self.0.as_secs();
        let nanos = self.0.subsec_nanos();

        if nanos != 0 {
            return write!(formatter, "{seconds}.{nanos:09}s");
        }
        if seconds == 0 {
            return formatter.write_str("0s");
        }
        if seconds.is_multiple_of(86_400) {
            return write!(formatter, "{}d", seconds / 86_400);
        }
        if seconds.is_multiple_of(3600) {
            return write!(formatter, "{}h", seconds / 3600);
        }
        if seconds.is_multiple_of(60) {
            return write!(formatter, "{}m", seconds / 60);
        }
        write!(formatter, "{seconds}s")
    }
}

impl FromStr for HumanDuration {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err(ValueError::new("duration", value, "must not be empty"));
        }

        let mut total = Duration::ZERO;
        let mut index = 0;
        let bytes = value.as_bytes();
        let mut parsed_any = false;

        while index < bytes.len() {
            let number_start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if number_start == index {
                return Err(ValueError::new(
                    "duration",
                    value,
                    "expected a number followed by ms, s, m, h or d",
                ));
            }
            let number: u64 = value[number_start..index]
                .parse()
                .map_err(|_| ValueError::new("duration", value, "number is out of range"))?;

            let unit_start = index;
            while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
                index += 1;
            }
            let unit = &value[unit_start..index];
            let multiplier = match unit {
                "ms" => {
                    total = total
                        .checked_add(Duration::from_millis(number))
                        .ok_or_else(|| {
                            ValueError::new("duration", value, "duration is out of range")
                        })?;
                    parsed_any = true;
                    continue;
                }
                "s" => 1,
                "m" => 60,
                "h" => 60 * 60,
                "d" => 24 * 60 * 60,
                _ => {
                    return Err(ValueError::new(
                        "duration",
                        value,
                        "unknown unit; use ms, s, m, h or d",
                    ));
                }
            };

            let seconds = number
                .checked_mul(multiplier)
                .ok_or_else(|| ValueError::new("duration", value, "duration is out of range"))?;
            total = total
                .checked_add(Duration::from_secs(seconds))
                .ok_or_else(|| ValueError::new("duration", value, "duration is out of range"))?;
            parsed_any = true;
        }

        if !parsed_any {
            return Err(ValueError::new("duration", value, "must not be empty"));
        }
        Ok(Self(total))
    }
}

impl Serialize for HumanDuration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for HumanDuration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for HumanDuration {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "HumanDuration".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^([0-9]+(ms|s|m|h|d))+$",
            "examples": ["30s", "10m", "1h30m"],
        })
    }
}

/// A path in the repository-relative manifest, resolved through the project root.
pub type ManifestPath = PathBuf;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_names_accept_expected_values() {
        assert_eq!(AppName::new("mosquitto").unwrap().as_str(), "mosquitto");
        assert_eq!(
            NamespaceName::new("mqtt2prometheus-mqtt-topic")
                .unwrap()
                .as_str(),
            "mqtt2prometheus-mqtt-topic"
        );
        assert!(ProjectName::new("node-feature-discovery").is_ok());
    }

    #[test]
    fn dns_names_reject_invalid_values() {
        assert!(AppName::new("").is_err());
        assert!(AppName::new("Upper").is_err());
        assert!(AppName::new("-leading").is_err());
        assert!(AppName::new("trailing-").is_err());
        assert!(AppName::new("under_score").is_err());
        assert!(AppName::new("a".repeat(64)).is_err());
    }

    #[test]
    fn durations_round_trip_common_forms() {
        for (input, expected_seconds) in [("30s", 30), ("10m", 600), ("1h", 3600), ("2d", 172_800)]
        {
            let duration: HumanDuration = input.parse().unwrap();
            assert_eq!(duration.as_secs(), expected_seconds);
            assert_eq!(duration.to_string(), input);
        }
        let combined: HumanDuration = "1h30m".parse().unwrap();
        assert_eq!(combined.as_secs(), 5400);
        assert_eq!(combined.to_string(), "90m");
    }

    #[test]
    fn durations_reject_invalid_forms() {
        for input in ["", "10", "10x", "-1s", "1.5h", "s"] {
            assert!(input.parse::<HumanDuration>().is_err(), "{input}");
        }
    }
}
