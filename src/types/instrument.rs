use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::ids::{SymbolId, VenueId};
use super::num::Px;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractKind {
    Stock,
    Crypto,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentStatus {
    Trading,
    Halt,
    Closed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instrument {
    pub venue: VenueId,
    pub symbol: SymbolId,
    pub native_symbol: String,
    pub tick_size: Decimal,
    pub lot_size: Decimal,
    pub contract_size: Decimal,
    pub min_qty: Decimal,
    pub min_notional: Decimal,
    pub quote_ccy: String,
    pub maker_fee: Decimal,
    pub taker_fee: Decimal,
    pub kind: ContractKind,
    pub status: InstrumentStatus,
    pub volume_24h: Decimal,
}

impl Instrument {
    pub fn is_tradable(&self) -> bool {
        self.status == InstrumentStatus::Trading
    }
}

/// Floor `px` to a multiple of `tick`.
pub fn floor_to_tick(px: Decimal, tick: Decimal) -> Decimal {
    if tick <= Decimal::ZERO {
        return px;
    }
    (px / tick).floor() * tick
}

/// Ceil `px` to a multiple of `tick`.
pub fn ceil_to_tick(px: Decimal, tick: Decimal) -> Decimal {
    if tick <= Decimal::ZERO {
        return px;
    }
    (px / tick).ceil() * tick
}

pub fn round_buy_px(px: Decimal, tick: Decimal) -> Decimal {
    floor_to_tick(px, tick)
}

pub fn round_sell_px(px: Decimal, tick: Decimal) -> Decimal {
    ceil_to_tick(px, tick)
}

pub fn round_qty_down(qty: Decimal, lot: Decimal) -> Decimal {
    if lot <= Decimal::ZERO {
        return qty;
    }
    (qty / lot).floor() * lot
}

/// Exit rounding must not give away target profit:
/// - covering a long (sell): price must be >= target → round up
/// - covering a short (buy): price must be <= target → round down
pub fn round_exit_px(exit_side: super::ids::Side, target: Decimal, tick: Decimal) -> Decimal {
    match exit_side {
        super::ids::Side::Sell => round_sell_px(target, tick),
        super::ids::Side::Buy => round_buy_px(target, tick),
    }
}

pub fn notional_to_qty(notional: Decimal, px: Px, contract_size: Decimal) -> Decimal {
    if px.0 <= Decimal::ZERO || contract_size <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    notional / px.0 / contract_size
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    use crate::types::ids::Side;

    #[test]
    fn buy_rounds_down_sell_rounds_up() {
        assert_eq!(round_buy_px(dec!(100.125), dec!(0.01)), dec!(100.12));
        assert_eq!(round_sell_px(dec!(100.121), dec!(0.01)), dec!(100.13));
        assert_eq!(round_qty_down(dec!(1.29), dec!(0.1)), dec!(1.2));
    }

    #[test]
    fn exit_rounding_preserves_profit() {
        assert_eq!(
            round_exit_px(Side::Sell, dec!(100.121), dec!(0.01)),
            dec!(100.13)
        );
        assert_eq!(
            round_exit_px(Side::Buy, dec!(99.889), dec!(0.01)),
            dec!(99.88)
        );
    }

    #[test]
    fn notional_converts_with_multiplier() {
        let q = notional_to_qty(dec!(200), Px::new(dec!(100)), dec!(1));
        assert_eq!(q, dec!(2));
    }
}
