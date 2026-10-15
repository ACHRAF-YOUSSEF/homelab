#![forbid(unsafe_code)]

use glance_live::{Error, config::Config, gateway, relay::Relay};

async fn run() -> Result<(), Error> {
    let config = Config::from_env()?;
    let mut arguments = std::env::args().skip(1);
    match (arguments.next().as_deref(), arguments.next()) {
        (Some("--healthcheck"), None) => return gateway::healthcheck(&config).await,
        (None, None) => {}
        _ => return Err(Error::Configuration("command line arguments")),
    }
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    let relay = Relay::new(config)?;
    eprintln!("glance-live ready");
    gateway::serve(listener, relay, async { let _ = tokio::signal::ctrl_c().await; }).await
}

fn main() -> std::process::ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(_) => { eprintln!("glance-live: runtime initialization failed"); return std::process::ExitCode::FAILURE; }
    };
    match runtime.block_on(run()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => { eprintln!("glance-live: {error}"); std::process::ExitCode::FAILURE }
    }
}
