//! The identity of one nextest test: binary id plus test name.
//!
//! Every artifact (exports, skipped list, candidates, verdicts) names a test by
//! this type. On disk it is the string `"<binary_id> <name>"`. Nextest binary
//! ids never contain spaces, so the first space splits the two parts.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TestId {
    /// `crate` (lib unit tests), `crate::bin/name` (bin unit tests), or
    /// `crate::target` (integration test targets).
    pub binary_id: String,
    /// The test path inside the binary, e.g. `tests::parses_a_number`.
    pub name: String,
}

/// Whether a test is a unit test (in a lib or bin target) or an integration test.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Unit,
    Integration,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Unit => "unit",
            Tier::Integration => "integration",
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TestId {
    pub fn new(binary_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            binary_id: binary_id.into(),
            name: name.into(),
        }
    }

    pub fn tier(&self) -> Tier {
        if !self.binary_id.contains("::") || self.binary_id.contains("::bin/") {
            Tier::Unit
        } else {
            Tier::Integration
        }
    }

    /// The last two `::` segments of the test name, for compact labels.
    pub fn short_name(&self) -> String {
        let parts: Vec<&str> = self.name.rsplitn(3, "::").collect();
        match parts.as_slice() {
            [last, prev, ..] => format!("{prev}::{last}"),
            _ => self.name.clone(),
        }
    }
}

impl fmt::Display for TestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.binary_id, self.name)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("test id {0:?} is not \"<binary_id> <name>\"")]
pub struct ParseTestIdError(String);

impl FromStr for TestId {
    type Err = ParseTestIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once(' ') {
            Some((binary_id, name)) if !binary_id.is_empty() && !name.is_empty() => {
                Ok(TestId::new(binary_id, name))
            }
            _ => Err(ParseTestIdError(s.to_owned())),
        }
    }
}

impl Serialize for TestId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TestId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_follows_nextest_binary_id_shape() {
        assert_eq!(TestId::new("crate", "t").tier(), Tier::Unit);
        assert_eq!(TestId::new("crate::bin/describe", "t").tier(), Tier::Unit);
        assert_eq!(TestId::new("crate::cli", "t").tier(), Tier::Integration);
    }

    #[test]
    fn names_with_spaces_survive_the_round_trip() {
        let id: TestId = "crate::it case one two".parse().unwrap();
        assert_eq!(id, TestId::new("crate::it", "case one two"));
        assert!("nospace".parse::<TestId>().is_err());
    }
}
