use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use rust_decimal::prelude::ToPrimitive;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::types::{ClientOrderId, PlaceReq, Px, Qty, Side, SymbolId, TimeInForce};

use super::rest::hmac_sha512_hex;
use super::session::{connect_text_ws, now_millis, now_secs, resolve_ws_addrs};
use super::{VenueId, native_symbol};

const TRADE_LANES: usize = 6;
const RPC_TIMEOUT: Duration = Duration::from_secs(8);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(8);

static REQ_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct GateTradePool {
    lanes: Arc<Vec<Lane>>,
    rr: Arc<AtomicUsize>,
}

struct Lane {
    name: String,
    ready: Arc<AtomicBool>,
    tx: mpsc::UnboundedSender<LaneReq>,
}

struct LaneReq {
    req_id: String,
    body: String,
    reply: oneshot::Sender<Result<Value>>,
}

struct Pending {
    reply: oneshot::Sender<Result<Value>>,
    at: Instant,
}

impl GateTradePool {
    pub async fn spawn(
        url: String,
        key: String,
        secret: String,
        link: mpsc::UnboundedSender<bool>,
    ) -> Self {
        let addrs = resolve_trade_addrs(&url).await;
        if addrs.is_empty() {
            info!(%url, "gate trade dns empty; falling back to hostname");
        } else {
            let ips: Vec<String> = addrs.iter().map(|a| a.ip().to_string()).collect();
            info!(n = ips.len(), ?ips, "gate trade dns");
        }
        let ready_n = Arc::new(AtomicUsize::new(0));
        let mut lanes = Vec::new();
        let pins: Vec<Option<SocketAddr>> = if addrs.is_empty() {
            vec![None]
        } else {
            addrs.into_iter().map(Some).collect()
        };
        for (i, pin) in pins.into_iter().enumerate() {
            let label = pin
                .map(|a| a.ip().to_string())
                .unwrap_or_else(|| "host".into());
            let name = format!("gate-trade-{i}-{label}");
            let ready = Arc::new(AtomicBool::new(false));
            let (tx, rx) = mpsc::unbounded_channel();
            lanes.push(Lane {
                name: name.clone(),
                ready: Arc::clone(&ready),
                tx,
            });
            tokio::spawn(run_lane(
                name,
                url.clone(),
                pin,
                key.clone(),
                secret.clone(),
                ready,
                Arc::clone(&ready_n),
                link.clone(),
                rx,
            ));
        }
        let pool = Self {
            lanes: Arc::new(lanes),
            rr: Arc::new(AtomicUsize::new(0)),
        };
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if pool.any_ready() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        pool
    }

    pub fn any_ready(&self) -> bool {
        self.lanes.iter().any(|l| l.ready.load(Ordering::Relaxed))
    }

    pub async fn place(&self, req: &PlaceReq) -> Result<()> {
        let _ = self
            .rpc("futures.order_place", place_param(req)?)
            .await?;
        Ok(())
    }

    pub async fn cancel(&self, _symbol: &SymbolId, coid: &ClientOrderId) -> Result<()> {
        let _ = self
            .rpc(
                "futures.order_cancel",
                json!({ "order_id": format!("t-{}", coid.as_str()) }),
            )
            .await?;
        Ok(())
    }

