//! Logging bootstrap: installs the tracing subscriber over the sink
//! [`crate::run_log`] opens.

use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

use crate::doctor::DriftSeen;
use crate::run_log::LogLocation;
use std::time::SystemTime;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;

/// Install the global tracing subscriber, and return what its drift layer
/// sees. TUI mode (and `floating`, a long-running GUI whose launching terminal
/// would just collect noise) ALWAYS logs to a file — the alternate screen owns
/// the terminal, so the log file is the only place a runtime error can surface
/// — at a `warn` floor unless $RUST_LOG, $PIXTUOID_LOG or --log-level raises
/// it. Every other mode goes to stderr.
pub(crate) fn init(tui_active: bool, log_level: &'static str) -> DriftSeen {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let rust_log = pixtuoid_core::platform::text_env("RUST_LOG");
    let make_filter = || {
        EnvFilter::try_new(filter_directives(rust_log.as_deref(), log_level))
            .unwrap_or_else(|_| EnvFilter::new(log_level))
    };

    let wants_verbose = matches!(log_level, "debug" | "trace");
    let log_at = LogLocation::from_env();
    let explicit_log_file = matches!(log_at, LogLocation::File(_));
    let drift = DriftSeen::default();
    let registry = tracing_subscriber::registry().with(drift.layer());

    if tui_active {
        let rust_log_set = rust_log.as_deref().is_some_and(|v| !v.is_empty());
        let filter = if wants_verbose || explicit_log_file {
            make_filter()
        } else if rust_log_set {
            // Honor RUST_LOG, but floor the parse-failure fallback at warn (not
            // log_level) so the always-on file stays quiet by default.
            EnvFilter::try_new(filter_directives(rust_log.as_deref(), "warn"))
                .unwrap_or_else(|_| EnvFilter::new("warn"))
        } else {
            EnvFilter::new(match log_level {
                lvl @ ("warn" | "error") => lvl,
                _ => "warn",
            })
        };
        match log_at.open_sink(SystemTime::now()) {
            Ok(f) => {
                registry
                    .with(
                        fmt_layer()
                            .with_ansi(false)
                            .with_writer(f)
                            .with_filter(filter),
                    )
                    .init();
            }
            Err((path, e)) => {
                // The footer's "see log" advice would point at nothing — say so on
                // the pre-altscreen stderr channel rather than degrading silently.
                let _ = writeln!(std::io::stderr(), "{}", log_open_failure(&path, &e));
                registry.init();
            }
        }
    } else {
        registry
            .with(
                fmt_layer()
                    .with_writer(std::io::stderr)
                    .with_filter(make_filter()),
            )
            .init();
    }
    drift
}

/// The formatting layer, its field values escaped by [`EscapedFields`] and its
/// internal-error report off: on a failed write that report is an
/// `eprintln!`, which panics when stderr is the pipe that failed, and lands on
/// the TUI's alternate screen when the log file is.
pub(crate) fn fmt_layer<S>() -> tracing_subscriber::fmt::Layer<S, EscapedFields>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer()
        .fmt_fields(EscapedFields)
        .log_internal_errors(false)
}

/// The default field layout with every value's [terminal-unsafe] characters
/// escaped as `char::escape_debug` writes them, so a `%` field is as safe as a
/// `?` one: the default escapes only the message, and of it only some controls
/// (tracing-subscriber's `fmt/format/escape.rs`). It works on each value, not
/// on the written line, where the formatter's own SGR would look injected.
///
/// [terminal-unsafe]: crate::is_terminal_unsafe
pub(crate) struct EscapedFields;

impl<'w> tracing_subscriber::fmt::FormatFields<'w> for EscapedFields {
    fn format_fields<R: tracing_subscriber::field::RecordFields>(
        &self,
        writer: tracing_subscriber::fmt::format::Writer<'w>,
        fields: R,
    ) -> std::fmt::Result {
        use tracing_subscriber::field::VisitOutput;
        let mut visitor = Escaping(tracing_subscriber::fmt::format::DefaultVisitor::new(
            writer, true,
        ));
        fields.record(&mut visitor);
        visitor.0.finish()
    }
}

/// Hands the default visitor each value escaped. The `record_*` it does not
/// override fall to `record_debug`.
struct Escaping<'w>(tracing_subscriber::fmt::format::DefaultVisitor<'w>);

impl tracing::field::Visit for Escaping<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.record_debug(field, &Escaped(value));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        // The default writes a message `str` as its Display, any other as its
        // Debug, which escapes these already.
        if field.name() == "message" {
            self.0
                .record_debug(field, &Escaped(&format_args!("{value}")));
        } else {
            self.0.record_str(field, value);
        }
    }

    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.0.record_error(field, &EscapedChain::of(value));
    }
}

/// An error chain with each link's Display escaped, for the default visitor to
/// list as it lists any chain: its own escaping misses bidi and `\n`.
#[derive(Debug)]
struct EscapedChain(String, Option<Box<EscapedChain>>);

impl EscapedChain {
    fn of(e: &(dyn std::error::Error + 'static)) -> Self {
        Self(
            format!("{:?}", Escaped(&format_args!("{e}"))),
            e.source().map(|s| Box::new(Self::of(s))),
        )
    }
}

impl std::fmt::Display for EscapedChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EscapedChain {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.1.as_deref().map(|e| e as _)
    }
}

/// A value whose Debug writes through [`EscapingWriter`].
struct Escaped<'a>(&'a dyn std::fmt::Debug);

impl std::fmt::Debug for Escaped<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(EscapingWriter(f), "{:?}", self.0)
    }
}

