use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::http::HeaderMap;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::Sha256;
use uuid::Uuid;

const COOKIE_NAME: &str = "mm_session";
const TICKET_TTL_SECS: u64 = 5 * 60;
const SESSION_TTL_SECS: u64 = 12 * 60 * 60;
const MAX_FAILS: u32 = 8;
const LOCK_SECS: u64 = 60;
type HmacSha256 = Hmac<Sha256>;
type HmacSha1 = Hmac<Sha1>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthStatus {
    pub enabled: bool,
    pub enrolled: bool,
}

#[derive(Clone, Debug)]
pub struct LoginOutcome {
    pub ticket: String,
    pub enroll: bool,
    pub otpauth_url: Option<String>,
    pub secret: Option<String>,
    pub qr_svg: Option<String>,
}

#[derive(Debug, Deserialize, Default, Serialize)]
struct Persist {
    #[serde(default)]
    password_key: String,
    #[serde(default)]
    password_hash: String,
    #[serde(default)]
    totp_secret: String,
    #[serde(default)]
    totp_enrolled: bool,
    #[serde(default)]
    session_secret: String,
}

struct Pending {
    exp: u64,
    enroll: bool,
}

struct FailState {
    fails: u32,
    locked_until: u64,
}

pub struct Auth {
    pub enabled: bool,
    pub username: String,
    password_key: Vec<u8>,
    password_hash: String,
    totp_secret: String,
    totp_enrolled: Mutex<bool>,
    session_secret: Vec<u8>,
    cookie_secure: bool,
    persist_path: PathBuf,
    bootstrap_path: PathBuf,
    pending: Mutex<HashMap<String, Pending>>,
    fails: Mutex<HashMap<String, FailState>>,
}

impl Auth {
    pub fn load(database_url: &str) -> Result<Self> {
        if env_flag("AUTH_DISABLED") {
            tracing::warn!("console auth disabled (AUTH_DISABLED)");
            return Ok(Self::disabled());
        }

        let persist_path = persist_path(database_url);
        if let Some(parent) = persist_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut persist = read_persist(&persist_path)?;

        let password_key = decode_hex_or_new(&mut persist.password_key, 32)?;
        let session_secret = if let Ok(raw) = std::env::var("AUTH_SESSION_SECRET") {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                decode_hex_or_new(&mut persist.session_secret, 32)?
            } else if let Ok(bytes) = hex::decode(trimmed) {
                persist.session_secret = trimmed.to_string();
                bytes
            } else {
                trimmed.as_bytes().to_vec()
            }
        } else {
            decode_hex_or_new(&mut persist.session_secret, 32)?
        };

