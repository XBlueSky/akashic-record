//! Prints the canonical `CorpusManifest` JSON Schema to stdout.
//!
//! Regenerate the committed golden file after any `CorpusManifest` change:
//!
//! ```text
//! cargo run -p akashic-server --bin dump_manifest_schema > backend/schemas/manifest.schema.json
//! ```

fn main() {
    let schema = akashic_domain::types::corpus::manifest_json_schema();
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).expect("schema serializes to JSON")
    );
}
