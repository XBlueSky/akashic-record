//! Prints the canonical `DocsToml` JSON Schema to stdout.
//!
//! Regenerate the committed golden file after any `DocsToml` change:
//!
//! ```text
//! cargo run -p akashic-server --bin dump_docs_toml_schema > backend/schemas/docs-toml.schema.json
//! ```

fn main() {
    let schema = akashic_domain::types::corpus::docs_toml_json_schema();
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).expect("schema serializes to JSON")
    );
}
