//! Network configuration shared by every download: custom user agent,
//! extra headers, cookies, referer, proxy and DNS-over-HTTPS resolution.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, COOKIE, REFERER};
use reqwest::{Client, Proxy};
use serde::{Deserialize, Serialize};

pub const DEFAULT_USER_AGENT: &str = "rdm/0.1 (+https://github.com/) Rust download manager";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetConfig {
    pub user_agent: Option<String>,
    /// `http://host:port`, `socks5://host:port`, … empty = direct.
    pub proxy: Option<String>,
    /// DNS-over-HTTPS resolver endpoint, e.g. `https://cloudflare-dns.com/dns-query`.
    pub doh: Option<String>,
    pub headers: Vec<(String, String)>,
    pub referer: Option<String>,
    pub cookie: Option<String>,
    pub timeout_secs: u64,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            user_agent: None,
            proxy: None,
            doh: None,
            headers: Vec::new(),
            referer: None,
            cookie: None,
            timeout_secs: 20,
        }
    }
}

impl NetConfig {
    pub fn header_map(&self) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (key, value) in &self.headers {
            if let (Ok(k), Ok(v)) = (
                key.trim().parse::<HeaderName>(),
                HeaderValue::from_str(value.trim()),
            ) {
                map.insert(k, v);
            }
        }
        if let Some(referer) = self.referer.as_deref().filter(|r| !r.trim().is_empty()) {
            if let Ok(v) = HeaderValue::from_str(referer.trim()) {
                map.insert(REFERER, v);
            }
        }
        if let Some(cookie) = self.cookie.as_deref().filter(|c| !c.trim().is_empty()) {
            if let Ok(v) = HeaderValue::from_str(cookie.trim()) {
                map.insert(COOKIE, v);
            }
        }
        map
    }
}

/// Builds a client for one download. When DNS-over-HTTPS is configured the
/// host is resolved through the DoH endpoint and pinned on the client, so the
/// system resolver (and its filtering) is bypassed entirely.
pub async fn build_client(cfg: &NetConfig, url: &str) -> Result<Client> {
    let mut builder = Client::builder()
        .user_agent(cfg.user_agent.clone().unwrap_or_else(|| DEFAULT_USER_AGENT.to_string()))
        .default_headers(cfg.header_map())
        .connect_timeout(Duration::from_secs(cfg.timeout_secs.max(5)))
        .pool_max_idle_per_host(32);

    if let Some(proxy) = cfg.proxy.as_deref().filter(|p| !p.trim().is_empty()) {
        builder = builder.proxy(Proxy::all(proxy.trim()).with_context(|| format!("bad proxy {proxy}"))?);
    }

    if let Some(doh) = cfg.doh.as_deref().filter(|d| !d.trim().is_empty()) {
        if let Ok(parsed) = url::Url::parse(url) {
            if let Some(host) = parsed.host_str() {
                if host.parse::<IpAddr>().is_err() {
                    if let Some(ip) = resolve_doh(doh.trim(), host).await {
                        let port = parsed.port_or_known_default().unwrap_or(443);
                        builder = builder.resolve(host, SocketAddr::new(ip, port));
                    }
                }
            }
        }
    }

    Ok(builder.build()?)
}

/// Minimal RFC 8484 JSON lookup — no extra DNS dependency needed.
async fn resolve_doh(endpoint: &str, host: &str) -> Option<IpAddr> {
    let client = Client::builder().timeout(Duration::from_secs(8)).build().ok()?;
    let resp = client
        .get(endpoint)
        .query(&[("name", host), ("type", "A")])
        .header("accept", "application/dns-json")
        .send()
        .await
        .ok()?;
    let value: serde_json::Value = resp.json().await.ok()?;
    value
        .get("Answer")?
        .as_array()?
        .iter()
        .filter_map(|a| a.get("data")?.as_str()?.parse::<IpAddr>().ok())
        .next()
}
