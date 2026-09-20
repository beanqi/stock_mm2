use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};

use crate::config::{TradingEnv, VenueSecrets};
use crate::types::{
    Action, BookUpdate, ClientOrderId, ContractKind, ExecKind, ExecReport, Instrument,
    InstrumentStatus, OrderRecord, PlaceReq, PositionSnapshot, Px, Qty, ReqResult, Side, SymbolId,
    TimeInForce, TopOfBook, Ts, VenueId,
};

use super::rest::{get_json, hmac_sha512_hex, http_client, send_signed, sha512_hex};
use super::gate_trade::GateTradePool;
use super::session::{now_secs, parse_dec, spawn_text_ws};
use super::{PrivateMsg, ReqOutcome, VenueApi, VenueEndpoints, endpoints, native_symbol};

/// Gate USDT perps on a **unified account** with **one-way** positions (`position_mode=single`).
/// Trading still uses `/api/v4/futures/usdt/*`; account mode is `/api/v4/unified/*`.
#[derive(Clone)]
pub struct GateVenue {
    _env: TradingEnv,
    ep: VenueEndpoints,
    key: Option<String>,
    secret: Option<String>,
    http: reqwest::Client,
}

impl GateVenue {
    pub fn new(env: TradingEnv, secrets: &VenueSecrets) -> Result<Self> {
        Ok(Self {
            _env: env,
            ep: endpoints(VenueId::Gate, env),
            key: secrets.gate_key.clone(),
            secret: secrets.gate_secret.clone(),
            http: http_client()?,
        })
    }

    fn sign(&self, method: &str, path: &str, query: &str, body: &str, ts: u64) -> Result<(String, String)> {
        let key = self.key.as_ref().context("gate key")?;
        let secret = self.secret.as_ref().context("gate secret")?;
        let hashed = sha512_hex(body);
        let payload = format!("{method}\n{path}\n{query}\n{hashed}\n{ts}");
        Ok((key.clone(), hmac_sha512_hex(secret, &payload)))
    }

    async fn signed(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &str,
        body: Option<&str>,
    ) -> Result<String> {
        let ts = now_secs();
        let body_s = body.unwrap_or("");
        let (key, sign) = self.sign(method.as_str(), path, query, body_s, ts)?;
        let url = if query.is_empty() {
            format!("{}{path}", self.ep.rest)
        } else {
            format!("{}{path}?{query}", self.ep.rest)
        };
        let headers = vec![
            ("KEY".into(), key),
            ("Timestamp".into(), ts.to_string()),
            ("SIGN".into(), sign),
        ];
        send_signed(
            &self.http,
            method,
            &url,
            headers,
            body.map(|s| s.to_string()),
        )
        .await
    }