        let username = std::env::var("AUTH_USERNAME")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "admin".into());

        let env_password = std::env::var("AUTH_PASSWORD")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let bootstrap_path = persist_path
            .parent()
            .unwrap_or(Path::new("data"))
            .join("initial-password.txt");

        let password_hash = if let Some(password) = env_password {
            let _ = std::fs::remove_file(&bootstrap_path);
            hmac_hex(&password_key, password.as_bytes())
        } else if !persist.password_hash.is_empty() {
            persist.password_hash.clone()
        } else {
            let password = generate_password();
            let hash = hmac_hex(&password_key, password.as_bytes());
            persist.password_hash = hash.clone();
            write_bootstrap(&bootstrap_path, &username, &password)?;
            tracing::warn!(
                username = %username,
                path = %bootstrap_path.display(),
                "generated console password; also written to data/initial-password.txt"
            );
            tracing::warn!(%password, "initial console password (save it, then enroll TOTP)");
            hash
        };

        let env_totp = std::env::var("AUTH_TOTP_SECRET")
            .ok()
            .map(|s| s.trim().replace(' ', "").to_ascii_uppercase())
            .filter(|s| !s.is_empty());

        let reset_totp = env_flag("AUTH_RESET_TOTP");
        let (totp_secret, totp_enrolled) = if let Some(secret) = env_totp {
            decode_totp_secret(&secret).context("AUTH_TOTP_SECRET")?;
            persist.totp_secret = secret.clone();
            persist.totp_enrolled = true;
            (secret, true)
        } else if !persist.totp_secret.is_empty() && !reset_totp {
            decode_totp_secret(&persist.totp_secret).context("stored TOTP secret")?;
            (persist.totp_secret.clone(), persist.totp_enrolled)
        } else {
            let secret = generate_totp_secret();
            persist.totp_secret = secret.clone();
            persist.totp_enrolled = false;
            (secret, false)
        };

        persist.password_key = hex::encode(&password_key);
        if std::env::var("AUTH_PASSWORD")
            .ok()
            .is_some_and(|s| !s.trim().is_empty())
        {
            persist.password_hash.clear();
        }
        persist.session_secret = hex::encode(&session_secret);
        persist.totp_secret.clone_from(&totp_secret);
        persist.totp_enrolled = totp_enrolled;
        write_persist(&persist_path, &persist)?;

        tracing::info!(
            username = %username,
            enrolled = totp_enrolled,
            "console password + TOTP login enabled"
        );

        Ok(Self {
            enabled: true,
            username,
            password_key,
            password_hash,
            totp_secret,
            totp_enrolled: Mutex::new(totp_enrolled),
            session_secret,
            cookie_secure: env_flag("AUTH_COOKIE_SECURE"),
            persist_path,
            bootstrap_path,
            pending: Mutex::new(HashMap::new()),
            fails: Mutex::new(HashMap::new()),
        })
    }

    fn disabled() -> Self {
        Self {
            enabled: false,
            username: "local".into(),
            password_key: vec![0; 32],
            password_hash: String::new(),
            totp_secret: String::new(),
            totp_enrolled: Mutex::new(true),
            session_secret: vec![0; 32],
            cookie_secure: false,
            persist_path: PathBuf::from("data/auth.json"),
            bootstrap_path: PathBuf::from("data/initial-password.txt"),
            pending: Mutex::new(HashMap::new()),
            fails: Mutex::new(HashMap::new()),
        }
    }

    pub fn status(&self) -> AuthStatus {
        AuthStatus {
            enabled: self.enabled,
            enrolled: *self.totp_enrolled.lock(),
        }
    }

    pub fn session_user(&self, headers: &HeaderMap) -> Option<String> {
        if !self.enabled {
            return Some(self.username.clone());
        }
        let token = cookie_value(headers, COOKIE_NAME)?;
        self.verify_session(&token)
    }

    pub fn session_ok(&self, headers: &HeaderMap) -> bool {
        self.session_user(headers).is_some()
    }

    pub fn login(&self, username: &str, password: &str) -> Result<LoginOutcome, AuthError> {
        if !self.enabled {
            return Err(AuthError::Disabled);
        }
        self.check_lock(username)?;
        if !ct_eq(username.as_bytes(), self.username.as_bytes()) || !self.password_matches(password)
        {
            self.record_fail(username);
            return Err(AuthError::Denied);
        }
        self.clear_fails(username);
        let enroll = !*self.totp_enrolled.lock();
        let ticket = Uuid::new_v4().to_string();
        let now = now_secs();
        {
            let mut pending = self.pending.lock();
            pending.retain(|_, p| p.exp > now);
            pending.insert(
                ticket.clone(),
                Pending {
                    exp: now + TICKET_TTL_SECS,
                    enroll,
                },
            );
        }
        if enroll {
            let otpauth_url = otpauth_url(&self.username, &self.totp_secret);
            let qr_svg = qr_svg(&otpauth_url).ok();
            Ok(LoginOutcome {
                ticket,
                enroll: true,
                otpauth_url: Some(otpauth_url),
                secret: Some(self.totp_secret.clone()),
                qr_svg,
            })
        } else {
            Ok(LoginOutcome {
                ticket,
                enroll: false,
                otpauth_url: None,
                secret: None,
                qr_svg: None,
            })
        }
    }

    pub fn verify(&self, ticket: &str, code: &str) -> Result<String, AuthError> {
        if !self.enabled {
            return Err(AuthError::Disabled);
        }
        let now = now_secs();
        let pending = {
            let mut pending = self.pending.lock();
            pending.retain(|_, p| p.exp > now);
            pending.remove(ticket).ok_or(AuthError::Denied)?
        };
        if pending.exp <= now {
            return Err(AuthError::Denied);
        }
        let code = code.trim();
        if !totp_accepts(&self.totp_secret, code, now) {
            self.record_fail(&self.username);
            return Err(AuthError::Denied);
        }
        self.clear_fails(&self.username);
        if pending.enroll {
            *self.totp_enrolled.lock() = true;
            if let Err(e) = self.persist_enrolled() {
                tracing::error!(error = %e, "persist TOTP enrollment failed");
            }
        }
        let _ = std::fs::remove_file(&self.bootstrap_path);
        Ok(self.issue_session())
    }

    pub fn logout_cookie(&self) -> String {
        cookie_header("", 0, self.cookie_secure)
    }

    pub fn session_cookie(&self, token: &str) -> String {
        cookie_header(token, SESSION_TTL_SECS as i64, self.cookie_secure)
    }

    fn issue_session(&self) -> String {
        let exp = now_secs() + SESSION_TTL_SECS;
        self.sign_session(&self.username, exp)
    }

    fn password_matches(&self, password: &str) -> bool {
        let got = hmac_hex(&self.password_key, password.as_bytes());
        ct_eq(got.as_bytes(), self.password_hash.as_bytes())
    }

    fn sign_session(&self, username: &str, exp: u64) -> String {
        let payload = format!("{username}|{exp}");
        let sig = hmac_hex(&self.session_secret, payload.as_bytes());
        format!("{}.{}", URL_SAFE_NO_PAD.encode(payload.as_bytes()), sig)
    }

    fn verify_session(&self, token: &str) -> Option<String> {
        let (payload_b64, sig) = token.split_once('.')?;
        let payload = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
        let expect = hmac_hex(&self.session_secret, &payload);
        if !ct_eq(expect.as_bytes(), sig.as_bytes()) {
            return None;
        }
        let payload = String::from_utf8(payload).ok()?;
        let (user, exp) = payload.rsplit_once('|')?;
        let exp: u64 = exp.parse().ok()?;
        if now_secs() >= exp || !ct_eq(user.as_bytes(), self.username.as_bytes()) {
            return None;
        }
        Some(user.to_string())
    }

    fn persist_enrolled(&self) -> Result<()> {
        let mut persist = read_persist(&self.persist_path)?;
        persist.totp_enrolled = true;
        persist.totp_secret.clone_from(&self.totp_secret);
        write_persist(&self.persist_path, &persist)
    }

    fn check_lock(&self, username: &str) -> Result<(), AuthError> {
        let now = now_secs();
        let fails = self.fails.lock();
        if let Some(st) = fails.get(username) {
            if st.locked_until > now {
                return Err(AuthError::Locked);
            }
        }
        Ok(())
    }

    fn record_fail(&self, username: &str) {
        let now = now_secs();
        let mut fails = self.fails.lock();
        let st = fails.entry(username.to_string()).or_insert(FailState {
            fails: 0,
            locked_until: 0,
        });
        if st.locked_until > now {
            return;
        }
        st.fails += 1;
        if st.fails >= MAX_FAILS {
            st.locked_until = now + LOCK_SECS;
            st.fails = 0;
        }
    }

    fn clear_fails(&self, username: &str) {
        self.fails.lock().remove(username);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    Denied,
    Locked,
    Disabled,
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|s| {
            matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn persist_path(database_url: &str) -> PathBuf {
    if let Some(path) = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))
    {
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                return parent.join("auth.json");
            }
        }
    }
    PathBuf::from("data/auth.json")
}

