#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("Subforge {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("integration-check") {
        let provider = std::env::args().nth(2).unwrap_or_default();
        if !["sonarr", "radarr", "bazarr", "jellyfin"].contains(&provider.as_str())
            || std::env::args().len() != 3
        {
            eprintln!("Usage: subforge integration-check sonarr|radarr|bazarr|jellyfin");
            std::process::exit(2);
        }
        let data = std::path::PathBuf::from(
            std::env::var("CONVERTER_DATA").unwrap_or_else(|_| "/data".into()),
        );
        let integrations =
            match subforge::integrations::Integrations::new(data.join("settings.json")) {
                Ok(integrations) => integrations,
                Err(_) => {
                    eprintln!("Could not load integration settings");
                    std::process::exit(2);
                }
            };
        let result = integrations.test(&provider);
        let connected = result["connected"].as_bool().unwrap_or(false);
        let mut summary = serde_json::json!({"provider":provider,"connected":connected});
        if provider == "jellyfin" && connected {
            let public = integrations.public_settings();
            summary["library_count"] = serde_json::json!(
                integrations
                    .libraries()
                    .ok()
                    .and_then(|value| value.as_array().map(Vec::len))
            );
            for field in ["movie_libraries", "series_libraries"] {
                summary[format!("selected_{field}")] =
                    serde_json::json!(public["jellyfin"][field].as_array().map_or(0, Vec::len));
            }
        }
        // Diagnostics disclose counts only, never endpoints, names, or credentials.
        println!("{summary}");
        std::process::exit(if connected { 0 } else { 1 });
    }
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        let healthy = (|| -> std::io::Result<bool> {
            let mut socket = TcpStream::connect_timeout(
                &"127.0.0.1:8100"
                    .parse()
                    .map_err(|_| std::io::ErrorKind::InvalidInput)?,
                Duration::from_secs(3),
            )?;
            socket.set_read_timeout(Some(Duration::from_secs(3)))?;
            socket.set_write_timeout(Some(Duration::from_secs(3)))?;
            socket.write_all(
                b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )?;
            let mut reply = String::new();
            socket.take(8192).read_to_string(&mut reply)?;
            Ok(reply.starts_with("HTTP/1.1 200 ") && reply.contains("\"ok\":true"))
        })()
        .unwrap_or(false);
        std::process::exit(if healthy { 0 } else { 1 });
    }
    if let Err(error) = subforge::server::run(std::env::args().skip(1).collect()) {
        eprintln!("Subforge startup failed: {error}");
        std::process::exit(2);
    }
}
