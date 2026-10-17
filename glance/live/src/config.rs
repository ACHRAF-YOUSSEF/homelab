use crate::Error;
use std::{collections::HashSet, env, net::SocketAddr, time::Duration};
use url::Url;

#[derive(Clone)]
pub struct Config {
    pub glance: Url,
    pub qbittorrent: Url,
    pub jellyfin: Url,
    pub sonarr: Url,
    pub radarr: Url,
    pub subforge: Url,
    pub allowed_hosts: HashSet<String>,
    pub bind: SocketAddr,
    pub qb_interval: Duration,
    pub jellyfin_interval: Duration,
    pub jellyfin_ws: bool,
    pub max_connections: usize,
    pub max_http_connections: usize,
    pub sse_lifetime: Duration,
    pub provider_bytes: usize,
    pub snapshot_bytes: usize,
    pub missing_provider_bytes: usize,
    pub missing_snapshot_bytes: usize,
    pub max_torrents: usize,
    pub max_sessions: usize,
    pub qb_username: String,
    pub qb_password: String,
    pub jellyfin_key: String,
    pub sonarr_key: String,
    pub radarr_key: String,
    pub subforge_token: String,
}

fn value(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn integer(name: &'static str, default: &str, maximum: usize) -> Result<usize, Error> {
    let raw = value(name, default);
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Configuration(name));
    }
    raw.parse::<usize>()
        .ok()
        .filter(|n| (1..=maximum).contains(n))
        .ok_or(Error::Configuration(name))
}

fn seconds(name: &'static str, default: &str, maximum: f64) -> Result<Duration, Error> {
    let n = value(name, default)
        .parse::<f64>()
        .map_err(|_| Error::Configuration(name))?;
    if !n.is_finite() || n <= 0.0 || n > maximum {
        return Err(Error::Configuration(name));
    }
    Ok(Duration::from_secs_f64(n))
}

pub fn authority(raw: &str) -> Result<(String, Option<u16>), Error> {
    if raw.is_empty()
        || raw.ends_with(':')
        || raw.chars().any(char::is_whitespace)
        || raw.chars().any(|c| "\\/@?#".contains(c))
    {
        return Err(Error::InvalidData);
    }
    let parsed = raw
        .parse::<hyper::http::uri::Authority>()
        .map_err(|_| Error::InvalidData)?;
    let port = parsed.port_u16();
    if port == Some(0) || (parsed.port().is_some() && port.is_none()) {
        return Err(Error::InvalidData);
    }
    let hostname = parsed.host().trim_end_matches('.').to_ascii_lowercase();
    if hostname.is_empty() {
        return Err(Error::InvalidData);
    }
    Ok((hostname, port))
}

pub fn base(raw: &str, name: &'static str) -> Result<Url, Error> {
    let invalid = || Error::Configuration(name);
    if raw.chars().any(char::is_whitespace) || raw.contains(['\\', '%']) {
        return Err(invalid());
    }
    let parsed = Url::parse(raw).map_err(|_| invalid())?;
    // Validate the literal authority too: URL normalization can erase empty userinfo/ports.
    let literal_authority = raw
        .split_once("://")
        .map(|(_, rest)| rest.split('/').next().unwrap_or_default())
        .ok_or_else(invalid)?;
    authority(literal_authority).map_err(|_| invalid())?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path_segments().is_none()
        || parsed.path().contains('%')
        || raw.split('/').any(|s| matches!(s, "." | ".."))
    {
        return Err(invalid());
    }
    Ok(parsed)
}

pub fn endpoint(base: &Url, path: &str) -> String {
    format!("{}{}", base.as_str().trim_end_matches('/'), path)
}

