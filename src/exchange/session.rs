use std::collections::HashSet;
use std::net::SocketAddr;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{
    client_async_tls_with_config, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use tracing::{info, warn};

/// Reconnecting text WebSocket. Incoming frames go to `incoming`.
pub fn spawn_text_ws(
    name: String,
    url: String,
    incoming: mpsc::UnboundedSender<String>,
    link: Option<mpsc::UnboundedSender<bool>>,
) -> mpsc::UnboundedSender<String> {
    spawn_text_ws_to(name, url, None, incoming, link)
}

/// Like [`spawn_text_ws`], but dials `pin` directly while keeping the URL host for TLS SNI / Host.
pub fn spawn_text_ws_to(
    name: String,
    url: String,
    pin: Option<SocketAddr>,
    incoming: mpsc::UnboundedSender<String>,
    link: Option<mpsc::UnboundedSender<bool>>,
) -> mpsc::UnboundedSender<String> {
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            info!(%name, %url, ?pin, "ws connecting");
            match connect_text_ws(&url, pin).await {
                Ok(ws) => {
                    backoff = 1;
                    if let Some(l) = &link {
                        let _ = l.send(true);
                    }
                    let (mut sink, mut stream) = ws.split();
                    loop {
                        tokio::select! {
                            msg = out_rx.recv() => {
                                let Some(msg) = msg else { return; };
                                if sink.send(Message::Text(msg.into())).await.is_err() {
                                    break;
                                }
                            }
                            frame = stream.next() => {
                                match frame {
                                    Some(Ok(Message::Text(t))) => {
                                        if incoming.send(t.to_string()).is_err() {
                                            return;
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
                                        warn!(%name, error = %e, "ws read error");
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    if let Some(l) = &link {
                        let _ = l.send(false);
                    }
                }
                Err(e) => {
                    warn!(%name, error = %e, "ws connect failed");
                    if let Some(l) = &link {
                        let _ = l.send(false);
                    }
                }
            }
            let jitter = (rand::random::<u64>() % 400) + 100;
            tokio::time::sleep(std::time::Duration::from_millis(
                backoff.saturating_mul(400) + jitter,
            ))
            .await;
            backoff = (backoff * 2).min(32);
        }
    });
    out_tx
}

pub async fn connect_text_ws(
    url: &str,
    pin: Option<SocketAddr>,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
> {
    if let Some(addr) = pin {
        let req = url
            .into_client_request()
            .map_err(|e| anyhow::anyhow!("ws request {url}: {e}"))?;
        let stream = TcpStream::connect(addr)
            .await
            .with_context(|| format!("tcp {addr}"))?;
        let _ = stream.set_nodelay(true);
        let (ws, _) = client_async_tls_with_config(req, stream, None, None)
            .await
            .with_context(|| format!("wss {url} via {addr}"))?;
        Ok(ws)
    } else {
        let (ws, _) = connect_async(url)
            .await
            .with_context(|| format!("wss {url}"))?;
        Ok(ws)
    }
}

/// Resolve unique IPs for a `wss://` URL, capped at `limit`.
pub async fn resolve_ws_addrs(url: &str, limit: usize) -> Result<Vec<SocketAddr>> {
    let (host, port) = ws_host_port(url)?;
    let looked = tokio::net::lookup_host((host.as_str(), port))
        .await
        .with_context(|| format!("dns {host}:{port}"))?;
    Ok(unique_addrs(looked, limit))
}

pub fn ws_host_port(url: &str) -> Result<(String, u16)> {
    let u = url::Url::parse(url).with_context(|| format!("parse {url}"))?;
    let host = u.host_str().context("ws host")?.to_string();
    let port = u.port_or_known_default().unwrap_or(443);
    Ok((host, port))
}

pub fn unique_addrs(addrs: impl IntoIterator<Item = SocketAddr>, limit: usize) -> Vec<SocketAddr> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for addr in addrs {
        if !seen.insert(addr.ip()) {
            continue;
        }
        out.push(addr);
        if out.len() >= limit {
            break;
        }
    }
    out
}

pub fn now_secs() -> u64 {
    crate::types::Ts::now_system().millis() as u64 / 1000
}

pub fn now_millis() -> u64 {
    crate::types::Ts::now_system().millis() as u64
}

pub fn parse_dec(s: &str) -> Result<rust_decimal::Decimal> {
    Ok(s.parse()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn unique_addrs_caps_and_dedups() {
        let a = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 443);
        let b = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 2)), 443);
        let a2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 8443);
        let got = unique_addrs([a, a2, b], 6);
        assert_eq!(got, vec![a, b]);
    }

    #[test]
    fn unique_addrs_takes_first_n() {
        let addrs: Vec<_> = (1..=8)
            .map(|i| SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, i)), 443))
            .collect();
        let got = unique_addrs(addrs, 6);
        assert_eq!(got.len(), 6);
        assert_eq!(got[5].ip(), IpAddr::V4(Ipv4Addr::new(1, 1, 1, 6)));
    }

    #[test]
    fn parses_wss_host_port() {
        let (host, port) = ws_host_port("wss://fx-ws.gateio.ws/v4/ws/usdt").unwrap();
        assert_eq!(host, "fx-ws.gateio.ws");
        assert_eq!(port, 443);
    }
}
