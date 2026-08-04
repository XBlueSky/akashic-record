use std::fmt;

/// One specific reason production-mode validation rejected the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// A required field is empty.
    Empty { field: &'static str },
    /// A field contains a known placeholder value.
    Placeholder { field: &'static str, value: String },
    /// A DSN-shaped field contains `localhost`/`127.0.0.1`/`::1` while
    /// the process is running inside a container. For non-special URL
    /// schemes (`bolt:`, `postgres:`) `url::Url::host_str()` preserves the
    /// bracketed `[::1]` form; for `http`/`https` it strips the brackets
    /// to `::1`. `is_localhost_host` matches both.
    ContainerLocalhost { field: &'static str, host: String },
    /// A field has invalid shape (e.g., `DATABASE_URL` does not parse).
    InvalidSecretShape { field: &'static str, reason: String },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { field } => {
                write!(f, "{field}: empty (required in production)")
            }
            Self::Placeholder { field, value } => {
                write!(f, "{field}: forbidden placeholder value \"{value}\"")
            }
            Self::ContainerLocalhost { field, host } => write!(
                f,
                "{field}: contains \"{host}\" while running inside a container"
            ),
            Self::InvalidSecretShape { field, reason } => {
                write!(f, "{field}: invalid shape ({reason})")
            }
        }
    }
}

/// Top-level error type returned by `Config::from_env`.
///
/// `main.rs` matches on this and prints the `Display` form to stderr before
/// exiting with status `2` (config error, distinct from `1` generic runtime
/// error so init-system scripts can distinguish "will never start" from
/// "transient failure").
#[derive(Debug)]
pub enum ConfigError {
    /// A required env var was not set.
    MissingEnv { var: &'static str },
    /// An env var failed to parse into its target type (port, integer, etc.).
    ParseError {
        var: &'static str,
        value: String,
        source: String,
    },
    /// `validate_for_production` collected one or more violations.
    ValidationFailed { violations: Vec<Violation> },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEnv { var } => {
                write!(
                    f,
                    "Akashic Record: required environment variable {var} is not set. Aborting startup."
                )
            }
            Self::ParseError { var, value, source } => write!(
                f,
                "Akashic Record: failed to parse environment variable {var}=\"{value}\": {source}. Aborting startup."
            ),
            Self::ValidationFailed { violations } => {
                // Leading blank line separates this diagnostic from any
                // preceding stderr output (tracing init, dotenvy warnings,
                // etc.) — operators see a clear visual block.
                writeln!(f)?;
                writeln!(
                    f,
                    "Akashic Record: production config validation failed (AKASHIC_ENV=production):"
                )?;
                writeln!(f)?;
                for v in violations {
                    writeln!(f, "  - {v}")?;
                }
                writeln!(f)?;
                writeln!(
                    f,
                    "Refer to .env.example for the required variables, and to"
                )?;
                writeln!(
                    f,
                    "docs/operations/rotation-sop.md for how to populate them in production."
                )?;
                write!(f, "Aborting startup.")
            }
        }
    }
}

impl std::error::Error for ConfigError {}
