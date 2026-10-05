//! GitHub's custom scalars. All are strings on the wire; the names matter
//! because clients declare variables with them (`$oid: GitObjectID`).

use async_graphql::{InputValueError, InputValueResult, Scalar, ScalarType, Value};
use chrono::{DateTime as ChronoDateTime, SecondsFormat, Utc};

macro_rules! string_scalar {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[allow(clippy::upper_case_acronyms)] // GitHub's scalar names
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(pub String);

        #[Scalar]
        impl ScalarType for $name {
            fn parse(value: Value) -> InputValueResult<Self> {
                match value {
                    Value::String(s) => Ok(Self(s)),
                    other => Err(InputValueError::expected_type(other)),
                }
            }

            fn to_value(&self) -> Value {
                Value::String(self.0.clone())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }
    };
}

string_scalar!(
    /// An RFC 3986, RFC 3987, and RFC 6570 (level 4) compliant URI string.
    URI
);
string_scalar!(
    /// A string containing HTML code.
    HTML
);
string_scalar!(
    /// A Git object ID.
    GitObjectID
);
string_scalar!(
    /// Git SSH string
    GitSSHRemote
);
string_scalar!(
    /// A string containing a Git timestamp.
    GitTimestamp
);
string_scalar!(
    /// An ISO-8601 encoded date string (`2024-01-31`).
    Date
);

/// An ISO-8601 encoded UTC date string (`2024-01-01T00:00:00Z`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DateTime(pub ChronoDateTime<Utc>);

#[Scalar]
impl ScalarType for DateTime {
    fn parse(value: Value) -> InputValueResult<Self> {
        match &value {
            Value::String(s) => ChronoDateTime::parse_from_rfc3339(s)
                .map(|d| Self(d.with_timezone(&Utc)))
                .map_err(|_| InputValueError::custom(format!("invalid DateTime {s:?}"))),
            _ => Err(InputValueError::expected_type(value)),
        }
    }

    fn to_value(&self) -> Value {
        Value::String(self.0.to_rfc3339_opts(SecondsFormat::Secs, true))
    }
}

impl From<ChronoDateTime<Utc>> for DateTime {
    fn from(d: ChronoDateTime<Utc>) -> Self {
        Self(d)
    }
}

pub fn dt(d: ChronoDateTime<Utc>) -> DateTime {
    DateTime(d)
}

pub fn odt(d: Option<ChronoDateTime<Utc>>) -> Option<DateTime> {
    d.map(DateTime)
}
