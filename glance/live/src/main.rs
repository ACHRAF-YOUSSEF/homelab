#![forbid(unsafe_code)]

use glance_live::{Error, config::Config, gateway, relay::Relay};

async fn shutdown_signal() {
    #[cfg(unix)]
    if let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}

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
    gateway::serve(listener, relay, shutdown_signal()).await
}

fn main() -> std::process::ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(2)
        .thread_stack_size(512 * 1024)
        .thread_keep_alive(std::time::Duration::from_secs(5))
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("glance-live: runtime initialization failed");
            return std::process::ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("glance-live: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