fn read_persist(path: &Path) -> Result<Persist> {
    if !path.exists() {
        return Ok(Persist::default());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

fn write_persist(path: &Path, persist: &Persist) -> Result<()> {
    let text = serde_json::to_string_pretty(persist)?;
    std::fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

fn write_bootstrap(path: &Path, username: &str, password: &str) -> Result<()> {
    let text = format!(
        "username={username}\npassword={password}\n# deleted after the first successful TOTP login\n"
    );
    std::fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

fn decode_hex_or_new(stored: &mut String, n: usize) -> Result<Vec<u8>> {
    if let Ok(bytes) = hex::decode(stored.as_str()) {
        if bytes.len() == n {
            return Ok(bytes);
        }
    }
    let mut bytes = vec![0u8; n];
    rand::thread_rng().fill(bytes.as_mut_slice());
    *stored = hex::encode(&bytes);
    Ok(bytes)
}

fn generate_password() -> String {
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    (0..16)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

fn generate_totp_secret() -> String {
    let mut raw = [0u8; 20];
    rand::thread_rng().fill(&mut raw);
    data_encoding::BASE32.encode(&raw).replace('=', "")
}

fn decode_totp_secret(secret: &str) -> Result<Vec<u8>> {
    let cleaned: String = secret
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '=')
        .collect::<String>()
        .to_ascii_uppercase();
    data_encoding::BASE32_NOPAD
        .decode(cleaned.as_bytes())
        .or_else(|_| {
            data_encoding::BASE32.decode(format!("{cleaned}{}", padding(&cleaned)).as_bytes())
        })
        .context("invalid base32 TOTP secret")
}

fn padding(s: &str) -> &'static str {
    match s.len() % 8 {
        2 => "======",
        4 => "====",
        5 => "===",
        7 => "=",
        _ => "",
    }
}

fn hmac_hex(key: &[u8], data: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}

fn totp_code(secret: &[u8], unix: u64) -> String {
    let counter = unix / 30;
    let mut mac = HmacSha1::new_from_slice(secret).expect("totp key");
    mac.update(&counter.to_be_bytes());
    let hash = mac.finalize().into_bytes();
    let offset = (hash[19] & 0x0f) as usize;
    let bin = ((u32::from(hash[offset]) & 0x7f) << 24)
        | (u32::from(hash[offset + 1]) << 16)
        | (u32::from(hash[offset + 2]) << 8)
        | u32::from(hash[offset + 3]);
    format!("{:06}", bin % 1_000_000)
}

fn totp_accepts(secret_b32: &str, code: &str, unix: u64) -> bool {
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Ok(secret) = decode_totp_secret(secret_b32) else {
        return false;
    };
    for skew in [0i64, -1, 1] {
        let t = unix.saturating_add_signed(skew * 30);
        if ct_eq(totp_code(&secret, t).as_bytes(), code.as_bytes()) {
            return true;
        }
    }
    false
}

fn otpauth_url(account: &str, secret: &str) -> String {
    format!(
        "otpauth://totp/{issuer}:{account}?secret={secret}&issuer={issuer}&algorithm=SHA1&digits=6&period=30",
        issuer = "STOCK%20MM",
        account = url_encode(account),
        secret = secret
    )
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn qr_svg(url: &str) -> Result<String> {
    let code = qrcode::QrCode::new(url.as_bytes()).context("qr encode")?;
    Ok(code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(180, 180)
        .dark_color(qrcode::render::svg::Color("#0b1020"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .quiet_zone(true)
        .build())
}

fn cookie_header(value: &str, max_age: i64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{COOKIE_NAME}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}{secure}")
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix(&format!("{name}="))
            .map(|v| v.to_string())
    })
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_auth(password: &str, totp_raw: &[u8]) -> Auth {
        let password_key = vec![7u8; 32];
        Auth {
            enabled: true,
            username: "admin".into(),
            password_hash: hmac_hex(&password_key, password.as_bytes()),
            password_key,
            totp_secret: data_encoding::BASE32.encode(totp_raw).replace('=', ""),
            totp_enrolled: Mutex::new(true),
            session_secret: vec![9u8; 32],
            cookie_secure: false,
            persist_path: PathBuf::from("/tmp/stock_mm_auth_test.json"),
            bootstrap_path: PathBuf::from("/tmp/stock_mm_auth_test_pw.txt"),
            pending: Mutex::new(HashMap::new()),
            fails: Mutex::new(HashMap::new()),
        }
    }

    #[test]
    fn rfc6238_sha1_vectors() {
        let secret = b"12345678901234567890";
        assert_eq!(totp_code(secret, 59), "287082");
        assert_eq!(totp_code(secret, 1_111_111_109), "081804");
        assert_eq!(totp_code(secret, 1_111_111_111), "050471");
    }

    #[test]
    fn totp_accepts_adjacent_windows() {
        let secret = data_encoding::BASE32
            .encode(b"12345678901234567890")
            .replace('=', "");
        let code = totp_code(b"12345678901234567890", 1_700_000_000);
        assert!(totp_accepts(&secret, &code, 1_700_000_000));
        assert!(totp_accepts(&secret, &code, 1_700_000_029));
        assert!(!totp_accepts(&secret, "000000", 1_700_000_000));
        assert!(!totp_accepts(&secret, "abc123", 1_700_000_000));
    }

    #[test]
    fn session_roundtrip() {
        let auth = test_auth("passw0rd", b"12345678901234567890");
        let token = auth.sign_session("admin", now_secs() + 60);
        assert_eq!(auth.verify_session(&token).as_deref(), Some("admin"));
        let expired = auth.sign_session("admin", now_secs() - 1);
        assert!(auth.verify_session(&expired).is_none());
        assert!(auth.verify_session("nope").is_none());
    }

    #[test]
    fn login_then_totp() {
        let secret = b"12345678901234567890";
        let auth = test_auth("s3cret", secret);
        assert!(auth.login("nope", "s3cret").is_err());
        assert!(auth.login("admin", "wrong").is_err());
        let out = auth.login("admin", "s3cret").unwrap();
        assert!(!out.enroll);
        let code = totp_code(secret, now_secs());
        let cookie = auth.verify(&out.ticket, &code).unwrap();
        assert!(auth.verify_session(&cookie).is_some());
        assert!(auth.verify(&out.ticket, &code).is_err());
    }

    #[test]
    fn first_login_returns_enroll_material() {
        let auth = test_auth("s3cret", b"12345678901234567890");
        *auth.totp_enrolled.lock() = false;
        let out = auth.login("admin", "s3cret").unwrap();
        assert!(out.enroll);
        assert!(out.secret.as_ref().is_some_and(|s| !s.is_empty()));
        assert!(
            out.otpauth_url
                .as_ref()
                .is_some_and(|u| u.starts_with("otpauth://totp/"))
        );
        assert!(out.qr_svg.as_ref().is_some_and(|s| s.contains("<svg")));
        let code = totp_code(b"12345678901234567890", now_secs());
        auth.verify(&out.ticket, &code).unwrap();
        assert!(*auth.totp_enrolled.lock());
    }

    #[test]
    fn lockout_after_repeated_failures() {
        let auth = test_auth("s3cret", b"12345678901234567890");
        for _ in 0..MAX_FAILS {
            assert_eq!(auth.login("admin", "bad").unwrap_err(), AuthError::Denied);
        }
        assert_eq!(
            auth.login("admin", "s3cret").unwrap_err(),
            AuthError::Locked
        );
    }

    #[test]
    fn otpauth_contains_secret() {
        let url = otpauth_url("admin", "MFRGGZDFMZTWQ2LK");
        assert!(url.starts_with("otpauth://totp/STOCK%20MM:admin?"));
        assert!(url.contains("secret=MFRGGZDFMZTWQ2LK"));
        assert!(url.contains("issuer=STOCK%20MM"));
    }
}
