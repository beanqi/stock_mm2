use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

/// Reconnecting text WebSocket. Incoming frames go to `incoming`.
pub fn spawn_text_ws(
    name: String,
    url: String,
    incoming: mpsc::UnboundedSender<String>,
    link: Option<mpsc::UnboundedSender<bool>>,
) -> mpsc::UnboundedSender<String> {
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            info!(%name, %url, "ws connecting");
            match connect_async(&url).await {
                Ok((ws, _)) => {
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

pub fn now_secs() -> u64 {
    crate::types::Ts::now_system().millis() as u64 / 1000
}

pub fn now_millis() -> u64 {
    crate::types::Ts::now_system().millis() as u64
}

pub fn parse_dec(s: &str) -> Result<rust_decimal::Decimal> {
    Ok(s.parse()?)
}
