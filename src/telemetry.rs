//! Optional crash/error reporting and performance monitoring via Sentry.
//!
//! Enabled only when a DSN is available from `MOBI2EPUB_SENTRY_DSN`: read at
//! runtime first, falling back to the value baked in at build time (release
//! builds get this from a CI secret). The variable is namespaced so it doesn't
//! pick up a `SENTRY_DSN` meant for some other program. Setting
//! `MOBI2EPUB_NO_TELEMETRY` to any value, or `MOBI2EPUB_SENTRY_DSN` to an
//! empty string, turns it off.
//!
//! What gets sent: panics (with a stack trace), conversion failures for books
//! the tool couldn't read, and a performance trace per converted book (time
//! spent in each stage, input/output sizes, chapter and image counts). File
//! names, paths and book contents are never sent.
//!
//! `MOBI2EPUB_TRACES_SAMPLE_RATE` (0.0–1.0, default 1.0) sets the share of
//! conversions that send a performance trace.

use sentry::protocol::{SpanStatus, Value};
use std::borrow::Cow;

pub fn init() -> Option<sentry::ClientInitGuard> {
    if std::env::var_os("MOBI2EPUB_NO_TELEMETRY").is_some() {
        return None;
    }
    let dsn = match std::env::var("MOBI2EPUB_SENTRY_DSN") {
        Ok(v) => v,
        Err(_) => option_env!("MOBI2EPUB_SENTRY_DSN")?.to_string(),
    };
    if dsn.trim().is_empty() {
        return None;
    }
    // ClientOptions is #[non_exhaustive], so start from the defaults.
    let mut options = sentry::ClientOptions::default();
    options.release = sentry::release_name!();
    options.environment = Some(Cow::Borrowed(if cfg!(debug_assertions) { "development" } else { "production" }));
    options.send_default_pii = false;
    options.server_name = None; // don't report the machine's hostname
    // In sentry 0.49 tracing is off unless a sampling strategy is set.
    let options = options.traces_sample_rate(traces_sample_rate());
    let guard = sentry::init((dsn.trim(), options));
    guard.is_enabled().then_some(guard)
}

fn traces_sample_rate() -> f32 {
    std::env::var("MOBI2EPUB_TRACES_SAMPLE_RATE")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|r| r.is_finite())
        .map(|r| r.clamp(0.0, 1.0))
        .unwrap_or(1.0)
}

/// Report a book that couldn't be converted (unsupported or corrupt input).
pub fn report_book_error(message: &str, extension: &str, size: usize) {
    sentry::with_scope(
        |scope| {
            scope.set_tag("input.extension", extension);
            scope.set_extra("input.size", size.into());
        },
        || {
            sentry::capture_message(message, sentry::Level::Warning);
        },
    );
}

/// How a conversion ended, for the trace's status.
pub enum Outcome {
    Ok,
    /// Bad arguments or the output already exists.
    Usage,
    /// The book couldn't be read (unsupported, corrupt, DRM).
    BadBook,
    /// Reading or writing files failed.
    Io,
}

/// Performance trace for converting one book. When Sentry is off this is
/// a cheap no-op.
pub struct Conversion {
    tx: sentry::Transaction,
}

impl Conversion {
    pub fn start(extension: &str) -> Self {
        let ctx = sentry::TransactionContext::new("convert", "cli.convert");
        let tx = sentry::start_transaction(ctx);
        tx.set_data("input.extension", extension.into());
        // Bind to the scope so errors reported during the conversion link to it.
        sentry::configure_scope(|scope| scope.set_span(Some(tx.clone().into())));
        Conversion { tx }
    }

    /// Time one stage of the conversion as a child span.
    pub fn step<T>(&self, op: &str, description: &str, f: impl FnOnce() -> T) -> T {
        let span = self.tx.start_child(op, description);
        let result = f();
        span.finish();
        result
    }

    pub fn set_data(&self, key: &str, value: impl Into<Value>) {
        self.tx.set_data(key, value.into());
    }

    pub fn finish(self, outcome: Outcome) {
        self.tx.set_status(match outcome {
            Outcome::Ok => SpanStatus::Ok,
            Outcome::Usage => SpanStatus::InvalidArgument,
            Outcome::BadBook => SpanStatus::DataLoss,
            Outcome::Io => SpanStatus::InternalError,
        });
        sentry::configure_scope(|scope| scope.set_span(None));
        self.tx.finish();
    }
}