    pub async fn amend(
        &self,
        _symbol: &SymbolId,
        coid: &ClientOrderId,
        px: Px,
        qty: Qty,
    ) -> Result<()> {
        let size = qty_to_i64(qty)?;
        let _ = self
            .rpc(
                "futures.order_amend",
                json!({
                    "order_id": format!("t-{}", coid.as_str()),
                    "price": px.0.normalize().to_string(),
                    "size": size,
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn cancel_all(&self, symbol: &SymbolId) -> Result<()> {
        let _ = self
            .rpc(
                "futures.order_cancel_cp",
                json!({ "contract": native_symbol(VenueId::Gate, symbol.as_str()) }),
            )
            .await?;
        Ok(())
    }

    async fn rpc(&self, channel: &str, req_param: Value) -> Result<Value> {
        let n = self.lanes.len();
        if n == 0 {
            bail!("disconnect: no gate trade lane");
        }
        let start = self.rr.fetch_add(1, Ordering::Relaxed);
        let mut last = None;
        for i in 0..n {
            let lane = &self.lanes[(start + i) % n];
            if !lane.ready.load(Ordering::Relaxed) {
                continue;
            }
            match send_rpc(lane, channel, &req_param).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let msg = e.to_string();
                    if is_safe_retry(&msg) {
                        last = Some(e);
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("disconnect: no ready gate trade ws")))
    }
}

async fn send_rpc(lane: &Lane, channel: &str, req_param: &Value) -> Result<Value> {
    let req_id = next_req_id();
    tracing::debug!(lane = %lane.name, channel, %req_id, "gate trade rpc");
    let body = api_request(channel, &req_id, Some(req_param));
    let (reply, rx) = oneshot::channel();
    lane.tx
        .send(LaneReq {
            req_id,
            body,
            reply,
        })
        .map_err(|_| anyhow::anyhow!("not_sent"))?;
    match timeout(RPC_TIMEOUT, rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => bail!("disconnect"),
        Err(_) => bail!("timeout"),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_lane(
    name: String,
    url: String,
    pin: Option<SocketAddr>,
    key: String,
    secret: String,
    ready: Arc<AtomicBool>,
    ready_n: Arc<AtomicUsize>,
    link: mpsc::UnboundedSender<bool>,
    mut cmd_rx: mpsc::UnboundedReceiver<LaneReq>,
) {
    let mut backoff = 1u64;
    loop {
        info!(%name, ?pin, "gate trade connecting");
        match connect_text_ws(&url, pin).await {
            Ok(ws) => {
                backoff = 1;
                let (mut sink, mut stream) = ws.split();
                let login_id = next_req_id();
                let login = login_request(&key, &secret, &login_id);
                if sink.send(Message::Text(login.into())).await.is_err() {
                    warn!(%name, "gate trade login send failed");
                } else if wait_login(&name, &mut stream, &mut sink, &login_id).await {
                    set_ready(&name, &ready, &ready_n, &link, true);
                    let mut pending: HashMap<String, Pending> = HashMap::new();
                    loop {
                        tokio::select! {
                            cmd = cmd_rx.recv() => {
                                let Some(cmd) = cmd else { return; };
                                purge_pending(&mut pending);
                                pending.insert(cmd.req_id, Pending { reply: cmd.reply, at: Instant::now() });
                                if sink.send(Message::Text(cmd.body.into())).await.is_err() {
                                    break;
                                }
                            }
                            frame = stream.next() => {
                                match frame {
                                    Some(Ok(Message::Text(t))) => {
                                        if !dispatch_frame(&name, &t, &mut pending) {
                                            // keep reading
                                        }
                                    }
                                    Some(Ok(Message::Ping(p))) => {
                                        if sink.send(Message::Pong(p)).await.is_err() {
                                            break;
                                        }
                                    }
                                    Some(Ok(Message::Pong(_))) => {}
                                    Some(Ok(Message::Close(_))) | None => break,
                                    Some(Err(e)) => {
                                        warn!(%name, error = %e, "gate trade read error");
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    fail_pending(pending, "disconnect");
                } else {
                    warn!(%name, "gate trade login failed");
                }
            }
            Err(e) => warn!(%name, error = %e, "gate trade connect failed"),
        }
        set_ready(&name, &ready, &ready_n, &link, false);
        let jitter = (rand::random::<u64>() % 400) + 100;
        tokio::time::sleep(Duration::from_millis(
            backoff.saturating_mul(400) + jitter,
        ))
        .await;
        backoff = (backoff * 2).min(32);
    }
}

async fn wait_login<S, R>(
    name: &str,
    stream: &mut S,
    sink: &mut R,
    login_id: &str,
) -> bool
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
    R: SinkExt<Message> + Unpin,
{
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        match timeout(left, stream.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                let Some(frame) = parse_api_frame(&t) else {
                    continue;
                };
                if frame.ack {
                    continue;
                }
                if frame.request_id.as_deref() != Some(login_id) {
                    continue;
                }
                if let Some(err) = frame.error {
                    warn!(%name, error = %err, "gate trade login rejected");
                    return false;
                }
                info!(%name, "gate trade login ok");
                return true;
            }
            Ok(Some(Ok(Message::Ping(p)))) => {
                if sink.send(Message::Pong(p)).await.is_err() {
                    return false;
                }
            }
            Ok(Some(Ok(Message::Pong(_)))) => {}
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => return false,
            Ok(Some(Err(e))) => {
                warn!(%name, error = %e, "gate trade login read error");
                return false;
            }
            Ok(Some(Ok(_))) => {}
            Err(_) => return false,
        }
    }
}

fn dispatch_frame(name: &str, text: &str, pending: &mut HashMap<String, Pending>) -> bool {
    let Some(frame) = parse_api_frame(text) else {
        return false;
    };
    if frame.ack {
        return false;
    }
    let Some(id) = frame.request_id.clone() else {
        return false;
    };
    if let Some(remain) = frame.remain {
        if remain <= 5 {
            tracing::debug!(%name, remain, "gate trade ratelimit remain");
        }
    }
    let Some(p) = pending.remove(&id) else {
        return false;
    };
    let r = match frame.error {
        Some(e) => Err(anyhow::anyhow!(e)),
        None => Ok(frame.result.unwrap_or(Value::Null)),
    };
    let _ = p.reply.send(r);
    true
}

fn fail_pending(pending: HashMap<String, Pending>, why: &str) {
    for (_, p) in pending {
        let _ = p.reply.send(Err(anyhow::anyhow!("{why}")));
    }
}

fn purge_pending(pending: &mut HashMap<String, Pending>) {
    let now = Instant::now();
    let stale: Vec<String> = pending
        .iter()
        .filter(|(_, p)| now.duration_since(p.at) > RPC_TIMEOUT + Duration::from_secs(2))
        .map(|(id, _)| id.clone())
        .collect();
    for id in stale {
        if let Some(p) = pending.remove(&id) {
            let _ = p.reply.send(Err(anyhow::anyhow!("timeout")));
        }
    }
}

fn set_ready(
    name: &str,
    ready: &AtomicBool,
    ready_n: &AtomicUsize,
    link: &mpsc::UnboundedSender<bool>,
    up: bool,
) {
    if ready.swap(up, Ordering::SeqCst) == up {
        return;
    }
    if up {
        if ready_n.fetch_add(1, Ordering::SeqCst) == 0 {
            info!(%name, "gate trade link up");
            let _ = link.send(true);
        }
    } else if ready_n.fetch_sub(1, Ordering::SeqCst) == 1 {
        warn!(%name, "gate trade link down");
        let _ = link.send(false);
    }
}

async fn resolve_trade_addrs(url: &str) -> Vec<SocketAddr> {
    match resolve_ws_addrs(url, TRADE_LANES).await {
        Ok(addrs) => addrs,
        Err(e) => {
            warn!(error = %e, "gate trade dns failed");
            Vec::new()
        }
    }
}

fn is_safe_retry(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("not_sent")
        || lower.contains("too_many")
        || lower.contains("rate limit")
        || lower.contains("ratelimit")
        || msg.contains("311")
        || msg.contains("312")
}

fn next_req_id() -> String {
    format!(
        "{}-{}",
        now_millis(),
        REQ_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn login_signature(secret: &str, channel: &str, req_param: &str, ts: u64) -> String {
    hmac_sha512_hex(secret, &format!("api\n{channel}\n{req_param}\n{ts}"))
}

fn login_request(key: &str, secret: &str, req_id: &str) -> String {
    let ts = now_secs();
    let sign = login_signature(secret, "futures.login", "", ts);
    json!({
        "time": ts,
        "channel": "futures.login",
        "event": "api",
        "payload": {
            "api_key": key,
            "signature": sign,
            "timestamp": ts.to_string(),
            "req_id": req_id,
        }
    })
    .to_string()
}

fn api_request(channel: &str, req_id: &str, req_param: Option<&Value>) -> String {
    let mut payload = json!({ "req_id": req_id });
    if let Some(p) = req_param {
        payload["req_param"] = p.clone();
    }
    json!({
        "time": now_secs(),
        "channel": channel,
        "event": "api",
        "payload": payload,
    })
    .to_string()
}

fn place_param(req: &PlaceReq) -> Result<Value> {
    let mut size = qty_to_i64(req.qty)?;
    if req.side == Side::Sell {
        size = -size;
    }
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
    Ok(body)
}

fn qty_to_i64(qty: Qty) -> Result<i64> {
    qty.0
        .normalize()
        .to_i64()
        .context("gate size must be an integer contract count")
}

#[derive(Debug)]
struct ApiFrame {
    request_id: Option<String>,
    ack: bool,
    result: Option<Value>,
    error: Option<String>,
    remain: Option<i64>,
}

fn parse_api_frame(text: &str) -> Option<ApiFrame> {
    let v: Value = serde_json::from_str(text).ok()?;
    let header = v.get("header")?;
    let request_id = v
        .get("request_id")
        .and_then(|x| x.as_str())
        .map(ToOwned::to_owned);
    let ack = v.get("ack").and_then(Value::as_bool).unwrap_or(false);
    let status = header.get("status").and_then(|s| {
        s.as_str()
            .map(ToOwned::to_owned)
            .or_else(|| s.as_u64().map(|n| n.to_string()))
    });
    let data = v.get("data");
    let errs = data.and_then(|d| d.get("errs"));
    let mut error = errs.and_then(|e| {
        if e.is_null() {
            return None;
        }
        let label = e.get("label").and_then(Value::as_str).unwrap_or("error");
        let msg = e.get("message").and_then(Value::as_str).unwrap_or("");
        Some(if msg.is_empty() {
            label.to_string()
        } else {
            format!("{label}: {msg}")
        })
    });
    if error.is_none() {
        if let Some(s) = status.as_deref() {
            if s != "200" {
                error = Some(format!("status {s}"));
            }
        }
    }
    let remain = header
        .get("x_gate_ratelimit_requests_remain")
        .and_then(Value::as_i64);
    Some(ApiFrame {
        request_id,
        ack,
        result: data.and_then(|d| d.get("result")).cloned(),
        error,
        remain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ClientOrderId, OrderType, SymbolId};
    use rust_decimal::Decimal;

    #[test]
    fn login_sign_is_stable() {
        let sign = login_signature("secret", "futures.login", "", 1681984544);
        assert_eq!(sign.len(), 128);
        assert_eq!(
            sign,
            login_signature("secret", "futures.login", "", 1681984544)
        );
        assert_ne!(
            sign,
            login_signature("secret", "futures.order_place", "", 1681984544)
        );
    }

    #[test]
    fn parses_place_ack_and_result() {
        let ack = r#"{"request_id":"r1","ack":true,"header":{"status":"200","channel":"futures.order_place"},"data":{"result":{"req_id":"r1"}}}"#;
        let f = parse_api_frame(ack).unwrap();
        assert!(f.ack);
        assert_eq!(f.request_id.as_deref(), Some("r1"));

        let ok = r#"{"request_id":"r1","ack":false,"header":{"status":"200","channel":"futures.order_place","x_gate_ratelimit_requests_remain":99},"data":{"result":{"id":1,"text":"t-abc"}}}"#;
        let f = parse_api_frame(ok).unwrap();
        assert!(!f.ack);
        assert!(f.error.is_none());
        assert_eq!(f.result.unwrap()["id"], 1);
        assert_eq!(f.remain, Some(99));
    }

    #[test]
    fn parses_rate_limit_error() {
        let raw = r#"{"request_id":"r2","header":{"status":"429","channel":"futures.order_place"},"data":{"errs":{"label":"TOO_MANY_REQUESTS","message":"rate limit"}}}"#;
        let f = parse_api_frame(raw).unwrap();
        assert!(!f.ack);
        assert_eq!(f.error.as_deref(), Some("TOO_MANY_REQUESTS: rate limit"));
    }

    #[test]
    fn place_param_signs_sell_and_tags_text() {
        let req = PlaceReq {
            coid: ClientOrderId("s1-grid-sell-0-1".into()),
            venue: VenueId::Gate,
            symbol: SymbolId::new("AAPLUSDT"),
            side: Side::Sell,
            px: Px(Decimal::new(101, 1)),
            qty: Qty(Decimal::from(3)),
            tif: TimeInForce::PostOnly,
            reduce_only: false,
            order_type: OrderType::Limit,
        };
        let v = place_param(&req).unwrap();
        assert_eq!(v["contract"], "AAPL_USDT");
        assert_eq!(v["size"], -3);
        assert_eq!(v["tif"], "poc");
        assert_eq!(v["text"], "t-s1-grid-sell-0-1");
        assert_eq!(v["price"], "10.1");
    }

    #[test]
    fn retries_only_unsent_or_rate_limit() {
        assert!(is_safe_retry("not_sent"));
        assert!(is_safe_retry("TOO_MANY_REQUESTS: rate limit"));
        assert!(is_safe_retry("311: Futures rate limit"));
        assert!(!is_safe_retry("timeout"));
        assert!(!is_safe_retry("disconnect"));
        assert!(!is_safe_retry("ORDER_NOT_FOUND"));
    }

    #[test]
    fn unique_helper_still_caps() {
        use std::net::{IpAddr, Ipv4Addr};

        use crate::exchange::session::{unique_addrs, ws_host_port};

        let addrs: Vec<_> = (1..=10)
            .map(|i| SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, i)), 443))
            .collect();
        assert_eq!(unique_addrs(addrs, TRADE_LANES).len(), 6);
        let _ = ws_host_port("wss://fx-ws.gateio.ws/v4/ws/usdt").unwrap();
    }
}
