use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use reqwest::Client;
use sha2::{Digest, Sha256, Sha512};

pub fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent("stock_mm/0.1")
        .timeout(std::time::Duration::from_secs(15))
        .build()?)
}

pub fn hmac_sha256_hex(secret: &str, msg: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(msg.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

pub fn hmac_sha512_hex(secret: &str, msg: &str) -> String {
    let mut mac = Hmac::<Sha512>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(msg.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

pub fn sha512_hex(msg: &str) -> String {
    let mut h = Sha512::new();
    h.update(msg.as_bytes());
    hex::encode(h.finalize())
}

pub async fn get_json<T: serde::de::DeserializeOwned>(
    client: &Client,
    url: &str,
    headers: &[(&str, String)],
) -> Result<T> {
    let mut req = client.get(url);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let res = req.send().await.with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("GET {url} -> {status}: {text}");
    }
    serde_json::from_str(&text).with_context(|| format!("decode {url}: {text}"))
}

pub async fn send_signed(
    client: &Client,
    method: reqwest::Method,
    url: &str,
    headers: Vec<(String, String)>,
    body: Option<String>,
) -> Result<String> {
    let mut req = client.request(method, url);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    if let Some(b) = body {
        req = req.header("Content-Type", "application/json").body(b);
    }
    let res = req.send().await.with_context(|| format!("request {url}"))?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("{url} -> {status}: {text}");
    }
    Ok(text)
}
