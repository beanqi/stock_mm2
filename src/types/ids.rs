use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VenueId {
    Binance,
    Gate,
}

impl VenueId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Binance => "binance",
            Self::Gate => "gate",
        }
    }

    pub fn all() -> [VenueId; 2] {
        [VenueId::Binance, VenueId::Gate]
    }
}

impl fmt::Display for VenueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for VenueId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "binance" | "bn" => Ok(Self::Binance),
            "gate" | "gateio" => Ok(Self::Gate),
            other => Err(format!("unknown venue: {other}")),
        }
    }
}

/// Canonical symbol, e.g. `AAPLUSDT` (no underscore, upper case).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SymbolId(pub String);

impl SymbolId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(canonicalize_symbol(&raw.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SymbolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn canonicalize_symbol(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Gate native form: `AAPL_USDT`.
pub fn to_gate_native(canonical: &str) -> String {
    if let Some(idx) = canonical.find("USDT") {
        if idx > 0 && !canonical.contains('_') {
            return format!("{}_USDT", &canonical[..idx]);
        }
    }
    if canonical.contains('_') {
        return canonical.to_string();
    }
    format!("{canonical}_USDT")
}

/// Binance native form: `AAPLUSDT`.
pub fn to_binance_native(canonical: &str) -> String {
    canonicalize_symbol(canonical)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StrategyId(pub String);

impl StrategyId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StrategyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientOrderId(pub String);

impl ClientOrderId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn strategy_prefix(&self) -> Option<&str> {
        self.0.split('-').next()
    }
}

impl fmt::Display for ClientOrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
        }
    }

    pub fn opposite(self) -> Self {
        match self {
            Self::Buy => Self::Sell,
            Self::Sell => Self::Buy,
        }
    }

    pub fn sign(self) -> i32 {
        match self {
            Self::Buy => 1,
            Self::Sell => -1,
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    GridEntry,
    Exit,
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GridEntry => "grid",
            Self::Exit => "exit",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SlotKey {
    pub purpose: Purpose,
    pub side: Side,
    pub index: u8,
}

impl SlotKey {
    pub fn grid(side: Side, index: u8) -> Self {
        Self {
            purpose: Purpose::GridEntry,
            side,
            index,
        }
    }

    pub fn exit(side: Side, index: u8) -> Self {
        Self {
            purpose: Purpose::Exit,
            side,
            index,
        }
    }
}

impl fmt::Display for SlotKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}-{}-{}",
            self.purpose.as_str(),
            self.side.as_str(),
            self.index
        )
    }
}

/// `{strategy}-{purpose}-{side}-{index}-{seq}`
pub fn make_client_order_id(strategy: &StrategyId, key: SlotKey, seq: u64) -> ClientOrderId {
    ClientOrderId(format!(
        "{}-{}-{}-{}-{}",
        strategy.as_str(),
        key.purpose.as_str(),
        key.side.as_str(),
        key.index,
        seq
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LotId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimerId(pub u64);

impl TimerId {
    pub const RECONCILE: Self = Self(1);
    pub const EXIT_SCAN: Self = Self(2);
    pub const COOLDOWN: Self = Self(3);
    pub const UNKNOWN_SCAN: Self = Self(4);

    pub fn lot_fast(lot: LotId) -> Self {
        Self(1000 + lot.0)
    }

    pub fn lot_force(lot: LotId) -> Self {
        Self(2000 + lot.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    MarketData,
    Private,
    Trading,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Observe,
    Live,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyMode {
    Init,
    Warmup,
    Running,
    Degraded,
    Flattening,
    Stopped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_gate_and_binance() {
        assert_eq!(canonicalize_symbol("AAPL_USDT"), "AAPLUSDT");
        assert_eq!(canonicalize_symbol("aapl-usdt"), "AAPLUSDT");
        assert_eq!(to_gate_native("AAPLUSDT"), "AAPL_USDT");
        assert_eq!(to_binance_native("AAPL_USDT"), "AAPLUSDT");
    }

    #[test]
    fn client_order_id_format() {
        let id = make_client_order_id(
            &StrategyId::new("s1"),
            SlotKey::grid(Side::Buy, 0),
            7,
        );
        assert_eq!(id.as_str(), "s1-grid-buy-0-7");
        assert_eq!(id.strategy_prefix(), Some("s1"));
    }
}
