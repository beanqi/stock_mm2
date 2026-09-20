use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::ids::{ClientOrderId, Side, SymbolId, VenueId};
use super::num::{Px, Qty};
use super::time::Ts;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
    PostOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderType {
    Limit,
    Market,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotStateKind {
    Empty,
    PendingNew,
    Live,
    PendingAmend,
    PendingCancel,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecKind {
    Ack,
    Reject,
    PartialFill,
    Fill,
    Canceled,
    Expired,
    AmendAck,
    AmendReject,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecReport {
    pub coid: ClientOrderId,
    pub exchange_id: Option<String>,
    pub venue: VenueId,
    pub symbol: SymbolId,
    pub kind: ExecKind,
    pub side: Side,
    pub px: Option<Px>,
    pub qty: Option<Qty>,
    pub filled_qty: Qty,
    pub last_px: Option<Px>,
    pub last_qty: Option<Qty>,
    pub reason: Option<String>,
    pub ts: Ts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReqResult {
    Accepted,
    Rejected,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaceReq {
    pub coid: ClientOrderId,
    pub venue: VenueId,
    pub symbol: SymbolId,
    pub side: Side,
    pub px: Px,
    pub qty: Qty,
    pub tif: TimeInForce,
    pub reduce_only: bool,
    pub order_type: OrderType,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesiredOrder {
    pub px: Px,
    pub qty: Qty,
    pub reduce_only: bool,
    pub tif: TimeInForce,
    pub order_type: OrderType,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiveOrder {
    pub coid: ClientOrderId,
    pub exchange_id: Option<String>,
    pub px: Px,
    pub qty: Qty,
    pub filled_qty: Qty,
    pub reduce_only: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrderRecord {
    pub coid: ClientOrderId,
    pub strategy_id: String,
    pub venue: VenueId,
    pub symbol: SymbolId,
    pub side: Side,
    pub purpose: String,
    pub px: Px,
    pub qty: Qty,
    pub filled_qty: Qty,
    pub status: String,
    pub exchange_id: Option<String>,
    pub reduce_only: bool,
    pub created_at: Ts,
    pub updated_at: Ts,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FillRecord {
    pub id: i64,
    pub strategy_id: String,
    pub coid: ClientOrderId,
    pub venue: VenueId,
    pub symbol: SymbolId,
    pub side: Side,
    pub px: Px,
    pub qty: Qty,
    pub fee: Option<Decimal>,
    pub ts: Ts,
}
