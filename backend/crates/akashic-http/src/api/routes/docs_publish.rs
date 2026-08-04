//! Task 7 (B2, docs-corpus): `POST /api/v1/ingest/publish` — the docs-kit
//! CI packer's upload endpoint.
//!
//! Task 1 addendum (docs-kit Plan 2, B2 addendum): `POST
//! /api/v1/ingest/validate` — a dry-run twin of `publish` registered in this
//! same router branch (same body-limit layer, same `apply_ingest_rate_limit`
//! wrapping at the `build_router` composition site, same `akp_` bearer auth).
//! It runs the identical auth → extract → repo-gate → validate pipeline but
//! stops before `CorpusIngestService::ingest_extracted`'s
//! store/derive-spawn/idempotency machinery — see `validate`'s doc comment.
//!
//! Bearer-authenticated with a repo-scoped `akp_` publish token (Task 6),
//! NOT a session token — `require_auth` (used by `protected_router()`) only
//! accepts session bearers/cookies and would reject `akp_` tokens with 401.
//! This route is therefore wired as its own top-level branch in
//! `akashic_http::build_router`, alongside the GitLab webhook branch, with
//! the same IP-keyed ingest rate limit and this module's own raised body
//! limit (the default axum limit is 2 MiB; a corpus.tar.gz needs headroom up
//! to the ingest service's own 64 MiB total-uncompressed cap).
//!
//! Handler ordering (mirrors `ingestion.rs`'s NON-NEGOTIABLE idempotency
//! choreography, commit c151ea4):
//!   1. token auth (`validate_publish_token`) — the body-size ceiling is
//!      already enforced structurally by the router's `DefaultBodyLimit`
//!      layer, which runs before extraction, i.e. before the handler body
//!      executes at all.
//!   2. untar + parse `manifest.json` (`extract_tar_gz`, run inside
//!      `spawn_blocking` — see this module's doc comment on `publish`)
//!   3. repo-authorization gate: `manifest.repo == token.repo_name`, checked
//!      HERE — before idempotency fast-path/reserve — not deferred to
//!      `ingest_extracted` (step 6). The idempotency key is derived from the
//!      still-unverified `manifest.repo`; reserving on it before this check
//!      would let a caller with a valid token for some OTHER repo race a
//!      real publisher for the claimed repo's idempotency key. See the
//!      inline comment at the check site in `publish` for the full
//!      griefing-vector rationale.
//!   4. idempotency fast-path, keyed on the peeked `(repo, sha)`
//!   5. atomic reserve
//!   6. `CorpusIngestService::ingest_extracted` (full contract validation +
//!      persist — re-checks `manifest.repo == token.repo_name` itself,
//!      unchanged, as defense-in-depth for any other caller of the service)
//!   7. finalize (cache the response) / release (on failure)
//!
//! Two different-shaped 413s: a `RejectCode::TooLarge` rejection (the
//! ingest service's own 64 MiB *uncompressed* total-bytes cap, tripped
//! inside `extract_tar_gz`/step 2) renders as this module's `{error:{code,
//! message,findings}}` envelope. A raw request body over `MAX_BODY_BYTES`
//! (below) never reaches the handler at all — axum's `DefaultBodyLimit`
//! layer rejects it first, with axum's own plain-text 413, NOT this JSON
//! envelope. A CI integrator parsing `body.error.code` must not assume it's
//! present on every 413.

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tracing::warn;

use akashic_context::AppState;
use akashic_domain::types::corpus::ContractFinding;
use akashic_ingestion::ingestion::corpus::{PublishRejection, RejectCode, extract_tar_gz};

use super::super::error::AppError;
use super::ingestion::{idem_fast_path, idem_finalize, idem_release, idem_reserve};

