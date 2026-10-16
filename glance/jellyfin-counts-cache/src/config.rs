use crate::Error;
use reqwest::header::HeaderValue;
use std::{env, net::SocketAddr};
use url::Url;

pub struct Config {
    pub endpoint: Url,
    pub authorization: HeaderValue,
}

pub fn bind_address(raw: &str) -> Result<SocketAddr, Error> {
    let host = match raw.trim() {
        "" | "localhost" => "127.0.0.1",
        other => other,
    };
    host.parse()
        .map(|host| SocketAddr::new(host, 8765))
        .map_err(|_| Error::Configuration("JELLYFIN_COUNTS_BIND_ADDRESS"))
}

pub fn bind_from_env() -> Result<SocketAddr, Error> {
    bind_address(&env::var("JELLYFIN_COUNTS_BIND_ADDRESS").unwrap_or_default())
}

impl Config {
    pub fn from_env() -> Result<Self, Error> {
        Self::new(
            &env::var("JELLYFIN_INTERNAL_URL")
                .map_err(|_| Error::Configuration("JELLYFIN_INTERNAL_URL"))?,
            &env::var("JELLYFIN_API_KEY").map_err(|_| Error::Configuration("JELLYFIN_API_KEY"))?,
        )
    }

    pub fn new(base: &str, token: &str) -> Result<Self, Error> {
        let invalid = || Error::Configuration("JELLYFIN_INTERNAL_URL");
        if base.len() > 4096
            || base.chars().any(|c| c.is_whitespace() || c.is_control())
            || base.contains(['\\', '%'])
            || base.split('/').any(|part| matches!(part, "." | ".."))
        {
            return Err(invalid());
        }
        let mut endpoint = Url::parse(base).map_err(|_| invalid())?;
        let authority = base
            .split_once("://")
            .map(|(_, rest)| rest.split('/').next().unwrap_or_default())
            .ok_or_else(invalid)?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || authority.contains('@')
            || authority.ends_with(':')
            || endpoint.port() == Some(0)
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(invalid());
        }
        let path = format!("{}/Items/Counts", endpoint.path().trim_end_matches('/'));
        endpoint.set_path(&path);

        if token.len() > 4096 || token.chars().any(char::is_control) || token.contains(['"', '\\'])
        {
            return Err(Error::Configuration("JELLYFIN_API_KEY"));
        }
        let mut authorization = HeaderValue::from_str(&format!(
            "MediaBrowser Client=\"Glance\", Device=\"Glance\", DeviceId=\"glance-dashboard\", Version=\"1.0\", Token=\"{token}\""
        ))
        .map_err(|_| Error::Configuration("JELLYFIN_API_KEY"))?;
        authorization.set_sensitive(true);
        Ok(Self {
            endpoint,
            authorization,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_base_path_and_rejects_unsafe_configuration() {
        assert_eq!(
            Config::new("http://jellyfin:8096/base/", "fixture")
                .unwrap()
                .endpoint
                .as_str(),
            "http://jellyfin:8096/base/Items/Counts"
        );
        for base in [
            "file:///tmp/counts",
            "http://@localhost",
            "http://user:pass@localhost",
            "http://localhost:0",
            "http://localhost:",
            "http://localhost/a/../b",
            "http://localhost/%2e",
            "http://localhost\\@other",
            "http://localhost/?q=a",
            "http://localhost/#fragment",
            "http://local host/",
        ] {
            assert!(Config::new(base, "fixture").is_err(), "{base}");
        }
        for token in ["bad\r\nheader", "bad\"token", "bad\\token"] {
            assert!(Config::new("http://localhost", token).is_err());
        }
    }

    #[test]
    fn bind_defaults_and_wildcards() {
        assert_eq!(bind_address(" ").unwrap().to_string(), "127.0.0.1:8765");
        assert_eq!(bind_address("0.0.0.0").unwrap().to_string(), "0.0.0.0:8765");
        assert_eq!(bind_address("::").unwrap().to_string(), "[::]:8765");
        assert!(bind_address("unexpected.example").is_err());
    }
}
