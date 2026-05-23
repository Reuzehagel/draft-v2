use anyhow::Result;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub fn init() -> Result<WorkerGuard> {
    let dir = crate::paths::log_dir()?;
    let appender = tracing_appender::rolling::daily(&dir, "app.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,draft=debug"));

    let file_layer = fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(true);

    let stderr_layer = fmt::layer().with_writer(std::io::stderr);

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .init();

    std::panic::set_hook(Box::new(|info| {
        tracing::error!(panic = %info, "panic");
        let bt = std::backtrace::Backtrace::force_capture();
        tracing::error!(backtrace = %bt, "panic backtrace");
    }));

    Ok(guard)
}
