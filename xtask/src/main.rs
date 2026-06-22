use std::fs;
use std::path::PathBuf;

fn main() {
    let command = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "schemas".to_string());
    match command.as_str() {
        "schemas" => generate_schemas(),
        "capability-matrix" => generate_capability_matrix(),
        other => {
            eprintln!("unknown xtask `{other}`");
            eprintln!("usage: cargo run -p xtask -- <schemas|capability-matrix>");
            std::process::exit(2);
        }
    }
}

fn generate_schemas() {
    let out_dir = PathBuf::from("schemas");
    fs::create_dir_all(&out_dir).expect("create schemas dir");
    for (name, schema) in opensks_contracts::schema_jsons().expect("generate schemas") {
        fs::write(out_dir.join(name), schema + "\n").expect("write schema");
    }
    println!("generated schemas in {}", out_dir.display());
}

fn generate_capability_matrix() {
    let out = PathBuf::from("docs/runtime-truth-matrix.generated.md");
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).expect("create docs dir");
    }
    let report = opensks_contracts::baseline_capability_report();
    report
        .validate()
        .expect("baseline capability report must be valid");
    fs::write(&out, report.render_truth_matrix_markdown()).expect("write capability matrix");
    println!("generated capability matrix at {}", out.display());
}
