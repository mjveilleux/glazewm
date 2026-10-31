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

impl Color {
  /// Mixes this color towards `other`: `progress` 0 is this color, 1 is
  /// `other`. Progress outside [0, 1] (from an overshooting curve) is
  /// clamped.
  #[must_use]
  pub fn interpolate(&self, other: &Color, progress: f32) -> Color {
    let progress = progress.clamp(0.0, 1.0);

    // LINT: The mix of two `u8`s, rounded, is a `u8`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mix = |from: u8, to: u8| {
      (f32::from(from) + (f32::from(to) - f32::from(from)) * progress)
        .round() as u8
    };

    Color {
      r: mix(self.r, other.r),
      g: mix(self.g, other.g),
      b: mix(self.b, other.b),
      a: mix(self.a, other.a),
    }
  }
}

impl FromStr for Color {
  type Err = crate::ParseError;

  fn from_str(unparsed: &str) -> Result<Self, crate::ParseError> {
    let mut chars = unparsed.chars();

    if chars.next() != Some('#') {
      return Err(crate::ParseError::Color(unparsed.to_string()));
    }

    let parse_hex = |slice: &str| -> Result<u8, crate::ParseError> {
      u8::from_str_radix(slice, 16)
        .map_err(|_| crate::ParseError::Color(unparsed.to_string()))
    };

    let r = parse_hex(&unparsed[1..3])?;
    let g = parse_hex(&unparsed[3..5])?;
    let b = parse_hex(&unparsed[5..7])?;

    let a = match unparsed.len() {
      9 => parse_hex(&unparsed[7..9])?,
      7 => 255,
      _ => return Err(crate::ParseError::Color(unparsed.to_string())),
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
  use super::Color;

  #[test]
  fn interpolate_mixes_and_clamps() {
    let black = Color {
      r: 0,
      g: 0,
      b: 0,
      a: 255,
    };
    let white = Color {
      r: 255,
      g: 255,
      b: 255,
      a: 255,
    };

    assert_eq!(black.interpolate(&white, 0.0), black);
    assert_eq!(black.interpolate(&white, 1.0), white);
    assert_eq!(black.interpolate(&white, 0.5).r, 128);
    assert_eq!(black.interpolate(&white, 1.2), white);
    assert_eq!(black.interpolate(&white, -0.3), black);
  }
}
