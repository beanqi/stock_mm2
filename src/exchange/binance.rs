use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};

use crate::config::{TradingEnv, VenueSecrets};
use crate::types::{
    Action, BookUpdate, ClientOrderId, ContractKind, ExecKind, ExecReport, Instrument,
    InstrumentStatus, OrderRecord, PlaceReq, PositionSnapshot, Px, Qty, ReqResult, Side, SymbolId,
    TimeInForce, TopOfBook, Ts, VenueId,
};

use super::rest::{get_json, hmac_sha256_hex, http_client, send_signed};
use super::session::{now_millis, parse_dec, spawn_text_ws};
use super::{PrivateMsg, ReqOutcome, VenueApi, VenueEndpoints, endpoints, native_symbol};

#[derive(Clone)]
pub struct BinanceVenue {
    _env: TradingEnv,
    ep: VenueEndpoints,
    key: Option<String>,
    secret: Option<String>,
    http: reqwest::Client,
}

impl BinanceVenue {
    pub fn new(env: TradingEnv, secrets: &VenueSecrets) -> Result<Self> {
        Ok(Self {
            _env: env,
            ep: endpoints(VenueId::Binance, env),
            key: secrets.binance_key.clone(),
            secret: secrets.binance_secret.clone(),
            http: http_client()?,
        })
    }

    fn signed_query(&self, mut params: BTreeMap<String, String>) -> Result<String> {
        let secret = self.secret.as_ref().context("binance secret")?;
        params
            .entry("timestamp".into())
            .or_insert_with(|| now_millis().to_string());
        params
            .entry("recvWindow".into())
            .or_insert_with(|| "5000".into());
        let qs = join_query(&params);
        let sig = hmac_sha256_hex(secret, &qs);
        Ok(format!("{qs}&signature={sig}"))
    }

    async fn signed(
        &self,
        method: reqwest::Method,
        path: &str,
        params: BTreeMap<String, String>,
        body_empty: bool,
    ) -> Result<String> {
        let key = self.key.as_ref().context("binance key")?;
        let qs = self.signed_query(params)?;
        let url = if method == reqwest::Method::GET || method == reqwest::Method::DELETE || body_empty
        {
            format!("{}{path}?{qs}", self.ep.rest)
        } else {
            format!("{}{path}", self.ep.rest)
        };
        let headers = vec![("X-MBX-APIKEY".into(), key.clone())];
        let body = if method == reqwest::Method::POST && !body_empty {
            Some(qs)
        } else {
            None
        };
        let mut req_headers = headers;
        if method == reqwest::Method::POST && body.is_some() {
            req_headers.push((
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ));
        }
        send_signed(&self.http, method, &url, req_headers, body).await
    }

    pub fn spawn_market_data(
        &self,
        symbols: Vec<SymbolId>,
        books: broadcast::Sender<BookUpdate>,
        link: mpsc::UnboundedSender<bool>,
    ) {
        let streams: Vec<String> = symbols
            .iter()
            .map(|s| format!("{}@bookTicker", native_symbol(VenueId::Binance, s.as_str()).to_ascii_lowercase()))
            .collect();
        if streams.is_empty() {
            return;
        }
        let base = self
            .ep
            .md_ws
            .trim_end_matches("/stream")
            .to_string();
        let url = if base.ends_with("/ws") {
            base
        } else {
            format!("{base}/ws")
        };
        let (tx_in, mut rx_in) = mpsc::unbounded_channel();
        let (link_tx, mut link_rx) = mpsc::unbounded_channel();
        let out = spawn_text_ws("binance-md".into(), url, tx_in, Some(link_tx));
        let params = streams.clone();
        tokio::spawn(async move {
            while let Some(up) = link_rx.recv().await {
                let _ = link.send(up);
                if up {
                    let sub = serde_json::json!({
                        "method": "SUBSCRIBE",
                        "params": params,
                        "id": 1
                    });
                    let _ = out.send(sub.to_string());
                }
            }
        });
        tokio::spawn(async move {
            let mut seen = 0u32;
            while let Some(text) = rx_in.recv().await {
                if seen < 3 {
                    tracing::debug!(len = text.len(), preview = %text.chars().take(180).collect::<String>(), "binance md frame");
                    seen += 1;
                }
                if let Some(upd) = parse_book_ticker(&text) {
                    let _ = books.send(upd);
                }
            }
        });
    }

