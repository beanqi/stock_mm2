use serde::{Deserialize, Serialize};

use super::ids::{SymbolId, VenueId};
use super::num::{Px, Qty};
use super::time::Ts;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TopOfBook {
    pub venue: VenueId,
    pub symbol: SymbolId,
    pub bid_px: Px,
    pub bid_qty: Qty,
    pub ask_px: Px,
    pub ask_qty: Qty,
    pub exchange_ts: Ts,
    pub recv_ts: Ts,
}

impl TopOfBook {
    pub fn mid(&self) -> Option<Px> {
        if self.is_valid() {
            Some(Px::mid(self.bid_px, self.ask_px))
        } else {
            None
        }
    }

    pub fn is_valid(&self) -> bool {
        self.bid_px.is_positive()
            && self.ask_px.is_positive()
            && self.bid_px < self.ask_px
            && self.bid_qty.is_positive()
            && self.ask_qty.is_positive()
    }

    pub fn crossed(&self) -> bool {
        self.bid_px.is_positive() && self.ask_px.is_positive() && self.bid_px >= self.ask_px
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BookUpdate {
    pub book: TopOfBook,
    pub seq: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FxUpdate {
    pub pair: String,
    pub rate: rust_decimal::Decimal,
    pub ts: Ts,
}
