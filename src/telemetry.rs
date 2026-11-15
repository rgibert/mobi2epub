//! Optional crash and error reporting to Sentry.
//!
//! Reporting is enabled only when a DSN is available: either `SENTRY_DSN` at
//! runtime, or `MOBI2EPUB_SENTRY_DSN` baked in at build time (release builds
//! get this from a CI secret). Setting `MOBI2EPUB_NO_TELEMETRY` to any value,
//! or `SENTRY_DSN` to an empty string, turns it off.
//!
//! What gets sent: panics (with a stack trace) and conversion failures for
//! books the tool couldn't read, tagged with the file extension and size.
//! File names, paths and book contents are never sent.

use std::borrow::Cow;

pub fn init() -> Option<sentry::ClientInitGuard> {
    if std::env::var_os("MOBI2EPUB_NO_TELEMETRY").is_some() {
        return None;
    }
    let dsn = match std::env::var("SENTRY_DSN") {
        Ok(v) => v,
        Err(_) => option_env!("MOBI2EPUB_SENTRY_DSN")?.to_string(),
    };
    if dsn.trim().is_empty() {
        return None;
    }
    let guard = sentry::init((
        dsn.trim(),
        sentry::ClientOptions {
            release: sentry::release_name!(),
            environment: Some(Cow::Borrowed(if cfg!(debug_assertions) { "development" } else { "production" })),
            send_default_pii: false,
            // Don't report the machine's hostname.
            server_name: None,
            ..Default::default()
        },
    ));
    guard.is_enabled().then_some(guard)
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
