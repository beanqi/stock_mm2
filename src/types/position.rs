use serde::{Deserialize, Serialize};

use super::ids::{LotId, Side, SymbolId, VenueId};
use super::num::{Px, Qty};
use super::time::Ts;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitPhase {
    MakerTp,
    FastMaker,
    TakerExit,
    Closed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lot {
    pub id: LotId,
    pub side: Side,
    pub qty: Qty,
    pub remaining: Qty,
    pub entry_px: Px,
    pub opened_at: Ts,
    pub phase: ExitPhase,
    pub fast_deadline: Ts,
    pub force_deadline: Ts,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PositionSnapshot {
    pub venue: VenueId,
    pub symbol: SymbolId,
    pub net_qty: Qty,
    pub avg_px: Option<Px>,
    pub ts: Ts,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetUpdate {
    pub venue: VenueId,
    pub currency: String,
    pub available: rust_decimal::Decimal,
    pub equity: rust_decimal::Decimal,
    pub ts: Ts,
}