/// Raised body-size ceiling for this route only (default axum limit is 2
/// MiB). Headroom above the ingest service's own 64 MiB
/// total-uncompressed-bytes cap (`CorpusIngestService`'s `MAX_TOTAL_BYTES`)
/// isn't needed here — the uploaded artifact IS the gzip-compressed stream,
/// always smaller than its decompressed contents, so the same 64 MiB figure
/// is a safe compressed-body ceiling too.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Task 11 (C5): publish outcome counter, labeled `outcome`. Named with the
/// `akashic_` prefix to match every other custom counter in this codebase
/// (`akashic_quota_exceeded_total`, `akashic_llm_tokens_total`,
/// `akashic_audit_log_writes_total`, …) even though the spec shorthand was
/// `docs_publish_total` — see the task 11 report for the rationale.
const M_DOCS_PUBLISH_TOTAL: &str = "akashic_docs_publish_total";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/ingest/publish", post(publish))
        .route("/api/v1/ingest/validate", post(validate))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

// ── Response / error contract (B2) ──────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct PublishResponse {
    repo: String,
    version: String,
    sha: String,
    pages: usize,
    assets: usize,
    warnings: Vec<ContractFinding>,
    derive_job_id: Option<uuid::Uuid>,
    replayed: bool,
}

/// Task 1 (B2 addendum): `validate`'s success body — [`ValidationReport`]'s
/// fields verbatim, with no `derive_job_id`/`replayed` (neither concept
/// applies to a dry run).
#[derive(Serialize, Deserialize)]
struct ValidateResponse {
    repo: String,
    version: String,
    sha: String,
    pages: usize,
    assets: usize,
    warnings: Vec<ContractFinding>,
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
    findings: Vec<ContractFinding>,
}

/// Map a corpus-ingest [`RejectCode`] to the HTTP status this endpoint
/// returns for it (B2 error contract): `Schema`/`Contract` are 422
/// (unprocessable — the artifact itself is malformed or fails the docs
/// contract), `RepoMismatch` is 403 (the caller's token doesn't authorize
/// this repo), `TooLarge` is 413, and `Internal` (store/infra failure) is
/// 500.
fn reject_code_status(code: RejectCode) -> StatusCode {
    match code {
        RejectCode::Schema | RejectCode::Contract => StatusCode::UNPROCESSABLE_ENTITY,
        RejectCode::RepoMismatch => StatusCode::FORBIDDEN,
        RejectCode::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        RejectCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn reject_code_str(code: RejectCode) -> &'static str {
    match code {
        RejectCode::Schema => "schema",
        RejectCode::Contract => "contract",
        RejectCode::RepoMismatch => "repo_mismatch",
        RejectCode::TooLarge => "too_large",
        RejectCode::Internal => "internal",
    }
}

/// Render a [`PublishRejection`] as the B2 4xx error contract:
/// `{error:{code,message,findings}}`. Single funnel for every rejection
/// return site in `publish` (extraction, repo-mismatch, contract-validation
/// failure) — instrumenting the `outcome="rejected"` counter here covers all
/// three without touching each call site individually.
fn reject_response(rej: PublishRejection) -> Response {
    metrics::counter!(M_DOCS_PUBLISH_TOTAL, "outcome" => "rejected").increment(1);
    let status = reject_code_status(rej.code);
    let body = ErrorBody {
        error: ErrorDetail {
            code: reject_code_str(rej.code),
            message: rej.message,
            findings: rej.findings,
        },
    };
    (status, Json(body)).into_response()
}

/// Task 11 (C5): maps a publish outcome to the `akashic_docs_publish_total`
/// `outcome` label. "replayed" covers both the HTTP-level idempotency-cache
/// short-circuit (idem_fast_path/idem_reserve returning a prior cached
/// response) and a fresh call where the store-level idempotent-insert
/// (`PublishAccepted::replayed`) determined the (repo, sha) was already
/// persisted; "accepted" is reserved for genuinely new persisted work.
fn publish_outcome_str(replayed: bool) -> &'static str {
    if replayed { "replayed" } else { "accepted" }
}

// ── Handler ──────────────────────────────────────────────────────────────

