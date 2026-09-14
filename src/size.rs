//! Sizes as PVE spells them: `20G`, `512M`, `2T`, or a bare number of GiB.
//!
//! PVE takes a mountpoint size as GiB (`storage:20`, fractions allowed) and
//! reports it back as `size=20G` / `size=512M`; a document says `20G`. One
//! type, bytes inside, so the two never disagree by rounding.

use std::fmt;
use std::str::FromStr;

use anyhow::{bail, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Size(pub u64);

const KIB: u64 = 1024;
const MIB: u64 = KIB * 1024;
const GIB: u64 = MIB * 1024;
const TIB: u64 = GIB * 1024;

impl Size {
    pub fn bytes(self) -> u64 {
        self.0
    }

    /// The size as `pct set` wants it in `storage:<size>`: GiB, fractional
    /// when needed, with no trailing zeros (`0.5`, `20`).
    pub fn to_pct_gib(self) -> String {
        let gib = self.0 as f64 / GIB as f64;
        if (gib - gib.round()).abs() < 1e-9 {
            format!("{}", gib.round() as u64)
        } else {
            let s = format!("{gib:.6}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        }
    }

    /// The size as `pct resize` wants it: an absolute size with a unit.
    pub fn to_pct_resize(self) -> String {
        self.to_string()
    }

    /// For a human: the largest unit that fits, one decimal (`14.2G`).
    pub fn human(self) -> String {
        let b = self.0 as f64;
        for (unit, div) in [("T", TIB), ("G", GIB), ("M", MIB), ("K", KIB)] {
            if b >= div as f64 {
                return format!("{:.1}{unit}", b / div as f64);
            }
        }
        format!("{}", self.0)
    }
}

impl fmt::Display for Size {
    #[allow(clippy::manual_is_multiple_of)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0;
        if b >= TIB && b % TIB == 0 {
            write!(f, "{}T", b / TIB)
        } else if b >= GIB && b % GIB == 0 {
            write!(f, "{}G", b / GIB)
        } else if b >= MIB && b % MIB == 0 {
            write!(f, "{}M", b / MIB)
        } else if b >= KIB && b % KIB == 0 {
            write!(f, "{}K", b / KIB)
        } else {
            write!(f, "{b}")
        }
    }
}

impl FromStr for Size {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.is_empty() {
            bail!("empty size");
        }
        let (num, unit) = match s
            .char_indices()
            .find(|(_, c)| !(c.is_ascii_digit() || *c == '.'))
        {
            Some((i, _)) => (&s[..i], &s[i..]),
            None => (s, "G"),
        };
        let n: f64 = num.parse().map_err(|_| anyhow::anyhow!("bad size '{s}'"))?;
        let mult = match unit.trim().to_ascii_uppercase().as_str() {
            "" | "G" | "GB" | "GIB" => GIB,
            "M" | "MB" | "MIB" => MIB,
            "T" | "TB" | "TIB" => TIB,
            "K" | "KB" | "KIB" => KIB,
            "B" => 1,
            _ => bail!("bad size unit in '{s}'"),
        };
        let bytes = (n * mult as f64).round();
        if bytes < 1.0 {
            bail!("size '{s}' is zero");
        }
        Ok(Size(bytes as u64))
    }
}

impl Serialize for Size {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        // A YAML `20G` is a string; a bare `20` may arrive as a number.
        let v = serde_yaml_ng::Value::deserialize(d)?;
        let s = match &v {
            serde_yaml_ng::Value::String(s) => s.clone(),
            serde_yaml_ng::Value::Number(n) => n.to_string(),
            _ => return Err(serde::de::Error::custom("size must be a string like 20G")),
        };
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints() {
        assert_eq!("20G".parse::<Size>().unwrap(), Size(20 * GIB));
        assert_eq!("512M".parse::<Size>().unwrap().to_string(), "512M");
        assert_eq!("2T".parse::<Size>().unwrap().to_pct_gib(), "2048");
        assert_eq!("4".parse::<Size>().unwrap().to_pct_gib(), "4");
        assert_eq!("512M".parse::<Size>().unwrap().to_pct_gib(), "0.5");
        assert_eq!("1.5G".parse::<Size>().unwrap().to_string(), "1536M");
        assert!("x".parse::<Size>().is_err());
        assert_eq!(Size(14910224 * 1024).human(), "14.2G");
        assert_eq!(Size(512).human(), "512");
    }
}
