use std::fs;
use std::path::PathBuf;

fn main() {
    let command = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "schemas".to_string());
    if command != "schemas" {
        eprintln!("usage: cargo run -p xtask -- schemas");
        std::process::exit(2);
    }

    let out_dir = PathBuf::from("schemas");
    fs::create_dir_all(&out_dir).expect("create schemas dir");
    for (name, schema) in opensks_contracts::schema_jsons().expect("generate schemas") {
        fs::write(out_dir.join(name), schema + "\n").expect("write schema");
    }
    println!("generated schemas in {}", out_dir.display());
}
