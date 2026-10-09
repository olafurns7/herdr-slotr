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
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(*value)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Boot(f64),
            Wall(String),
        }
        match Stored::deserialize(deserializer)? {
            Stored::Boot(value) => Ok(value),
            Stored::Wall(value) => super::parse(&value)
                .map(|wall| crate::manager::now() + wall - crate::manager::wall_now())
                .map_err(D::Error::custom),
        }
    }
}
#[cfg(target_os = "linux")]
pub mod optional {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error> {
        value.serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<f64>, D::Error> {
        #[derive(Deserialize)]
        struct Stored(#[serde(with = "super::required")] f64);
        Ok(Option::<Stored>::deserialize(deserializer)?.map(|v| v.0))
    }
}
#[test]
fn utc_milliseconds() {
    assert_eq!(format(0.0).unwrap(), "1970-01-01T00:00:00.000Z");
    assert_eq!(format(1.234).unwrap(), "1970-01-01T00:00:01.234Z");
}

#[cfg(target_os = "linux")]
pub fn display(value: &mut serde_json::Value) -> anyhow::Result<()> {
    if let Some(map) = value.as_object_mut() {
        for key in [
            "since",
            "seen_at",
            "admitted_at",
            "lease_expires_at",
            "warned_at",
            "stopping_at",
            "idle_since",
            "holder_idle_since",
            "cpu_sample_at",
            "at",
        ] {
            if let Some(seconds) = map.get(key).and_then(serde_json::Value::as_f64) {
                map.insert(
                    key.into(),
                    serde_json::json!(format(
                        crate::manager::wall_now() + seconds - crate::manager::now()
                    )?),
                );
            }
        }
    }
    Ok(())
}