async fn publish(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    // ── 1. token auth ────────────────────────────────────────────────────
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)?;
    let token = state
        .auth_store
        .validate_publish_token(presented)
        .await
        .ok_or(AppError::Unauthorized)?;

    // Best-effort audit (mirrors publish_tokens::issue_token/revoke_token) —
    // never fails the request. `actor_user_id=0` is the documented sentinel
    // for "not a session-authenticated user" (this call is bearer-token
    // authenticated, not session-authenticated).
    if let Err(e) = state
        .auth_store
        .record_publish_token_audit(
            0,
            &token.token_id.to_string(),
            "publish_token",
            "ingest_corpus",
            &token.repo_name,
        )
        .await
    {
        warn!(event = "publish_token_audit_write_failed", error = %e);
    }

    // ── 2. untar + parse manifest.json ──────────────────────────────────
    // The gunzip+untar pass is the CPU-heavy, security-critical boundary
    // (up to 64 MiB of decompression + per-entry validation — see
    // `akashic_ingestion::ingestion::corpus`'s module doc comment) and must
    // not run inline on a tokio worker thread. Extraction happens exactly
    // once here; its result feeds both the repo-authorization gate and
    // idempotency key below (peeked `manifest.repo`/`sha`) and
    // `ingest_extracted` (step 6) — no second extraction pass.
    let extracted = match tokio::task::spawn_blocking(move || extract_tar_gz(&body))
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("extract_tar_gz task join error: {e}")))?
    {
        Ok(extracted) => extracted,
        Err(rejection) => return Ok(reject_response(rejection)),
    };

    // ── 3. repo-authorization gate (MUST run before idem_fast_path/idem_reserve) ─
    // The idempotency key below is keyed on the peeked `manifest.repo`/`sha`
    // — UNVERIFIED against the token at this point. If the reserve ran
    // first (as `ingest_extracted`'s own copy of this same check would allow,
    // since it runs later, step 6), a caller holding a valid token for SOME
    // repo could submit a manifest claiming ANY OTHER repo + a
    // known/guessable sha and reserve that OTHER repo's
    // `docs_publish:{other_repo}:{sha}` key — racing a real concurrent
    // request from that other repo's legitimate publisher and, if it wins,
    // handing them a spurious 409 (`IdempotencyReservation::InProgress`)
    // even though this request is doomed to fail moments later anyway.
    // Gating here means a mismatched request never calls idem_reserve at
    // all, so it can never collide with (or even transiently touch) another
    // repo's idempotency key. `ingest_extracted`'s own check (step 6) stays
    // in place unchanged as defense-in-depth / a service-layer invariant for
    // any other caller (e.g. a future pull-path reuse) that doesn't go
    // through this gate.
    if extracted.manifest.repo != token.repo_name {
        return Ok(reject_response(PublishRejection {
            code: RejectCode::RepoMismatch,
            message: format!(
                "manifest.repo \"{}\" does not match expected repo \"{}\"",
                extracted.manifest.repo, token.repo_name
            ),
            findings: Vec::new(),
        }));
    }

    let repo = extracted.manifest.repo.clone();
    let sha = extracted.manifest.sha.clone();
    let idem_key = Some(format!("docs_publish:{repo}:{sha}"));
    let cached_fallback = || PublishResponse {
        repo: repo.clone(),
        version: String::new(),
        sha: sha.clone(),
        pages: 0,
        assets: 0,
        warnings: Vec::new(),
        derive_job_id: None,
        replayed: true,
    };

    // ── 4. idempotency fast-path ─────────────────────────────────────────
    if let Some(cached) = idem_fast_path(&state, idem_key.as_ref(), cached_fallback()).await? {
        metrics::counter!(M_DOCS_PUBLISH_TOTAL, "outcome" => "replayed").increment(1);
        return Ok(Json(cached).into_response());
    }

    // ── 5. atomic reserve (before the service call) ─────────────────────
    if let Some(cached) = idem_reserve(
        &state,
        idem_key.as_ref(),
        "docs_publish",
        &repo,
        cached_fallback(),
    )
    .await?
    {
        metrics::counter!(M_DOCS_PUBLISH_TOTAL, "outcome" => "replayed").increment(1);
        return Ok(Json(cached).into_response());
    }

    // ── 6. full contract validation + persist ────────────────────────────
    // `expected_repo` is the AUTHENTICATED token's repo, not the (as yet
    // unverified) manifest's own `repo` field — `ingest_extracted` compares
    // the two and rejects with `RejectCode::RepoMismatch` on mismatch.
    let accepted = match state
        .corpus_ingest_service
        .ingest_extracted(extracted, &token.repo_name, false)
        .await
    {
        Ok(accepted) => accepted,
        Err(rejection) => {
            idem_release(&state, idem_key.as_ref()).await;
            return Ok(reject_response(rejection));
        }
    };

    metrics::counter!(
        M_DOCS_PUBLISH_TOTAL,
        "outcome" => publish_outcome_str(accepted.replayed),
    )
    .increment(1);

    let response = PublishResponse {
        repo: accepted.repo,
        version: accepted.version,
        sha: accepted.sha,
        pages: accepted.pages,
        assets: accepted.assets,
        warnings: accepted.warnings,
        derive_job_id: accepted.derive_job_id,
        replayed: accepted.replayed,
    };

    // ── 7. finalize ───────────────────────────────────────────────────────
    idem_finalize(&state, idem_key.as_ref(), &response).await?;

    Ok(Json(response).into_response())
}

