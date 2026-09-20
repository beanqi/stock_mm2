use serde::{Deserialize, Serialize};

use super::ids::{ClientOrderId, SymbolId, TimerId, VenueId};
use super::num::Px;
use super::order::PlaceReq;
use super::time::Ts;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TelemetryEvent {
    Guard { reason: String },
    Fair { px: String, spread: f64, vol: f64 },
    Jump { prev: String, next: String },
    Info { msg: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Place(PlaceReq),
    Cancel {
        coid: ClientOrderId,
        venue: VenueId,
        symbol: SymbolId,
    },
    Amend {
        coid: ClientOrderId,
        venue: VenueId,
        symbol: SymbolId,
        px: Px,
        qty: super::num::Qty,
    },
    CancelAll {
        venue: VenueId,
        symbol: SymbolId,
    },
    SetTimer {
        id: TimerId,
        at: Ts,
    },
    ClearTimer(TimerId),
    Emit(TelemetryEvent),
}

impl Action {
    pub fn priority(&self) -> u8 {
        match self {
            Action::CancelAll { .. } => 0,
            Action::Cancel { .. } => 1,
            Action::Place(req) if req.reduce_only => 2,
            Action::Amend { .. } => 3,
            Action::Place(_) => 4,
            Action::SetTimer { .. } | Action::ClearTimer(_) | Action::Emit(_) => 5,
        }
    }
}
