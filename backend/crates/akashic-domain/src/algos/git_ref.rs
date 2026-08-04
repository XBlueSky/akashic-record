//! Git-ref resolution helpers shared across layers.

/// Resolve the effective `git_ref` for a reingest request (finding #39): the
/// explicit query branch wins, falling back to the last job's ref; blank /
/// whitespace-only values are treated as absent. Returns an empty string only
/// when neither source has a usable ref (acceptable for website/local sources,
/// which do not clone).
///
/// Lives in `akashic-domain` so the production caller
/// (`akashic-ingestion::services`) and the HTTP-layer tests exercise the SAME
/// implementation — it used to exist as a `#[cfg(test)]` copy in
/// `akashic-http::api::routes::ingestion` whose 5 unit tests asserted on the
/// copy rather than the real code.
#[must_use]
pub fn resolve_reingest_git_ref(
    query_branch: Option<String>,
    last_job_ref: Option<String>,
) -> String {
    query_branch
        .filter(|b| !b.trim().is_empty())
        .or_else(|| last_job_ref.filter(|b| !b.trim().is_empty()))
        .unwrap_or_default()
}