    pub fn spawn_market_data(
        &self,
        symbols: Vec<SymbolId>,
        books: broadcast::Sender<BookUpdate>,
        link: mpsc::UnboundedSender<bool>,
    ) {
        if symbols.is_empty() {
            return;
        }
        let natives: Vec<String> = symbols
            .iter()
            .map(|s| native_symbol(VenueId::Gate, s.as_str()))
            .collect();
        let url = self.ep.md_ws.clone();
        let (tx_in, mut rx_in) = mpsc::unbounded_channel();
        let (link_tx, mut link_rx) = mpsc::unbounded_channel();
        let out = spawn_text_ws("gate-md".into(), url, tx_in, Some(link_tx));
        let payload = natives.clone();
        tokio::spawn(async move {
            while let Some(up) = link_rx.recv().await {
                let _ = link.send(up);
                if up {
                    let sub = json!({
                        "time": now_secs(),
                        "channel": "futures.book_ticker",
                        "event": "subscribe",
                        "payload": payload,
                    });
                    let _ = out.send(sub.to_string());
                }
            }
        });
        tokio::spawn(async move {
            let mut seen = 0u32;
            while let Some(text) = rx_in.recv().await {
                if seen < 3 {
                    tracing::debug!(len = text.len(), preview = %text.chars().take(180).collect::<String>(), "gate md frame");
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
        let url = this.ep.user_ws.clone();
        let (tx_in, mut rx_in) = mpsc::unbounded_channel();
        let (link_tx, mut link_rx) = mpsc::unbounded_channel();
        let out = spawn_text_ws("gate-private".into(), url, tx_in, Some(link_tx));
        let private_link = private.clone();
        tokio::spawn(async move {
            while let Some(up) = link_rx.recv().await {
                let _ = link.send(up);
                let _ = private_link.send(PrivateMsg::Link {
                    kind: crate::types::LinkKind::Private,
                    up,
                });
                if up {
                    for ch in ["futures.orders", "futures.usertrades", "futures.positions"] {
                        if let Ok(msg) = this.auth_sub(ch) {
                            let _ = out.send(msg);
                        }
                    }
                }
            }
        });
        tokio::spawn(async move {
            while let Some(text) = rx_in.recv().await {
                for msg in parse_private(&text) {
                    let _ = private.send(msg);
                }
            }
        });
    }

    fn auth_sub(&self, channel: &str) -> Result<String> {
        let key = self.key.as_ref().context("gate key")?;
        let secret = self.secret.as_ref().context("gate secret")?;
        let time = now_secs();
        let msg = format!("channel={channel}&event=subscribe&time={time}");
        let sign = hmac_sha512_hex(secret, &msg);
        Ok(json!({
            "time": time,
            "channel": channel,
            "event": "subscribe",
            "payload": ["!all"],
            "auth": { "method": "api_key", "KEY": key, "SIGN": sign }
        })
        .to_string())
    }

    pub fn spawn_trading(
        self: &Arc<Self>,
        mut trade_rx: mpsc::UnboundedReceiver<Action>,
        outcomes: broadcast::Sender<ReqOutcome>,
        link: mpsc::UnboundedSender<bool>,
    ) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            if !this.has_keys() {
                let _ = link.send(false);
                return;
            }
            let key = this.key.clone().expect("gate key");
            let secret = this.secret.clone().expect("gate secret");
            let pool = GateTradePool::spawn(this.ep.trade_ws.clone(), key, secret, link).await;
            while let Some(action) = trade_rx.recv().await {
                match action {
                    Action::Place(req) => {
                        let r = if pool.any_ready() {
                            pool.place(&req).await
                        } else {
                            this.place(&req).await
                        };
                        let _ = outcomes.send(map_result(req.coid, r));
                    }
                    Action::Cancel { coid, symbol, .. } => {
                        let r = if pool.any_ready() {
                            pool.cancel(&symbol, &coid).await
                        } else {
                            this.cancel(&symbol, &coid).await
                        };
                        let _ = outcomes.send(map_result(coid, r));
                    }
                    Action::Amend {
                        coid,
                        symbol,
                        px,
                        qty,
                        ..
                    } => {
                        let r = if pool.any_ready() {
                            pool.amend(&symbol, &coid, px, qty).await
                        } else {
                            this.amend(&symbol, &coid, px, qty).await
                        };
                        let _ = outcomes.send(map_result(coid, r));
                    }
                    Action::CancelAll { symbol, .. } => {
                        let r = if pool.any_ready() {
                            pool.cancel_all(&symbol).await
                        } else {
                            this.cancel_all(&symbol).await
                        };
                        if let Err(e) = r {
                            tracing::warn!(error = %e, "gate cancel all");
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    async fn place(&self, req: &PlaceReq) -> Result<()> {
        let size = match req.side {
            Side::Buy => req.qty.0,
            Side::Sell => -req.qty.0,
        };
        let tif = match req.tif {
            TimeInForce::PostOnly => "poc",
            TimeInForce::Ioc => "ioc",
            TimeInForce::Fok => "fok",
            TimeInForce::Gtc => "gtc",
        };
        let mut body = json!({
            "contract": native_symbol(VenueId::Gate, req.symbol.as_str()),
            "size": size,
            "price": req.px.0.normalize().to_string(),
            "tif": tif,
            "text": format!("t-{}", req.coid.as_str()),
            "reduce_only": req.reduce_only,
        });
        if req.order_type == crate::types::OrderType::Market {
            body["tif"] = json!("ioc");
            body["price"] = json!("0");
        }
        let _ = self
            .signed(
                reqwest::Method::POST,
                "/api/v4/futures/usdt/orders",
                "",
                Some(&body.to_string()),
            )
            .await?;
        Ok(())
    }

    async fn cancel(&self, _symbol: &SymbolId, coid: &ClientOrderId) -> Result<()> {
        let q = format!("text=t-{}", coid.as_str());
        // Gate cancel by text via listing then delete; try id-less cancel-all text filter
        let _ = self
            .signed(
                reqwest::Method::DELETE,
                "/api/v4/futures/usdt/orders",
                &format!("contract={}&text=t-{}", 
                    native_symbol(VenueId::Gate, _symbol.as_str()),
                    coid.as_str()),
                None,
            )
            .await;
        let _ = q;
        Ok(())
    }

    async fn amend(
        &self,
        symbol: &SymbolId,
        coid: &ClientOrderId,
        px: Px,
        qty: Qty,
    ) -> Result<()> {
        // cancel + place is handled by core if amend fails; try price amend via REST if we know id
        let orders = self.open_orders(symbol).await?;
        if let Some(o) = orders.iter().find(|o| o.coid.as_str() == coid.as_str() || o.coid.as_str().ends_with(coid.as_str())) {
            if let Some(id) = &o.exchange_id {
                let body = json!({
                    "price": px.0.normalize().to_string(),
                    "size": qty.0,
                });
                let path = format!("/api/v4/futures/usdt/orders/{id}");
                let _ = self
                    .signed(reqwest::Method::PUT, &path, "", Some(&body.to_string()))
                    .await?;
                return Ok(());
            }
        }
        anyhow::bail!("gate amend: order not found")
    }

    async fn cancel_all(&self, symbol: &SymbolId) -> Result<()> {
        let q = format!("contract={}", native_symbol(VenueId::Gate, symbol.as_str()));
        let _ = self
            .signed(
                reqwest::Method::DELETE,
                "/api/v4/futures/usdt/orders",
                &q,
                None,
            )
            .await?;
        Ok(())
    }

    /// Unified-account modes that can trade USDT perps. Classic is not auto-upgraded.
    async fn ensure_unified_account(&self) -> Result<()> {
        let text = self
            .signed(
                reqwest::Method::GET,
                "/api/v4/unified/unified_mode",
                "",
                None,
            )
            .await
            .context("gate GET /unified/unified_mode")?;
        let v: Value = serde_json::from_str(&text).context("gate unified_mode json")?;
        let mode = v["mode"].as_str().unwrap_or("");
        let usdt_futures = v
            .pointer("/settings/usdt_futures")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match mode {
            "multi_currency" | "portfolio" | "single_currency" => {
                tracing::info!(mode, usdt_futures, "gate unified account");
                if mode == "multi_currency" && !usdt_futures {
                    let body = json!({
                        "mode": "multi_currency",
                        "settings": { "usdt_futures": true }
                    });
                    self.signed(
                        reqwest::Method::PUT,
                        "/api/v4/unified/unified_mode",
                        "",
                        Some(&body.to_string()),
                    )
                    .await
                    .context("gate enable usdt_futures")?;
                }
            }
            "classic" | "" => {
                tracing::warn!(
                    mode,
                    "gate account is classic; adapter expects unified + one-way"
                );
            }
            other => tracing::warn!(mode = other, "gate unknown unified account mode"),
        }
        Ok(())
    }

    async fn ensure_single_position_mode(&self) -> Result<()> {
        let text = self
            .signed(
                reqwest::Method::GET,
                "/api/v4/futures/usdt/accounts",
                "",
                None,
            )
            .await
            .context("gate GET /futures/usdt/accounts")?;
        let v: Value = serde_json::from_str(&text).context("gate futures account json")?;
        if is_single_position_mode(&v) {
            tracing::info!("gate futures position_mode=single");
            return Ok(());
        }
        let current = current_position_mode(&v);
        tracing::info!(current, "gate switching futures position_mode to single");
        match self
            .signed(
                reqwest::Method::POST,
                "/api/v4/futures/usdt/set_position_mode",
                "position_mode=single",
                None,
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "gate set_position_mode failed; falling back to dual_mode=false"
                );
                self.signed(
                    reqwest::Method::POST,
                    "/api/v4/futures/usdt/dual_mode",
                    "dual_mode=false",
                    None,
                )
                .await
                .context("gate set one-way position mode")?;
                Ok(())
            }
        }
    }
}

#[async_trait::async_trait]
impl VenueApi for GateVenue {
    fn venue(&self) -> VenueId {
        VenueId::Gate
    }

    fn has_keys(&self) -> bool {
        self.key.as_ref().is_some_and(|s| !s.is_empty())
            && self.secret.as_ref().is_some_and(|s| !s.is_empty())
    }

    async fn list_instruments(&self) -> Result<Vec<Instrument>> {
        let url = format!("{}/api/v4/futures/usdt/contracts", self.ep.public_rest);
        let v: Value = get_json(&self.http, &url, &[]).await?;
        let mut out = Vec::new();
        for c in v.as_array().context("contracts")? {
            if c["in_delisting"].as_bool().unwrap_or(false) {
                continue;
            }
            let native = c["name"].as_str().unwrap_or("").to_string();
            if native.is_empty() {
                continue;
            }
            let ctype = c["type"].as_str().unwrap_or("direct");
            if ctype == "inverse" {
                continue;
            }
            let kind = match c["contract_type"].as_str().unwrap_or("") {
                "stocks" => ContractKind::Stock,
                "metals" | "indices" | "forex" | "commodities" => ContractKind::Other,
                _ => ContractKind::Crypto,
            };
            let tick = parse_dec(c["order_price_round"].as_str().unwrap_or("0.01"))?;
            let lot = c["order_size_min"].as_i64().unwrap_or(1);
            let quanto = parse_dec(c["quanto_multiplier"].as_str().unwrap_or("1"))?;
            let status = if c["in_trading"].as_bool().unwrap_or(true) {
                InstrumentStatus::Trading
            } else {
                InstrumentStatus::Halt
            };
            out.push(Instrument {
                venue: VenueId::Gate,
                symbol: SymbolId::new(&native),
                native_symbol: native,
                tick_size: tick,
                lot_size: Decimal::from(lot.max(1)),
                contract_size: if quanto.is_zero() { Decimal::ONE } else { quanto },
                min_qty: Decimal::from(lot.max(1)),
                min_notional: Decimal::ZERO,
                quote_ccy: "USDT".into(),
                maker_fee: parse_dec(c["maker_fee_rate"].as_str().unwrap_or("0.0002"))
                    .unwrap_or(Decimal::new(2, 4)),
                taker_fee: parse_dec(c["taker_fee_rate"].as_str().unwrap_or("0.0005"))
                    .unwrap_or(Decimal::new(5, 4)),
                kind,
                status,
                volume_24h: parse_dec(c["volume_24h_quote"].as_str().unwrap_or("0"))
                    .or_else(|_| parse_dec(&c["volume_24h_quote"].to_string()))
                    .unwrap_or(Decimal::ZERO),
            });
        }
        Ok(out)
    }

    async fn volumes(&self) -> Result<Vec<(SymbolId, Decimal)>> {
        let url = format!("{}/api/v4/futures/usdt/tickers", self.ep.public_rest);
        let v: Value = get_json(&self.http, &url, &[]).await?;
        let mut out = Vec::new();
        if let Some(arr) = v.as_array() {
            for t in arr {
                let name = t["contract"].as_str().unwrap_or_default();
                let q = t["volume_24h_quote"]
                    .as_str()
                    .or_else(|| t["volume_24h_settle"].as_str())
                    .unwrap_or("0");
                out.push((SymbolId::new(name), parse_dec(q).unwrap_or(Decimal::ZERO)));
            }
        }
        Ok(out)
    }

    async fn open_orders(&self, symbol: &SymbolId) -> Result<Vec<OrderRecord>> {
        if !self.has_keys() {
            return Ok(vec![]);
        }
        let contract = native_symbol(VenueId::Gate, symbol.as_str());
        let text = self
            .signed(
                reqwest::Method::GET,
                "/api/v4/futures/usdt/orders",
                &format!("status=open&contract={contract}"),
                None,
            )
            .await?;
        let v: Value = serde_json::from_str(&text)?;
        let mut out = Vec::new();
        for o in v.as_array().context("orders")? {
            let size = o.get("size").and_then(json_i64).unwrap_or(0);
            let left = o.get("left").and_then(json_i64).unwrap_or(size);
            let text_id = o["text"].as_str().unwrap_or("");
            let coid = text_id.strip_prefix("t-").unwrap_or(text_id);
            out.push(OrderRecord {
                coid: ClientOrderId(coid.into()),
                strategy_id: String::new(),
                venue: VenueId::Gate,
                symbol: symbol.clone(),
                side: if size >= 0 { Side::Buy } else { Side::Sell },
                purpose: String::new(),
                px: Px(o.get("price").and_then(json_dec).unwrap_or(Decimal::ZERO)),
                qty: Qty(Decimal::from(size.abs())),
                filled_qty: Qty(Decimal::from((size.abs() - left.abs()).max(0))),
                status: o["status"].as_str().unwrap_or("open").into(),
                exchange_id: o["id"].as_i64().map(|i| i.to_string()),
                reduce_only: o["is_reduce_only"].as_bool().unwrap_or(false),
                created_at: Ts::from_secs(o["create_time"].as_i64().unwrap_or(0)),
                updated_at: Ts::now_system(),
            });
        }
        Ok(out)
    }

    async fn positions(&self) -> Result<Vec<PositionSnapshot>> {
        if !self.has_keys() {
            return Ok(vec![]);
        }
        let text = self
            .signed(
                reqwest::Method::GET,
                "/api/v4/futures/usdt/positions",
                "holding=true",
                None,
            )
            .await?;
        let v: Value = serde_json::from_str(&text)?;
        let mut by_symbol: HashMap<SymbolId, PositionSnapshot> = HashMap::new();
        for p in v.as_array().context("positions")? {
            let Some(snap) = parse_gate_position(p) else {
                continue;
            };
            by_symbol
                .entry(snap.symbol.clone())
                .and_modify(|e| {
                    e.net_qty = Qty(e.net_qty.0 + snap.net_qty.0);
                    e.ts = snap.ts;
                })
                .or_insert(snap);
        }
        Ok(by_symbol
            .into_values()
            .filter(|p| !p.net_qty.0.is_zero())
            .collect())
    }

    async fn ensure_oneway(&self) -> Result<()> {
        if !self.has_keys() {
            return Ok(());
        }
        if let Err(e) = self.ensure_unified_account().await {
            tracing::warn!(error = %e, "gate unified account check failed");
        }
        self.ensure_single_position_mode().await
    }

    async fn query_order(
        &self,
        symbol: &SymbolId,
        coid: &ClientOrderId,
    ) -> Result<Option<ExecReport>> {
        let orders = self.open_orders(symbol).await?;
        Ok(orders.into_iter().find(|o| o.coid == *coid).map(|o| ExecReport {
            coid: o.coid,
            exchange_id: o.exchange_id,
            venue: VenueId::Gate,
            symbol: o.symbol,
            kind: ExecKind::Ack,
            side: o.side,
            px: Some(o.px),
            qty: Some(o.qty),
            filled_qty: o.filled_qty,
            last_px: None,
            last_qty: None,
            reason: None,
            ts: o.updated_at,
        }))
    }
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
    match v {
        Value::String(s) => parse_dec(s).ok(),
        // Gate sends numbers as JSON numbers on the private stream and as strings over REST.
        // serde_json holds them as f64, so round-trip through the shortest decimal form:
        // `Decimal::from_f64_retain` would keep the binary error (1770.22 becomes
        // 1770.2200000000000272848410527) and break tick-size and price comparisons.
        Value::Number(n) => parse_dec(&n.to_string()).ok(),
        _ => None,
    }
}

fn json_i64(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(n) = v.as_u64() {
        return i64::try_from(n).ok();
    }
    if let Some(s) = v.as_str() {
        return s.parse().ok();
    }
    v.as_f64().and_then(|n| n.is_finite().then_some(n as i64))
}

fn is_single_position_mode(account: &Value) -> bool {
    if let Some(mode) = account["position_mode"].as_str() {
        return mode == "single";
    }
    !account["in_dual_mode"].as_bool().unwrap_or(false)
}

fn current_position_mode(account: &Value) -> &str {
    if let Some(mode) = account["position_mode"].as_str() {
        return mode;
    }
    if account["in_dual_mode"].as_bool().unwrap_or(false) {
        "dual"
    } else {
        "single"
    }
}

fn parse_gate_position(p: &Value) -> Option<PositionSnapshot> {
    let size = json_i64(p.get("size")?)?;
    if size == 0 {
        return None;
    }
    let mode = p["mode"].as_str().unwrap_or("single");
    if mode != "single" {
        tracing::warn!(
            contract = p["contract"].as_str().unwrap_or(""),
            mode,
            size,
            "gate position is not one-way"
        );
    }
    Some(PositionSnapshot {
        venue: VenueId::Gate,
        symbol: SymbolId::new(p["contract"].as_str().unwrap_or_default()),
        net_qty: Qty(Decimal::from(size)),
        avg_px: p.get("entry_price").and_then(json_dec).map(Px),
        ts: Ts::now_system(),
    })
}

fn parse_book_ticker(text: &str) -> Option<BookUpdate> {
    let v: Value = serde_json::from_str(text).ok()?;
    if v["channel"].as_str() != Some("futures.book_ticker") {
        return None;
    }
    let r = v.get("result")?;
    if r.is_array() {
        return None;
    }
    let native = r.get("s")?.as_str()?;
    let bid = json_dec(r.get("b")?)?;
    let ask = json_dec(r.get("a")?)?;
    let bq = r.get("B").and_then(json_dec).unwrap_or(Decimal::ONE);
    let aq = r.get("A").and_then(json_dec).unwrap_or(Decimal::ONE);
    let exch = r
        .get("t")
        .and_then(|x| x.as_i64())
        .map(Ts::from_millis)
        .unwrap_or_else(Ts::now_system);
    Some(BookUpdate {
        book: TopOfBook {
            venue: VenueId::Gate,
            symbol: SymbolId::new(native),
            bid_px: Px(bid),
            bid_qty: Qty(bq),
            ask_px: Px(ask),
            ask_qty: Qty(aq),
            exchange_ts: exch,
            recv_ts: Ts::now_system(),
        },
        seq: r.get("u").and_then(|x| x.as_u64()),
    })
}

fn parse_private(text: &str) -> Vec<PrivateMsg> {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    let ch = v["channel"].as_str().unwrap_or("");
    match ch {
        "futures.orders" => {
            let r = &v["result"];
            let items = if r.is_array() {
                r.as_array().cloned().unwrap_or_default()
            } else {
                vec![r.clone()]
            };
            items.iter().filter_map(|o| parse_gate_order(o)).collect()
        }
        "futures.positions" => {
            let r = &v["result"];
            let items = if r.is_array() {
                r.as_array().cloned().unwrap_or_default()
            } else {
                vec![r.clone()]
            };
            items
                .iter()
                .filter_map(parse_gate_position)
                .map(PrivateMsg::Position)
                .collect()
        }
        _ => vec![],
    }
}

fn parse_gate_order(o: &Value) -> Option<PrivateMsg> {
    if o.get("id").is_none() && o.get("text").is_none() {
        return None;
    }
    let size = o.get("size").and_then(json_i64).unwrap_or(0);
    let left = o.get("left").and_then(json_i64).unwrap_or(size);
    let filled = (size.abs() - left.abs()).max(0);
    let status = o["status"].as_str().unwrap_or("");
    let finish = o["finish_as"].as_str().unwrap_or("");
    // A `finished` order is terminal whatever the reason, so it must never map to `Ack`:
    // that would leave the strategy believing a dead order is still working.
    let kind = match status {
        "finished" if left == 0 || finish == "filled" => ExecKind::Fill,
        "finished" => ExecKind::Canceled,
        _ if filled > 0 => ExecKind::PartialFill,
        _ => ExecKind::Ack,
    };
    let text = o["text"].as_str().unwrap_or("");
    let coid = text.strip_prefix("t-").unwrap_or(text);
    Some(PrivateMsg::Exec(ExecReport {
        coid: ClientOrderId(coid.into()),
        exchange_id: o["id"].as_i64().map(|i| i.to_string()),
        venue: VenueId::Gate,
        symbol: SymbolId::new(o["contract"].as_str().unwrap_or_default()),
        kind,
        side: if size >= 0 { Side::Buy } else { Side::Sell },
        px: o
            .get("price")
            .and_then(json_dec)
            .filter(|p| *p > Decimal::ZERO)
            .map(Px),
        qty: Some(Qty(Decimal::from(size.abs()))),
        filled_qty: Qty(Decimal::from(filled)),
        last_px: o
            .get("fill_price")
            .and_then(json_dec)
            .filter(|p| *p > Decimal::ZERO)
            .map(Px),
        last_qty: None,
        reason: (!finish.is_empty()).then(|| finish.to_string()),
        ts: Ts::now_system(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gate_book_ticker() {
        let raw = r#"{"time":1,"channel":"futures.book_ticker","event":"update","result":{"t":1,"u":2,"s":"AAPL_USDT","b":"10.1","B":"5","a":"10.2","A":"6"}}"#;
        let u = parse_book_ticker(raw).unwrap();
        assert_eq!(u.book.symbol.as_str(), "AAPLUSDT");
    }

    #[test]
    fn parses_oneway_position() {
        let raw = r#"{"contract":"AAPL_USDT","size":3,"entry_price":"10.5","mode":"single"}"#;
        let p = parse_gate_position(&serde_json::from_str(raw).unwrap()).unwrap();
        assert_eq!(p.symbol.as_str(), "AAPLUSDT");
        assert_eq!(p.net_qty.0, Decimal::from(3));
        assert_eq!(p.avg_px.unwrap().0.to_string(), "10.5");
    }

    #[test]
    fn parses_string_size_short() {
        let raw = r#"{"contract":"AAPL_USDT","size":"-2","entry_price":1,"mode":"single"}"#;
        let p = parse_gate_position(&serde_json::from_str(raw).unwrap()).unwrap();
        assert_eq!(p.net_qty.0, Decimal::from(-2));
    }

    #[test]
    fn skips_flat_position() {
        let raw = r#"{"contract":"AAPL_USDT","size":0,"mode":"single"}"#;
        assert!(parse_gate_position(&serde_json::from_str(raw).unwrap()).is_none());
    }

    #[test]
    fn reads_single_position_mode_from_account() {
        assert!(is_single_position_mode(&json!({"position_mode":"single"})));
        assert!(!is_single_position_mode(&json!({"position_mode":"dual"})));
        assert!(!is_single_position_mode(&json!({"position_mode":"dual_plus"})));
        assert!(is_single_position_mode(&json!({"in_dual_mode":false})));
        assert!(!is_single_position_mode(&json!({"in_dual_mode":true})));
        assert_eq!(current_position_mode(&json!({"position_mode":"dual"})), "dual");
    }

    /// The `futures.orders` push sends `price`/`fill_price` as JSON numbers while REST sends
    /// strings; reading only the string form silently yielded a price of 0, which made the
    /// strategy think every resting order was mispriced and requote it on every tick.
    #[test]
    fn parses_numeric_order_push_price() {
        let raw = r#"{"channel":"futures.orders","event":"update","result":[{"contract":"SNDK_USDT","id":324822126929229396,"text":"t-s1-grid-sell-2-9","size":-1,"left":-1,"status":"open","price":1770.22,"fill_price":0,"tif":"poc"}]}"#;
        let msgs = parse_private(raw);
        assert_eq!(msgs.len(), 1);
        let PrivateMsg::Exec(e) = &msgs[0] else {
            panic!("expected exec, got {:?}", msgs[0]);
        };
        assert_eq!(e.coid.as_str(), "s1-grid-sell-2-9");
        assert_eq!(e.kind, ExecKind::Ack);
        assert_eq!(e.side, Side::Sell);
        assert_eq!(e.px.unwrap().0.to_string(), "1770.22");
        assert!(e.last_px.is_none(), "unfilled order has no fill price");
    }

    #[test]
    fn json_numbers_keep_exact_decimals() {
        assert_eq!(json_dec(&json!(1770.22)).unwrap().to_string(), "1770.22");
        assert_eq!(json_dec(&json!("1770.22")).unwrap().to_string(), "1770.22");
        assert_eq!(json_dec(&json!(-3)).unwrap().to_string(), "-3");
        assert!(json_dec(&Value::Null).is_none());
    }

    #[test]
    fn finished_order_is_never_live() {
        let raw = r#"{"channel":"futures.orders","event":"update","result":{"contract":"SNDK_USDT","id":1,"text":"t-s1-grid-buy-0-1","size":2,"left":2,"status":"finished","finish_as":"poc","price":1762.5}}"#;
        let msgs = parse_private(raw);
        let PrivateMsg::Exec(e) = &msgs[0] else {
            panic!("expected exec");
        };
        assert_eq!(e.kind, ExecKind::Canceled);
        assert_eq!(e.reason.as_deref(), Some("poc"));
    }

    #[test]
    fn partial_fill_reports_cumulative_filled() {
        let raw = r#"{"channel":"futures.orders","event":"update","result":{"contract":"SNDK_USDT","id":1,"text":"t-s1-grid-sell-0-1","size":-5,"left":-2,"status":"open","price":1770.5,"fill_price":1770.5}}"#;
        let msgs = parse_private(raw);
        let PrivateMsg::Exec(e) = &msgs[0] else {
            panic!("expected exec");
        };
        assert_eq!(e.kind, ExecKind::PartialFill);
        assert_eq!(e.filled_qty.0, Decimal::from(3));
        assert_eq!(e.last_px.unwrap().0.to_string(), "1770.5");
    }

    #[test]
    fn parses_private_oneway_position() {
        let raw = r#"{"channel":"futures.positions","event":"update","result":{"contract":"AAPL_USDT","size":1,"entry_price":"10","mode":"single"}}"#;
        let msgs = parse_private(raw);
        assert_eq!(msgs.len(), 1);
        match &msgs[0] {
            PrivateMsg::Position(p) => {
                assert_eq!(p.symbol.as_str(), "AAPLUSDT");
                assert_eq!(p.net_qty.0, Decimal::from(1));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
