use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Color {
  pub r: u8,
  pub g: u8,
  pub b: u8,
  pub a: u8,
}

impl Color {
  #[must_use]
  #[allow(clippy::missing_panics_doc)]
  pub fn to_bgr(&self) -> u32 {
    let bgr = format!("{:02x}{:02x}{:02x}", self.b, self.g, self.r);
    // SAFETY: An invalid hex value is unrepresentable.
    u32::from_str_radix(&bgr, 16).unwrap()
  }
}

impl FromStr for Color {
  type Err = crate::ParseError;

  fn from_str(unparsed: &str) -> Result<Self, crate::ParseError> {
    let invalid = || crate::ParseError::Color(unparsed.to_string());

    // Check the shape before slicing, so that short or non-ASCII input
    // (e.g. a typo in a command) is an error rather than a panic.
    let hex = unparsed.strip_prefix('#').ok_or_else(invalid)?;

    if !matches!(hex.len(), 6 | 8)
      || !hex.chars().all(|char| char.is_ascii_hexdigit())
    {
      return Err(invalid());
    }

    let parse_hex = |slice: &str| -> Result<u8, crate::ParseError> {
      u8::from_str_radix(slice, 16).map_err(|_| invalid())
    };

    let r = parse_hex(&hex[0..2])?;
    let g = parse_hex(&hex[2..4])?;
    let b = parse_hex(&hex[4..6])?;

    let a = if hex.len() == 8 {
      parse_hex(&hex[6..8])?
    } else {
      255
    };

    Ok(Self { r, g, b, a })
  }
}

/// Deserialize a `Color` from either a string or a struct.
impl<'de> Deserialize<'de> for Color {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: Deserializer<'de>,
  {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ColorDe {
      Struct { r: u8, g: u8, b: u8, a: u8 },
      String(String),
    }

    match ColorDe::deserialize(deserializer)? {
      ColorDe::Struct { r, g, b, a } => Ok(Self { r, g, b, a }),
      ColorDe::String(str) => {
        Self::from_str(&str).map_err(serde::de::Error::custom)
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use std::str::FromStr;

  use super::Color;

  #[test]
  fn parses_rgb_and_rgba_hex() {
    assert_eq!(
      Color::from_str("#a6e3a1").ok(),
      Some(Color {
        r: 0xa6,
        g: 0xe3,
        b: 0xa1,
        a: 0xff,
      })
    );
    assert_eq!(
      Color::from_str("#A6E3A180").ok(),
      Some(Color {
        r: 0xa6,
        g: 0xe3,
        b: 0xa1,
        a: 0x80,
      })
    );
  }

  #[test]
  fn rejects_malformed_hex_without_panicking() {
    for input in [
      "", "#", "#a6", "a6e3a1", "#a6e3a", "#a6e3a1f", "#gggggg", "#a6e3é1",
    ] {
      assert!(Color::from_str(input).is_err(), "{input} should fail");
    }
  }
}
