pub mod binance;
pub mod gate;
pub mod gate_trade;
pub mod rest;
pub mod session;

use anyhow::Result;
use rust_decimal::Decimal;
use tokio::sync::{broadcast, mpsc};

use crate::config::TradingEnv;
use crate::types::{
    Action, BookUpdate, ClientOrderId, ExecReport, Instrument, PositionSnapshot, ReqResult,
    SymbolId, VenueId,
};

#[derive(Clone, Debug)]
pub struct ReqOutcome {
    pub coid: ClientOrderId,
    pub result: ReqResult,
    pub reason: Option<String>,
}

#[derive(Clone, Debug)]
pub enum PrivateMsg {
    Exec(ExecReport),
    Position(PositionSnapshot),
    Link { kind: crate::types::LinkKind, up: bool },
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct VenueHealth {
    pub venue: VenueId,
    pub md: bool,
    pub private: bool,
    pub trading: bool,
    pub has_keys: bool,
}

#[derive(Clone)]
pub struct VenueHandles {
    pub venue: VenueId,
    pub books: broadcast::Sender<BookUpdate>,
    pub private: broadcast::Sender<PrivateMsg>,
    pub trade_tx: mpsc::UnboundedSender<Action>,
    pub outcomes: broadcast::Sender<ReqOutcome>,
}

pub struct VenueRuntime {
    pub handles: VenueHandles,
    pub adapter: Box<dyn VenueApi>,
}

#[async_trait::async_trait]
pub trait VenueApi: Send + Sync {
    fn venue(&self) -> VenueId;
    fn has_keys(&self) -> bool;
    async fn list_instruments(&self) -> Result<Vec<Instrument>>;
    async fn volumes(&self) -> Result<Vec<(SymbolId, Decimal)>>;
    async fn open_orders(&self, symbol: &SymbolId) -> Result<Vec<crate::types::OrderRecord>>;
    async fn positions(&self) -> Result<Vec<PositionSnapshot>>;
    async fn ensure_oneway(&self) -> Result<()>;
    async fn query_order(
        &self,
        symbol: &SymbolId,
        coid: &ClientOrderId,
    ) -> Result<Option<ExecReport>>;
}

pub fn endpoints(venue: VenueId, env: TradingEnv) -> VenueEndpoints {
    // Public MD + catalog always use mainnet so observe can see real stock perps.
    // Signed trading / private streams follow TRADING_ENV.
    match (venue, env) {
        (VenueId::Binance, TradingEnv::Mainnet) => VenueEndpoints {
            rest: "https://fapi.binance.com".into(),
            public_rest: "https://fapi.binance.com".into(),
            md_ws: "wss://fstream.binance.com/stream".into(),
            user_ws: "wss://fstream.binance.com/ws".into(),
            trade_ws: "wss://ws-fapi.binance.com/ws-fapi/v1".into(),
        },
        (VenueId::Binance, TradingEnv::Testnet) => VenueEndpoints {
            rest: "https://testnet.binancefuture.com".into(),
            public_rest: "https://fapi.binance.com".into(),
            md_ws: "wss://fstream.binance.com/stream".into(),
            user_ws: "wss://stream.binancefuture.com/ws".into(),
            trade_ws: "wss://testnet.binancefuture.com/ws-fapi/v1".into(),
        },
        (VenueId::Gate, TradingEnv::Mainnet) => VenueEndpoints {
            rest: "https://api.gateio.ws".into(),
            public_rest: "https://api.gateio.ws".into(),
            md_ws: "wss://fx-ws.gateio.ws/v4/ws/usdt".into(),
            user_ws: "wss://fx-ws.gateio.ws/v4/ws/usdt".into(),
            trade_ws: "wss://fx-ws.gateio.ws/v4/ws/usdt".into(),
        },
        (VenueId::Gate, TradingEnv::Testnet) => VenueEndpoints {
            rest: "https://api-testnet.gateapi.io".into(),
            public_rest: "https://api.gateio.ws".into(),
            md_ws: "wss://fx-ws.gateio.ws/v4/ws/usdt".into(),
            user_ws: "wss://fx-ws-testnet.gateio.ws/v4/ws/usdt".into(),
            trade_ws: "wss://fx-ws-testnet.gateio.ws/v4/ws/usdt".into(),
        },
    }
}

#[derive(Clone, Debug)]
pub struct VenueEndpoints {
    pub rest: String,
    pub public_rest: String,
    pub md_ws: String,
    pub user_ws: String,
    pub trade_ws: String,
}

pub fn native_symbol(venue: VenueId, canonical: &str) -> String {
    match venue {
        VenueId::Binance => crate::types::to_binance_native(canonical),
        VenueId::Gate => crate::types::to_gate_native(canonical),
    }
}
