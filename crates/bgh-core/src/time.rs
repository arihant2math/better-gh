//! GitHub timestamp format: ISO-8601 UTC, second precision, `Z` suffix.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A UTC timestamp that serializes as `2024-01-01T00:00:00Z`.
///
/// Use it for every timestamp field in API models:
/// `pub created_at: Timestamp` / `pub closed_at: Option<Timestamp>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub DateTime<Utc>);

impl Timestamp {
    pub fn now() -> Self {
        Self(Utc::now())
    }

    pub fn to_github_string(&self) -> String {
        self.0.to_rfc3339_opts(SecondsFormat::Secs, true)
    }
}

impl From<DateTime<Utc>> for Timestamp {
    fn from(d: DateTime<Utc>) -> Self {
        Self(d)
    }
}

impl From<Timestamp> for DateTime<Utc> {
    fn from(t: Timestamp) -> Self {
        t.0
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_github_string())
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_github_string())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        DateTime::parse_from_rfc3339(&s)
            .map(|dt| Self(dt.with_timezone(&Utc)))
            .map_err(serde::de::Error::custom)
    }
}

/// Convert an optional chrono timestamp (as read from the DB).
pub fn ts(d: Option<DateTime<Utc>>) -> Option<Timestamp> {
    d.map(Timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_second_precision() {
        let d = DateTime::parse_from_rfc3339("2024-01-02T03:04:05.678+02:00")
            .unwrap()
            .with_timezone(&Utc);
        let json = serde_json::to_string(&Timestamp(d)).unwrap();
        assert_eq!(json, "\"2024-01-02T01:04:05Z\"");
        let back: Timestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(back.to_github_string(), "2024-01-02T01:04:05Z");
    }
}
