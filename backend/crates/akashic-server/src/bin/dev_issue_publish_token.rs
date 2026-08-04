//! Dev-ops CLI: mint a `docs-kit` publish token for a repo directly against
//! the database, bypassing HTTP auth entirely.
//!
//! Interim operational path until the G-part admin UI exists for minting
//! publish tokens (`akp_<32hex>`) — see Task 11, docs-kit Plan 2. Wraps
//! `AuthStore::issue_publish_token` directly; the caller is expected to
//! already have `DATABASE_URL` access. This is NOT an HTTP-exposed
//! capability and MUST NOT be reachable from production request paths —
//! dev/ops use only (minting a token for a CI packer to test against a
//! local or staging stack).
//!
//! ```text
//! DATABASE_URL=postgres://... cargo run -p akashic-server --bin dev_issue_publish_token -- <repo>
//! ```
//!
//! Prints the plaintext `akp_...` token to stdout on success. The plaintext
//! is shown once and never persisted — only its hash is stored (see
//! `AuthStore::issue_publish_token`).

use akashic_record::auth::AuthStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let repo = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: dev_issue_publish_token <repo>"))?;

    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL must be set"))?;
    let pg = akashic_store_pg::connect(&database_url).await?;
    let auth_store = AuthStore::new(pg, 60);

    let (_id, token) = auth_store.issue_publish_token(&repo, "dev-cli").await?;
    println!("{token}");
    Ok(())
}
