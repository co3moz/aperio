//! Values with a unit, written the way a person writes them.
//!
//! Every duration in a configuration is a [`Span`] (`30s`, `5m`, `2h`, `1d`)
//! and every size a [`ByteSize`] (`512KB`, `10MB`, `1GB`). A bare number is
//! still read, as seconds or bytes, because that is what a number in those
//! places has always meant; the unit is what the schema documents and what a
//! value is written back as. No key carries its unit in its name.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// A length of time: `250ms`, `30s`, `5m`, `2h`, `1d`, `1w`, or a bare number
/// of seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Span(pub std::time::Duration);

impl Span {
  pub const fn secs(n: u64) -> Self {
    Span(std::time::Duration::from_secs(n))
  }

  pub const fn millis(n: u64) -> Self {
    Span(std::time::Duration::from_millis(n))
  }

  pub fn as_secs(&self) -> u64 {
    self.0.as_secs()
  }

  pub fn is_zero(&self) -> bool {
    self.0.is_zero()
  }

  /// Parses `250ms`, `30s`, `5m`, `2h`, `1d`, `1w`, or a bare number of
  /// seconds. Case-insensitive; surrounding whitespace is ignored.
  pub fn parse(raw: &str) -> Result<Span, String> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
      return Err("an empty duration".to_string());
    }
    let split = value
      .find(|c: char| !c.is_ascii_digit())
      .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let n: u64 = number
      .parse()
      .map_err(|_| format!("`{raw}` is not a duration (write 30s, 5m, 2h, 1d)"))?;
    let millis_per: u64 = match unit.trim() {
      "" | "s" | "sec" | "secs" => 1_000,
      "ms" => 1,
      "m" | "min" => 60_000,
      "h" => 3_600_000,
      "d" => 86_400_000,
      "w" => 604_800_000,
      other => {
        return Err(format!(
          "`{raw}` has an unknown unit `{other}` (use ms, s, m, h, d or w)"
        ));
      }
    };
    n.checked_mul(millis_per)
      .map(Span::millis)
      .ok_or_else(|| format!("`{raw}` is too long"))
  }
}

impl fmt::Display for Span {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let ms = self.0.as_millis() as u64;
    if ms == 0 {
      return write!(f, "0s");
    }
    for (unit, per) in [
      ("w", 604_800_000u64),
      ("d", 86_400_000),
      ("h", 3_600_000),
      ("m", 60_000),
      ("s", 1_000),
    ] {
      if ms.is_multiple_of(per) {
        return write!(f, "{}{unit}", ms / per);
      }
    }
    write!(f, "{ms}ms")
  }
}

impl Serialize for Span {
  fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&self.to_string())
  }
}

impl<'de> Deserialize<'de> for Span {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
      Number(u64),
      Text(String),
    }
    match Raw::deserialize(d)? {
      Raw::Number(n) => Ok(Span::secs(n)),
      Raw::Text(t) => Span::parse(&t).map_err(serde::de::Error::custom),
    }
  }
}

impl JsonSchema for Span {
  fn schema_name() -> std::borrow::Cow<'static, str> {
    "Duration".into()
  }

  // Inline, so a setting's schema reads as the value it takes rather than a
  // reference an editor has to follow.
  fn inline_schema() -> bool {
    true
  }

  fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
      "description": "A duration: 250ms, 30s, 5m, 2h, 1d or 1w. A bare number is seconds.",
      "anyOf": [
        { "type": "string", "pattern": "^\\s*[0-9]+\\s*(ms|s|sec|secs|m|min|h|d|w)?\\s*$" },
        { "type": "integer", "minimum": 0 }
      ],
      "examples": ["30s", "5m", "1h"]
    })
  }
}

/// A size in bytes: `512KB`, `10MB`, `1GB`, or a bare number of bytes. The
/// units are binary (`1KB` = 1024 bytes), the way memory and file sizes are
/// usually meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct ByteSize(pub u64);

impl ByteSize {
  pub const fn bytes(n: u64) -> Self {
    ByteSize(n)
  }

  pub const fn kb(n: u64) -> Self {
    ByteSize(n * 1024)
  }

  pub const fn mb(n: u64) -> Self {
    ByteSize(n * 1024 * 1024)
  }

  pub const fn gb(n: u64) -> Self {
    ByteSize(n * 1024 * 1024 * 1024)
  }

  /// Parses `512KB`, `10MB`, `1GB` (or `KiB`/`MiB`/`GiB`, `K`/`M`/`G`), or a
  /// bare number of bytes.
  pub fn parse(raw: &str) -> Result<ByteSize, String> {
    let value = raw.trim().to_ascii_lowercase().replace(' ', "");
    if value.is_empty() {
      return Err("an empty size".to_string());
    }
    let split = value
      .find(|c: char| !c.is_ascii_digit())
      .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let n: u64 = number
      .parse()
      .map_err(|_| format!("`{raw}` is not a size (write 512KB, 10MB, 1GB)"))?;
    let per: u64 = match unit {
      "" | "b" => 1,
      "k" | "kb" | "kib" => 1024,
      "m" | "mb" | "mib" => 1024 * 1024,
      "g" | "gb" | "gib" => 1024 * 1024 * 1024,
      other => {
        return Err(format!(
          "`{raw}` has an unknown unit `{other}` (use KB, MB or GB)"
        ));
      }
    };
    n.checked_mul(per)
      .map(ByteSize)
      .ok_or_else(|| format!("`{raw}` is too large"))
  }
}

impl fmt::Display for ByteSize {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let n = self.0;
    if n == 0 {
      return write!(f, "0");
    }
    for (unit, per) in [("GB", 1u64 << 30), ("MB", 1 << 20), ("KB", 1 << 10)] {
      if n.is_multiple_of(per) {
        return write!(f, "{}{unit}", n / per);
      }
    }
    write!(f, "{n}")
  }
}

impl Serialize for ByteSize {
  fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&self.to_string())
  }
}

impl<'de> Deserialize<'de> for ByteSize {
  fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
      Number(u64),
      Text(String),
    }
    match Raw::deserialize(d)? {
      Raw::Number(n) => Ok(ByteSize(n)),
      Raw::Text(t) => ByteSize::parse(&t).map_err(serde::de::Error::custom),
    }
  }
}

impl JsonSchema for ByteSize {
  fn schema_name() -> std::borrow::Cow<'static, str> {
    "ByteSize".into()
  }

  // Inline, so a setting's schema reads as the value it takes rather than a
  // reference an editor has to follow.
  fn inline_schema() -> bool {
    true
  }

  fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
      "description": "A size: 512KB, 10MB or 1GB (binary units). A bare number is bytes.",
      "anyOf": [
        { "type": "string", "pattern": "^\\s*[0-9]+\\s*([kKmMgG]([iI]?[bB])?|[bB])?\\s*$" },
        { "type": "integer", "minimum": 0 }
      ],
      "examples": ["512KB", "10MB", "1GB"]
    })
  }
}

#[cfg(test)]
#[path = "units_tests.rs"]
mod tests;
