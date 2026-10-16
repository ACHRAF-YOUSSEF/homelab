#![forbid(unsafe_code)]

use jellyfin_counts_cache::{Error, cache, config, refresh_forever, server, upstream::Upstream};

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
    let mut arguments = std::env::args().skip(1);
    let bind = config::bind_from_env()?;
    match (arguments.next().as_deref(), arguments.next()) {
        (Some("--healthcheck"), None) => return server::healthcheck(bind).await,
        (None, None) => {}
        _ => return Err(Error::Configuration("command line arguments")),
    }
    let upstream = Upstream::new(config::Config::from_env()?)?;
    let listener = server::bind(bind)?;
    let cache = cache();
    eprintln!("jellyfin-counts-cache listening");
    tokio::select! {
        result = server::serve(listener, cache.clone(), shutdown_signal()) => result,
        _ = refresh_forever(upstream, cache) => Ok(()),
    }
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
            eprintln!("jellyfin-counts-cache: runtime initialization failed");
            return std::process::ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("jellyfin-counts-cache: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