/// Task 1 (docs-kit Plan 2, B2 addendum): `POST /api/v1/ingest/validate` —
/// `publish`'s dry-run twin. Same auth, same untar/repo-gate steps, same
/// reject envelope (`reject_response`/`reject_code_status` — so
/// `check --remote` callers get byte-identical rejections to a real
/// `publish` call for the same broken artifact) — but stops at
/// `CorpusIngestService::validate_extracted` instead of `ingest_extracted`:
/// no idempotency fast-path/reserve, no `store.insert_version`, no derive
/// spawn, and (unlike `publish`'s step 1) no `record_publish_token_audit`
/// call — a dry run mutates nothing, so there is nothing for that
/// `ingest_corpus` audit action to describe.
async fn validate(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    // ── 1. token auth ────────────────────────────────────────────────────
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)?;
    let token = state
        .auth_store
        .validate_publish_token(presented)
        .await
        .ok_or(AppError::Unauthorized)?;

    // ── 2. untar + parse manifest.json (same CPU-heavy boundary as publish) ─
    let extracted = match tokio::task::spawn_blocking(move || extract_tar_gz(&body))
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("extract_tar_gz task join error: {e}")))?
    {
        Ok(extracted) => extracted,
        Err(rejection) => return Ok(reject_response(rejection)),
    };

    // ── 3. repo-authorization gate ──────────────────────────────────────
    // Mirrors `publish`'s step 3. There's no idempotency reserve downstream
    // to race-protect here (a dry run never reserves anything), but gating
    // on the cheap manifest.repo/token check before running the full
    // UTF-8/nav/link-checking pipeline still avoids paying for that work on
    // a request that's rejected either way.
    if extracted.manifest.repo != token.repo_name {
        return Ok(reject_response(PublishRejection {
            code: RejectCode::RepoMismatch,
            message: format!(
                "manifest.repo \"{}\" does not match expected repo \"{}\"",
                extracted.manifest.repo, token.repo_name
            ),
            findings: Vec::new(),
        }));
    }

    // ── 4. validation only — no insert, no derive spawn, no idempotency ──
    let report = match state
        .corpus_ingest_service
        .validate_extracted(extracted, &token.repo_name)
        .await
    {
        Ok(report) => report,
        Err(rejection) => return Ok(reject_response(rejection)),
    };

    Ok(Json(ValidateResponse {
        repo: report.repo,
        version: report.version,
        sha: report.sha,
        pages: report.pages,
        assets: report.assets,
        warnings: report.warnings,
    })
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_outcome_str_accepted_vs_replayed() {
        assert_eq!(publish_outcome_str(false), "accepted");
        assert_eq!(publish_outcome_str(true), "replayed");
    }

    #[test]
    fn reject_code_maps_to_expected_http_status() {
        assert_eq!(
            reject_code_status(RejectCode::Schema),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            reject_code_status(RejectCode::Contract),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            reject_code_status(RejectCode::RepoMismatch),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            reject_code_status(RejectCode::TooLarge),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            reject_code_status(RejectCode::Internal),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