    pub fn spawn_private(
        self: &Arc<Self>,
        private: broadcast::Sender<PrivateMsg>,
        link: mpsc::UnboundedSender<bool>,
    ) {
        if !self.has_keys() {
            return;
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match this.listen_key().await {
                    Ok(key) => {
                        let url = format!("{}/{}", this.ep.user_ws, key);
                        let (tx_in, mut rx_in) = mpsc::unbounded_channel();
                        let link2 = link.clone();
                        let _out = spawn_text_ws("binance-private".into(), url, tx_in, Some(link2));
                        let keepalive = this.clone();
                        let lk = key.clone();
                        let ka = tokio::spawn(async move {
                            loop {
                                tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
                                let _ = keepalive.keepalive(&lk).await;
                            }
                        });
                        while let Some(text) = rx_in.recv().await {
                            for msg in parse_user_data(&text) {
                                let _ = private.send(msg);
                            }
                        }
                        ka.abort();
                    }
                    Err(e) => tracing::warn!(error = %e, "binance listenKey failed"),
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        });
    }

    pub fn spawn_trading(
        self: &Arc<Self>,
        mut trade_rx: mpsc::UnboundedReceiver<Action>,
        outcomes: broadcast::Sender<ReqOutcome>,
        link: mpsc::UnboundedSender<bool>,
    ) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let _ = link.send(this.has_keys());
            while let Some(action) = trade_rx.recv().await {
                match action {
                    Action::Place(req) => {
                        let r = this.place(&req).await;
                        let _ = outcomes.send(map_result(req.coid, r));
                    }
                    Action::Cancel { coid, symbol, .. } => {
                        let r = this.cancel(&symbol, &coid).await;
                        let _ = outcomes.send(map_result(coid, r));
                    }
                    Action::Amend {
                        coid,
                        symbol,
                        px,
                        qty,
                        ..
                    } => {
                        let r = this.amend(&symbol, &coid, px, qty).await;
                        let _ = outcomes.send(map_result(coid, r));
                    }
                    Action::CancelAll { symbol, .. } => {
                        if let Err(e) = this.cancel_all(&symbol).await {
                            tracing::warn!(error = %e, "binance cancel all");
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    async fn listen_key(&self) -> Result<String> {
        let key = self.key.as_ref().context("binance key")?;
        let url = format!("{}/fapi/v1/listenKey", self.ep.rest);
        let text = send_signed(
            &self.http,
            reqwest::Method::POST,
            &url,
            vec![("X-MBX-APIKEY".into(), key.clone())],
            None,
        )
        .await?;
        let v: Value = serde_json::from_str(&text)?;
        Ok(v["listenKey"].as_str().context("listenKey")?.to_string())
    }

    async fn keepalive(&self, listen_key: &str) -> Result<()> {
        let key = self.key.as_ref().context("binance key")?;
        let url = format!(
            "{}/fapi/v1/listenKey?listenKey={listen_key}",
            self.ep.rest
        );
        let _ = send_signed(
            &self.http,
            reqwest::Method::PUT,
            &url,
            vec![("X-MBX-APIKEY".into(), key.clone())],
            None,
        )
        .await;
        Ok(())
    }

    async fn place(&self, req: &PlaceReq) -> Result<()> {
        let mut p = BTreeMap::new();
        p.insert("symbol".into(), native_symbol(VenueId::Binance, req.symbol.as_str()));
        p.insert(
            "side".into(),
            if req.side == Side::Buy {
                "BUY".into()
            } else {
                "SELL".into()
            },
        );
        p.insert(
            "type".into(),
            if req.order_type == crate::types::OrderType::Market {
                "MARKET".into()
            } else {
                "LIMIT".into()
            },
        );
        if req.order_type != crate::types::OrderType::Market {
            p.insert("price".into(), req.px.0.normalize().to_string());
            p.insert(
                "timeInForce".into(),
                match req.tif {
                    TimeInForce::PostOnly => "GTX".into(),
                    TimeInForce::Ioc => "IOC".into(),
                    TimeInForce::Fok => "FOK".into(),
                    TimeInForce::Gtc => "GTC".into(),
                },
            );
        }
        p.insert("quantity".into(), req.qty.0.normalize().to_string());
        p.insert("newClientOrderId".into(), req.coid.as_str().to_string());
        if req.reduce_only {
            p.insert("reduceOnly".into(), "true".into());
        }
        let _ = self
            .signed(reqwest::Method::POST, "/fapi/v1/order", p, false)
            .await?;
        Ok(())
    }

    async fn cancel(&self, symbol: &SymbolId, coid: &ClientOrderId) -> Result<()> {
        let mut p = BTreeMap::new();
        p.insert("symbol".into(), native_symbol(VenueId::Binance, symbol.as_str()));
        p.insert("origClientOrderId".into(), coid.as_str().to_string());
        let _ = self
            .signed(reqwest::Method::DELETE, "/fapi/v1/order", p, true)
            .await?;
        Ok(())
    }

    async fn amend(
        &self,
        symbol: &SymbolId,
        coid: &ClientOrderId,
        px: Px,
        qty: Qty,
    ) -> Result<()> {
        let mut p = BTreeMap::new();
        p.insert("symbol".into(), native_symbol(VenueId::Binance, symbol.as_str()));
        p.insert("origClientOrderId".into(), coid.as_str().to_string());
        p.insert("side".into(), "BUY".into()); // required by API; will fail if wrong — query first
        if let Ok(Some(rep)) = self.query_order(symbol, coid).await {
            p.insert(
                "side".into(),
                if rep.side == Side::Buy {
                    "BUY".into()
                } else {
                    "SELL".into()
                },
            );
        }
        p.insert("quantity".into(), qty.0.normalize().to_string());
        p.insert("price".into(), px.0.normalize().to_string());
        let _ = self
            .signed(reqwest::Method::PUT, "/fapi/v1/order", p, false)
            .await?;
        Ok(())
    }

    async fn cancel_all(&self, symbol: &SymbolId) -> Result<()> {
        let mut p = BTreeMap::new();
        p.insert("symbol".into(), native_symbol(VenueId::Binance, symbol.as_str()));
        let _ = self
            .signed(reqwest::Method::DELETE, "/fapi/v1/allOpenOrders", p, true)
            .await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl VenueApi for BinanceVenue {
    fn venue(&self) -> VenueId {
        VenueId::Binance
    }

    fn has_keys(&self) -> bool {
        self.key.as_ref().is_some_and(|s| !s.is_empty())
            && self.secret.as_ref().is_some_and(|s| !s.is_empty())
    }

    async fn list_instruments(&self) -> Result<Vec<Instrument>> {
        let url = format!("{}/fapi/v1/exchangeInfo", self.ep.public_rest);
        let v: Value = get_json(&self.http, &url, &[]).await?;
        let mut out = Vec::new();
        for s in v["symbols"].as_array().context("symbols")? {
            let status = s["status"].as_str().unwrap_or("");
            if status != "TRADING" {
                continue;
            }
            let native = s["symbol"].as_str().unwrap_or("").to_string();
            if native.is_empty() {
                continue;
            }
            let ctype = s["contractType"].as_str().unwrap_or("");
            let kind = if ctype.eq_ignore_ascii_case("TRADIFI_PERPETUAL")
                || s["underlyingType"].as_str() == Some("STOCK")
            {
                ContractKind::Stock
            } else if ctype.eq_ignore_ascii_case("PERPETUAL") {
                ContractKind::Crypto
            } else {
                continue;
            };
            let mut tick = Decimal::new(1, 2);
            let mut lot = Decimal::new(1, 3);
            let mut min_qty = lot;
            let mut min_notional = Decimal::ZERO;
            if let Some(filters) = s["filters"].as_array() {
                for f in filters {
                    match f["filterType"].as_str() {
                        Some("PRICE_FILTER") => {
                            if let Some(t) = f["tickSize"].as_str() {
                                tick = parse_dec(t)?;
                            }
                        }
                        Some("LOT_SIZE") => {
                            if let Some(t) = f["stepSize"].as_str() {
                                lot = parse_dec(t)?;
                            }
                            if let Some(t) = f["minQty"].as_str() {
                                min_qty = parse_dec(t)?;
                            }
                        }
                        Some("MIN_NOTIONAL") => {
                            if let Some(t) = f
                                .get("notional")
                                .or_else(|| f.get("minNotional"))
                                .and_then(|x| x.as_str())
                            {
                                min_notional = parse_dec(t)?;
                            }
                        }
                        _ => {}
                    }
                }
            }
            out.push(Instrument {
                venue: VenueId::Binance,
                symbol: SymbolId::new(&native),
                native_symbol: native,
                tick_size: tick,
                lot_size: lot,
                contract_size: Decimal::ONE,
                min_qty,
                min_notional,
                quote_ccy: s["quoteAsset"].as_str().unwrap_or("USDT").into(),
                maker_fee: Decimal::new(2, 4),
                taker_fee: Decimal::new(5, 4),
                kind,
                status: InstrumentStatus::Trading,
                volume_24h: Decimal::ZERO,
            });
        }
        Ok(out)
    }

    async fn volumes(&self) -> Result<Vec<(SymbolId, Decimal)>> {
        let url = format!("{}/fapi/v1/ticker/24hr", self.ep.public_rest);
        let v: Value = get_json(&self.http, &url, &[]).await?;
        let mut out = Vec::new();
        if let Some(arr) = v.as_array() {
            for t in arr {
                let sym = t["symbol"].as_str().unwrap_or_default();
                let q = t["quoteVolume"].as_str().unwrap_or("0");
                out.push((SymbolId::new(sym), parse_dec(q).unwrap_or(Decimal::ZERO)));
            }
        }
        Ok(out)
    }

    async fn open_orders(&self, symbol: &SymbolId) -> Result<Vec<OrderRecord>> {
        if !self.has_keys() {
            return Ok(vec![]);
        }
        let mut p = BTreeMap::new();
        p.insert("symbol".into(), native_symbol(VenueId::Binance, symbol.as_str()));
        let text = self
            .signed(reqwest::Method::GET, "/fapi/v1/openOrders", p, true)
            .await?;
        let v: Value = serde_json::from_str(&text)?;
        let mut out = Vec::new();
        for o in v.as_array().context("openOrders")? {
            out.push(OrderRecord {
                coid: ClientOrderId(o["clientOrderId"].as_str().unwrap_or_default().into()),
                strategy_id: String::new(),
                venue: VenueId::Binance,
                symbol: symbol.clone(),
                side: if o["side"].as_str() == Some("SELL") {
                    Side::Sell
                } else {
                    Side::Buy
                },
                purpose: String::new(),
                px: Px(parse_dec(o["price"].as_str().unwrap_or("0"))?),
                qty: Qty(parse_dec(o["origQty"].as_str().unwrap_or("0"))?),
                filled_qty: Qty(parse_dec(o["executedQty"].as_str().unwrap_or("0"))?),
                status: o["status"].as_str().unwrap_or("NEW").to_ascii_lowercase(),
                exchange_id: o["orderId"].as_i64().map(|i| i.to_string()),
                reduce_only: o["reduceOnly"].as_bool().unwrap_or(false),
                created_at: Ts::from_millis(o["time"].as_i64().unwrap_or(0)),
                updated_at: Ts::from_millis(o["updateTime"].as_i64().unwrap_or(0)),
            });
        }
        Ok(out)
    }

    async fn positions(&self) -> Result<Vec<PositionSnapshot>> {
        if !self.has_keys() {
            return Ok(vec![]);
        }
        let text = self
            .signed(reqwest::Method::GET, "/fapi/v2/positionRisk", BTreeMap::new(), true)
            .await?;
        let v: Value = serde_json::from_str(&text)?;
        let mut out = Vec::new();
        for p in v.as_array().context("positionRisk")? {
            let amt = parse_dec(p["positionAmt"].as_str().unwrap_or("0"))?;
            if amt.is_zero() {
                continue;
            }
            out.push(PositionSnapshot {
                venue: VenueId::Binance,
                symbol: SymbolId::new(p["symbol"].as_str().unwrap_or_default()),
                net_qty: Qty(amt),
                avg_px: Some(Px(parse_dec(p["entryPrice"].as_str().unwrap_or("0"))?)),
                ts: Ts::now_system(),
            });
        }
        Ok(out)
    }

    async fn ensure_oneway(&self) -> Result<()> {
        if !self.has_keys() {
            return Ok(());
        }
        let mut p = BTreeMap::new();
        p.insert("dualSidePosition".into(), "false".into());
        let _ = self
            .signed(
                reqwest::Method::POST,
                "/fapi/v1/positionSide/dual",
                p,
                false,
            )
            .await;
        Ok(())
    }

    async fn query_order(
        &self,
        symbol: &SymbolId,
        coid: &ClientOrderId,
    ) -> Result<Option<ExecReport>> {
        if !self.has_keys() {
            return Ok(None);
        }
        let mut p = BTreeMap::new();
        p.insert("symbol".into(), native_symbol(VenueId::Binance, symbol.as_str()));
        p.insert("origClientOrderId".into(), coid.as_str().to_string());
        let text = self
            .signed(reqwest::Method::GET, "/fapi/v1/order", p, true)
            .await?;
        let o: Value = serde_json::from_str(&text)?;
        Ok(Some(order_to_exec(&o)))
    }
}

fn join_query(params: &BTreeMap<String, String>) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn map_result(coid: ClientOrderId, r: Result<()>) -> ReqOutcome {
    match r {
        Ok(()) => ReqOutcome {
            coid,
            result: ReqResult::Accepted,
            reason: None,
        },
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("timeout") || msg.contains("disconnect") {
                ReqOutcome {
                    coid,
                    result: ReqResult::Unknown,
                    reason: Some(msg),
                }
            } else {
                ReqOutcome {
                    coid,
                    result: ReqResult::Rejected,
                    reason: Some(msg),
                }
            }
        }
    }
}

fn json_dec(v: &Value) -> Option<Decimal> {
    if let Some(s) = v.as_str() {
        return parse_dec(s).ok();
    }
    if let Some(n) = v.as_i64() {
        return Some(Decimal::from(n));
    }
    v.as_f64().and_then(Decimal::from_f64_retain)
}

fn parse_book_ticker(text: &str) -> Option<BookUpdate> {
    let v: Value = serde_json::from_str(text).ok()?;
    let d = if v.get("data").is_some() { &v["data"] } else { &v };
    let native = d.get("s")?.as_str()?;
    let bid = json_dec(d.get("b")?)?;
    let ask = json_dec(d.get("a")?)?;
    let bq = json_dec(d.get("B")?).unwrap_or(Decimal::ONE);
    let aq = json_dec(d.get("A")?).unwrap_or(Decimal::ONE);
    let exch = d
        .get("E")
        .or_else(|| d.get("T"))
        .and_then(|x| x.as_i64())
        .map(Ts::from_millis)
        .unwrap_or_else(Ts::now_system);
    Some(BookUpdate {
        book: TopOfBook {
            venue: VenueId::Binance,
            symbol: SymbolId::new(native),
            bid_px: Px(bid),
            bid_qty: Qty(bq),
            ask_px: Px(ask),
            ask_qty: Qty(aq),
            exchange_ts: exch,
            recv_ts: Ts::now_system(),
        },
        seq: d.get("u").and_then(|x| x.as_u64()),
    })
}

fn parse_user_data(text: &str) -> Vec<PrivateMsg> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    let ev = v.get("e").and_then(|x| x.as_str()).unwrap_or("");
    match ev {
        "ORDER_TRADE_UPDATE" => {
            let o = &v["o"];
            vec![PrivateMsg::Exec(order_trade_to_exec(o, &v))]
        }
        "ACCOUNT_UPDATE" => {
            let mut out = Vec::new();
            if let Some(arr) = v["a"]["P"].as_array() {
                for p in arr {
                    let amt = parse_dec(p["pa"].as_str().unwrap_or("0")).unwrap_or(Decimal::ZERO);
                    out.push(PrivateMsg::Position(PositionSnapshot {
                        venue: VenueId::Binance,
                        symbol: SymbolId::new(p["s"].as_str().unwrap_or_default()),
                        net_qty: Qty(amt),
                        avg_px: parse_dec(p["ep"].as_str().unwrap_or("0")).ok().map(Px),
                        ts: Ts::from_millis(v["E"].as_i64().unwrap_or(0)),
                    }));
                }
            }
            out
        }
        _ => vec![],
    }
}

fn order_trade_to_exec(o: &Value, root: &Value) -> ExecReport {
    let status = o["X"].as_str().unwrap_or("");
    let last_qty = parse_dec(o["l"].as_str().unwrap_or("0")).unwrap_or(Decimal::ZERO);
    let kind = match status {
        "NEW" => ExecKind::Ack,
        "PARTIALLY_FILLED" => ExecKind::PartialFill,
        "FILLED" => ExecKind::Fill,
        "CANCELED" => ExecKind::Canceled,
        "EXPIRED" => ExecKind::Expired,
        "REJECTED" => ExecKind::Reject,
        _ => {
            if last_qty > Decimal::ZERO {
                ExecKind::PartialFill
            } else {
                ExecKind::Ack
            }
        }
    };
    ExecReport {
        coid: ClientOrderId(o["c"].as_str().unwrap_or_default().into()),
        exchange_id: o["i"].as_i64().map(|i| i.to_string()),
        venue: VenueId::Binance,
        symbol: SymbolId::new(o["s"].as_str().unwrap_or_default()),
        kind,
        side: if o["S"].as_str() == Some("SELL") {
            Side::Sell
        } else {
            Side::Buy
        },
        px: parse_dec(o["p"].as_str().unwrap_or("0")).ok().map(Px),
        qty: parse_dec(o["q"].as_str().unwrap_or("0")).ok().map(Qty),
        filled_qty: Qty(parse_dec(o["z"].as_str().unwrap_or("0")).unwrap_or(Decimal::ZERO)),
        last_px: parse_dec(o["L"].as_str().unwrap_or("0")).ok().map(Px),
        last_qty: if last_qty > Decimal::ZERO {
            Some(Qty(last_qty))
        } else {
            None
        },
        reason: None,
        ts: Ts::from_millis(root["E"].as_i64().unwrap_or(0)),
    }
}

fn order_to_exec(o: &Value) -> ExecReport {
    let status = o["status"].as_str().unwrap_or("");
    ExecReport {
        coid: ClientOrderId(o["clientOrderId"].as_str().unwrap_or_default().into()),
        exchange_id: o["orderId"].as_i64().map(|i| i.to_string()),
        venue: VenueId::Binance,
        symbol: SymbolId::new(o["symbol"].as_str().unwrap_or_default()),
        kind: match status {
            "NEW" => ExecKind::Ack,
            "PARTIALLY_FILLED" => ExecKind::PartialFill,
            "FILLED" => ExecKind::Fill,
            "CANCELED" => ExecKind::Canceled,
            "EXPIRED" => ExecKind::Expired,
            _ => ExecKind::Ack,
        },
        side: if o["side"].as_str() == Some("SELL") {
            Side::Sell
        } else {
            Side::Buy
        },
        px: parse_dec(o["price"].as_str().unwrap_or("0")).ok().map(Px),
        qty: parse_dec(o["origQty"].as_str().unwrap_or("0")).ok().map(Qty),
        filled_qty: Qty(parse_dec(o["executedQty"].as_str().unwrap_or("0")).unwrap_or(Decimal::ZERO)),
        last_px: None,
        last_qty: None,
        reason: None,
        ts: Ts::from_millis(o["updateTime"].as_i64().unwrap_or(0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_combined_book_ticker() {
        let raw = r#"{"stream":"aaplusdt@bookTicker","data":{"u":1,"s":"AAPLUSDT","b":"100.1","B":"2","a":"100.2","A":"3"}}"#;
        let u = parse_book_ticker(raw).unwrap();
        assert_eq!(u.book.symbol.as_str(), "AAPLUSDT");
        assert_eq!(u.book.bid_px.0.to_string(), "100.1");
    }
}
