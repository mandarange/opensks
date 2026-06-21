//! Regenerate `swift/Sources/DesignSystem/Generated/GeneratedDesignTokens.swift`
//! from the canonical token IR. Run from anywhere:
//!
//! ```sh
//! cargo run -p opensks-design --bin gen-swift-tokens
//! ```
//!
//! The committed Swift file is verified against this output by the
//! `generated_swift_matches_token_set` drift test in the crate.

use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR"); // crates/opensks-design
    let root = Path::new(manifest_dir)
        .parent()
        .and_then(Path::parent)
        .expect("workspace root is two levels above the crate manifest");

    let tokens_path = root.join(".opensks/design-systems/opensks-studio-dark/tokens.json");
    let out_path = root.join("swift/Sources/DesignSystem/Generated/GeneratedDesignTokens.swift");

    let tokens_json = fs::read_to_string(&tokens_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", tokens_path.display()));
    let set: opensks_design::DesignTokenSet =
        serde_json::from_str(&tokens_json).expect("parse tokens.json");
    let swift = opensks_design::compile_swift_tokens(&set);

    fs::create_dir_all(out_path.parent().unwrap()).expect("create generated dir");
    fs::write(&out_path, swift).unwrap_or_else(|e| panic!("write {}: {e}", out_path.display()));
    println!("wrote {}", out_path.display());
}