impl Config {
    pub fn from_env() -> Result<Self, Error> {
        let allowed_hosts: HashSet<_> = value("GLANCE_LIVE_ALLOWED_HOSTS", "localhost,127.0.0.1")
            .split(',')
            .filter(|h| !h.trim().is_empty())
            .map(|h| authority(h.trim()).map(|a| a.0))
            .collect::<Result<_, _>>()
            .map_err(|_| Error::Configuration("GLANCE_LIVE_ALLOWED_HOSTS"))?;
        if allowed_hosts.is_empty() {
            return Err(Error::Configuration("GLANCE_LIVE_ALLOWED_HOSTS"));
        }
        let host = value("GLANCE_LIVE_BIND_ADDRESS", "127.0.0.1")
            .parse::<std::net::IpAddr>()
            .map_err(|_| Error::Configuration("GLANCE_LIVE_BIND_ADDRESS"))?;
        let port = integer("GLANCE_LIVE_PORT", "8081", 65535)? as u16;
        let jellyfin_ws = match value("GLANCE_LIVE_JELLYFIN_WS", "true")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => return Err(Error::Configuration("GLANCE_LIVE_JELLYFIN_WS")),
        };
        let config = Self {
            glance: base(
                &value("GLANCE_UPSTREAM_URL", "http://glance:8080"),
                "GLANCE_UPSTREAM_URL",
            )?,
            qbittorrent: base(
                &value("QBITTORRENT_INTERNAL_URL", "http://qbittorrent:8080"),
                "QBITTORRENT_INTERNAL_URL",
            )?,
            jellyfin: base(
                &value("JELLYFIN_INTERNAL_URL", "http://jellyfin:8096"),
                "JELLYFIN_INTERNAL_URL",
            )?,
            sonarr: base(
                &value("SONARR_INTERNAL_URL", "http://sonarr:8989"),
                "SONARR_INTERNAL_URL",
            )?,
            radarr: base(
                &value("RADARR_INTERNAL_URL", "http://radarr:7878"),
                "RADARR_INTERNAL_URL",
            )?,
            subforge: base(
                &value("GLANCE_SUBFORGE_INTERNAL_URL", "http://subforge:8100"),
                "GLANCE_SUBFORGE_INTERNAL_URL",
            )?,
            allowed_hosts,
            bind: SocketAddr::new(host, port),
            qb_interval: seconds("GLANCE_LIVE_QBITTORRENT_INTERVAL", "2", 60.0)?,
            jellyfin_interval: seconds("GLANCE_LIVE_JELLYFIN_INTERVAL", "5", 60.0)?,
            jellyfin_ws,
            max_connections: integer("GLANCE_LIVE_MAX_CONNECTIONS", "64", 1000)?,
            max_http_connections: integer("GLANCE_LIVE_MAX_HTTP_CONNECTIONS", "128", 2000)?,
            sse_lifetime: seconds("GLANCE_LIVE_SSE_MAX_SECONDS", "900", 86400.0)?,
            provider_bytes: integer(
                "GLANCE_LIVE_MAX_PROVIDER_BYTES",
                "2097152",
                16 * 1024 * 1024,
            )?,
            snapshot_bytes: integer("GLANCE_LIVE_MAX_SNAPSHOT_BYTES", "262144", 2 * 1024 * 1024)?,
            missing_provider_bytes: integer(
                "GLANCE_LIVE_MISSING_MAX_PROVIDER_BYTES",
                "8388608",
                16 * 1024 * 1024,
            )?,
            missing_snapshot_bytes: integer(
                "GLANCE_LIVE_MISSING_MAX_SNAPSHOT_BYTES",
                "2097152",
                4 * 1024 * 1024,
            )?,
            max_torrents: integer("GLANCE_LIVE_MAX_TORRENTS", "2000", 10000)?,
            max_sessions: integer("GLANCE_LIVE_MAX_SESSIONS", "256", 2048)?,
            qb_username: value("QBITTORRENT_USERNAME", ""),
            qb_password: value("QBITTORRENT_PASSWORD", ""),
            jellyfin_key: value("JELLYFIN_API_KEY", ""),
            sonarr_key: value("SONARR_API_KEY", ""),
            radarr_key: value("RADARR_API_KEY", ""),
            subforge_token: value("CONVERTER_WIDGET_TOKEN", ""),
        };
        if [
            &config.qb_username,
            &config.qb_password,
            &config.jellyfin_key,
            &config.sonarr_key,
            &config.radarr_key,
            &config.subforge_token,
        ]
        .iter()
        .any(|v| v.len() > 4096 || v.chars().any(char::is_control))
            || config.jellyfin_key.contains(['"', '\\'])
        {
            return Err(Error::Configuration("provider credentials"));
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ambiguous_authorities_and_provider_urls() {
        for invalid in [
            "http://@localhost",
            "http://localhost:bad",
            "http://localhost:",
            "http://bad host",
            "http://localhost\\@evil",
            "http://localhost:0",
            "http://user:pass@localhost",
            "http://localhost/a/../b",
            "http://localhost/%2e%2e",
        ] {
            assert!(base(invalid, "TEST").is_err(), "{invalid}");
        }
        assert_eq!(
            authority("LOCALHOST.:8081").unwrap(),
            ("localhost".into(), Some(8081))
        );
        assert!(base("http://jellyfin:8096/base", "TEST").is_ok());
    }
}
