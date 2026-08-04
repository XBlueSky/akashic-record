//! Golden test: `CorpusManifest`'s schemars-generated JSON Schema must match
//! the committed `backend/schemas/manifest.schema.json` — the canonical
//! contract both the CI packer and the platform validator consume (A6).

#[test]
fn manifest_schema_matches_committed_golden() {
    let generated = akashic_domain::types::corpus::manifest_json_schema();
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("../../../schemas/manifest.schema.json")).unwrap();
    assert_eq!(
        generated, golden,
        "regenerate: cargo run -p akashic-server --bin dump_manifest_schema > backend/schemas/manifest.schema.json"
    );
}

/// Golden test: `DocsToml`'s schemars-generated JSON Schema must match the
/// committed `backend/schemas/docs-toml.schema.json` — the canonical
/// `.akashic/docs.toml` pull-bootstrap contract, consumed by both the
/// platform (`corpus_pull::load_contract`) and the docs-kit kit's `akashic`
/// plugin via the `get_docs_schema` MCP tool (E5/A6).
#[test]
fn docs_toml_schema_matches_committed_golden() {
    let generated = akashic_domain::types::corpus::docs_toml_json_schema();
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("../../../schemas/docs-toml.schema.json")).unwrap();
    assert_eq!(
        generated, golden,
        "regenerate: cargo run -p akashic-server --bin dump_docs_toml_schema > backend/schemas/docs-toml.schema.json"
    );
}
