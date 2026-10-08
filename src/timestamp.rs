#[cfg(target_os = "linux")]
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, format_description};

pub fn format(seconds: f64) -> anyhow::Result<String> {
    let datetime = OffsetDateTime::from_unix_timestamp_nanos((seconds * 1_000_000_000.0) as i128)?;
    Ok(datetime.format(&format_description::parse_borrowed::<2>(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z",
    )?)?)
}
#[cfg(target_os = "linux")]
fn parse(value: &str) -> anyhow::Result<f64> {
    Ok(OffsetDateTime::parse(value, &Rfc3339)?.unix_timestamp_nanos() as f64 / 1_000_000_000.0)
}
#[cfg(target_os = "linux")]
pub mod required {
    use serde::{Deserialize, Deserializer, Serializer, de::Error, ser::Error as _};
    pub fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::format(*value).map_err(S::Error::custom)?)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        super::parse(&String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}
#[cfg(target_os = "linux")]
pub mod optional {
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error, ser::Error as _};
    pub fn serialize<S: Serializer>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error> {
        value
            .map(super::format)
            .transpose()
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<f64>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .as_deref()
            .map(super::parse)
            .transpose()
            .map_err(D::Error::custom)
    }
}
#[test]
fn utc_milliseconds() {
    assert_eq!(format(0.0).unwrap(), "1970-01-01T00:00:00.000Z");
    assert_eq!(format(1.234).unwrap(), "1970-01-01T00:00:01.234Z");
}
