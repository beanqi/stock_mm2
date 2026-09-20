use serde::{Deserialize, Serialize};

use super::book::TopOfBook;
use super::ids::{ClientOrderId, Side, SlotKey, StrategyId, SymbolId, VenueId};
use super::num::{Px, Qty};
use super::order::SlotStateKind;
use super::position::Lot;
use super::{RunMode, StrategyMode};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LinkStatus {
    pub ref_md: bool,
    pub maker_md: bool,
    pub private: bool,
    pub trading: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuotePermission {
    pub allow_new_buy: bool,
    pub allow_new_sell: bool,
    pub force_cancel_all: bool,
    pub force_flatten: bool,
    pub reason: Option<String>,
}

impl QuotePermission {
    pub fn open() -> Self {
        Self {
            allow_new_buy: true,
            allow_new_sell: true,
            force_cancel_all: false,
            force_flatten: false,
            reason: None,
        }
    }

    pub fn reduce_only(reason: impl Into<String>) -> Self {
        Self {
            allow_new_buy: false,
            allow_new_sell: false,
            force_cancel_all: false,
            force_flatten: false,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlotView {
    pub key: SlotKey,
    pub state: SlotStateKind,
    pub coid: Option<ClientOrderId>,
    pub side: Side,
    pub px: Option<Px>,
    pub qty: Option<Qty>,
    pub filled_qty: Qty,
    pub desired_px: Option<Px>,
    pub desired_qty: Option<Qty>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub strategy_id: StrategyId,
    pub name: String,
    pub lifecycle: StrategyMode,
    pub run_mode: RunMode,
    pub maker_venue: VenueId,
    pub maker_symbol: SymbolId,
    pub ref_venue: VenueId,
    pub ref_symbol: SymbolId,
    pub fair_price: Option<Px>,
    pub natural_spread: Option<f64>,
    pub spread_vol: Option<f64>,
    pub spread_t: Option<f64>,
    pub ref_book: Option<TopOfBook>,
    pub maker_book: Option<TopOfBook>,
    pub permission: QuotePermission,
    pub slots: Vec<SlotView>,
    pub lots: Vec<Lot>,
    pub net_qty: Qty,
    pub links: LinkStatus,
    pub warmup_samples: usize,
    pub warmup_needed: usize,
    pub enabled: bool,
}
