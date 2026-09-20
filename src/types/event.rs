use serde::{Deserialize, Serialize};

use super::book::{BookUpdate, FxUpdate};
use super::ids::{ClientOrderId, LinkKind, StrategyId, SymbolId, TimerId, VenueId};
use super::order::{ExecReport, ReqResult};
use super::position::{AssetUpdate, PositionSnapshot};
use super::time::Ts;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlCmd {
    Start,
    Stop,
    Flatten,
    ConfigUpdate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    RefBook(BookUpdate),
    MakerBook(BookUpdate),
    Fx(FxUpdate),
    Exec(ExecReport),
    PositionSync(PositionSnapshot),
    Asset(AssetUpdate),
    ReqOutcome {
        coid: ClientOrderId,
        result: ReqResult,
        reason: Option<String>,
    },
    Timer(TimerId),
    Link {
        venue: VenueId,
        kind: LinkKind,
        up: bool,
    },
    Control(ControlCmd),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoutedEvent {
    pub strategy_id: StrategyId,
    pub symbol: Option<SymbolId>,
    pub event: Event,
    pub now: Ts,
}
