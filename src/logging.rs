use anyhow::Result;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Layer};

/// What the terminal gets. The log file is the same either way; these differ
/// only in what is worth putting in front of whoever is watching stderr.
enum Terminal {
    /// The tray app: everything, styled. Nobody is piping it.
    Everything,
    /// `draft-cli`: Draft's own warnings and errors, unstyled — stderr there
    /// is usually a pipe, and a decoder's internal grumbling ("skipped 44
    /// bytes of junk") is not something the caller of `transcribe` can act on.
    DraftsOwnWarnings,
}

pub fn init() -> Result<WorkerGuard> {
    init_for(Terminal::Everything)
}

/// Logging for `draft-cli`. `RUST_LOG` still overrides what reaches the
/// terminal when someone wants the detail; the log file has it either way.
pub fn init_cli() -> Result<WorkerGuard> {
    init_for(Terminal::DraftsOwnWarnings)
}

fn init_for(terminal: Terminal) -> Result<WorkerGuard> {
    let dir = crate::paths::log_dir()?;
    let appender = tracing_appender::rolling::daily(&dir, "app.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,draft=debug"));

    let file_layer = fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(true);

    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(matches!(terminal, Terminal::Everything))
        .with_filter(match terminal {
            Terminal::Everything => EnvFilter::new("trace"),
            Terminal::DraftsOwnWarnings => EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("off,draft=warn")),
        });

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