struct EscapingWriter<'a, 'b>(&'a mut std::fmt::Formatter<'b>);

impl std::fmt::Write for EscapingWriter<'_, '_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let mut clean = 0;
        for (at, c) in s.char_indices() {
            if crate::is_terminal_unsafe(c) {
                self.0.write_str(&s[clean..at])?;
                write!(self.0, "{}", c.escape_debug())?;
                clean = at + c.len_utf8();
            }
        }
        self.0.write_str(&s[clean..])
    }
}

/// The tracing directive string to build the `EnvFilter` from: a NON-EMPTY
/// `RUST_LOG` wins, otherwise the requested `log_level`. An empty `RUST_LOG` is
/// treated as unset — left as-is it parses to zero directives (everything OFF) and
/// silently defeats logging.
fn filter_directives<'a>(rust_log: Option<&'a str>, log_level: &'a str) -> &'a str {
    match rust_log {
        Some(v) if !v.is_empty() => v,
        _ => log_level,
    }
}

/// The path is [`LogLocation::from_env`]'s, which env decides, so it is stripped
/// before it reaches the terminal.
fn log_open_failure(path: &Path, e: &std::io::Error) -> String {
    format!(
        "⚠ pixtuoid: cannot open log file {} ({e}) — runtime warnings will not be recorded",
        crate::display_path(path)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_open_failure_notice_strips_the_path() {
        let notice = log_open_failure(
            Path::new("/tmp/\u{1b}]0;pwned\u{7}\u{202e}.log"),
            &std::io::ErrorKind::PermissionDenied.into(),
        );
        assert_eq!(crate::strip_control_chars(&notice), notice);
        assert!(notice.contains("/tmp/]0;pwned.log"), "{notice:?}");
    }

    /// Every character a field value is escaped of.
    fn terminal_unsafe() -> String {
        ('\0'..='\u{2069}')
            .filter(|c| crate::is_terminal_unsafe(*c))
            .collect()
    }

    /// An error and its source, which the default formatter lists.
    #[derive(Debug)]
    struct Caused(String, std::io::Error);

    impl std::fmt::Display for Caused {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl std::error::Error for Caused {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.1)
        }
    }

    /// Whatever carries a value (a `%` or `?` field, a bare `str`, the message
    /// as arguments or a `str`, a span's field, an error or its cause), it
    /// reaches the log escaped, not dropped.
    #[test]
    fn every_field_value_is_escaped_whatever_carries_it() {
        let hostile = terminal_unsafe();
        let escaped: String = hostile.chars().flat_map(char::escape_debug).collect();
        let error = Caused(hostile.clone(), std::io::Error::other(hostile.clone()));
        let cause = Caused("outer".to_owned(), std::io::Error::other(hostile.clone()));
        let out = crate::test_capture::capture(|| {
            let span = tracing::warn_span!("span", field = %hostile);
            let _in = span.enter();
            tracing::warn!(
                display = %hostile,
                debug = ?hostile,
                bare = hostile.as_str(),
                "message {hostile}"
            );
            tracing::warn!(
                error = &error as &(dyn std::error::Error + 'static),
                "error"
            );
            tracing::warn!(
                error = &cause as &(dyn std::error::Error + 'static),
                "cause"
            );
            tracing::warn!(message = hostile.as_str());
        });
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4, "a value broke its line: {out:?}");
        for line in lines {
            assert!(!line.chars().any(crate::is_terminal_unsafe), "{line:?}");
        }
        for carrier in [
            format!("span{{field={escaped}}}"),
            format!("message {escaped} display={escaped} debug="),
            format!("error={escaped} error.sources=[{escaped}]"),
            format!("error=outer error.sources=[{escaped}]"),
            format!(": {escaped}\n"),
        ] {
            assert!(out.contains(&carrier), "{carrier:?} not in {out:?}");
        }
    }

    /// Clean text formats byte for byte as the default formatter writes it, so
    /// `doctor` parses the breadcrumbs it always parsed.
    #[test]
    fn clean_fields_format_as_the_default_does() {
        use crate::test_capture::capture_with;
        let caused = Caused("outer".to_owned(), std::io::Error::other("inner"));
        let emit = || {
            let span = tracing::info_span!("span", id = 7, name = %"a \"quoted\" \\ name");
            let _in = span.enter();
            tracing::warn!(
                display = %"\"quotes\", \\ and ünïcode 日本",
                debug = ?"x\"y",
                bare = "plain",
                n = -3,
                ok = true,
                ratio = 1.5,
                "message {} ünïcode",
                42
            );
            tracing::warn!(
                error = &caused as &(dyn std::error::Error + 'static),
                "error"
            );
            tracing::warn!(message = "a message `str`");
            pixtuoid_core::source::drift::unknown_event("codex", "MysteryHook");
        };
        for ansi in [false, true] {
            assert_eq!(
                capture_with(fmt_layer().with_ansi(ansi).without_time(), emit),
                capture_with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(ansi)
                        .without_time(),
                    emit
                ),
                "ansi={ansi}"
            );
        }
    }

    #[test]
    fn empty_rust_log_falls_back_to_requested_level() {
        assert_eq!(filter_directives(Some(""), "debug"), "debug");
        assert_eq!(filter_directives(None, "debug"), "debug");
        assert_eq!(filter_directives(Some("trace"), "warn"), "trace");
        assert_eq!(
            filter_directives(Some("info,pixtuoid=debug"), "warn"),
            "info,pixtuoid=debug"
        );
    }
}
