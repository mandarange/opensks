use std::fs;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("schemas");
    match command {
        "schemas" => generate_schemas(),
        "capability-matrix" => generate_capability_matrix(&args[1..]),
        "architecture-graph" => architecture_graph(&args[1..]),
        "direct-io-audit" => direct_io_audit(&args[1..]),
        other => {
            eprintln!("unknown xtask `{other}`");
            eprintln!(
                "usage: cargo run -p xtask -- <schemas|capability-matrix|architecture-graph|direct-io-audit>"
            );
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

fn generate_capability_matrix(args: &[String]) {
    let out = PathBuf::from("docs/runtime-truth-matrix.generated.md");
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).expect("create docs dir");
    }
    let report = opensks_cli::runtime_capability_report(&std::env::current_dir().unwrap(), args);
    report
        .validate()
        .expect("runtime capability report must be valid");
    fs::write(&out, report.render_truth_matrix_markdown()).expect("write capability matrix");
    println!("generated capability matrix at {}", out.display());
}

fn architecture_graph(_args: &[String]) {
    let mut failures = Vec::new();
    let adapter =
        fs::read_to_string("crates/opensks-adapter/Cargo.toml").expect("read adapter Cargo.toml");
    if adapter.contains("opensks-file-service") {
        failures.push("opensks-adapter must not depend on opensks-file-service implementation");
    }
    let contracts = fs::read_to_string("crates/opensks-contracts/Cargo.toml")
        .expect("read contracts Cargo.toml");
    for forbidden in [
        "opensks-adapter",
        "opensks-daemon",
        "opensks-provider",
        "opensks-policy",
        "opensks-patch-engine",
    ] {
        if contracts.contains(forbidden) {
            failures.push("opensks-contracts must not depend on runtime/domain crates");
        }
    }
    finish_check("architecture-graph", failures);
}

fn direct_io_audit(_args: &[String]) {
    let mut failures = Vec::new();
    for path in rust_sources("crates/opensks-adapter/src") {
        let text = fs::read_to_string(&path).expect("read source");
        let product_text = text.split("\n#[cfg(test)]").next().unwrap_or(text.as_str());
        let display = path.display().to_string();
        if product_text.contains("DefaultHasher") {
            failures.push(format!(
                "{display}: DefaultHasher content identity is forbidden"
            ));
        }
        if product_text.contains("Command::new(\"curl\")")
            || product_text.contains("CurlChatCompleter")
        {
            failures.push(format!(
                "{display}: provider curl subprocess transport is forbidden"
            ));
        }
        for (line_no, line) in product_text.lines().enumerate() {
            if line.contains("std::fs::write") {
                failures.push(format!(
                    "{display}:{}: product std::fs::write must go through opensks-patch-engine",
                    line_no + 1
                ));
            }
        }
    }
    finish_check("direct-io-audit", failures);
}

fn rust_sources(root: &str) -> Vec<PathBuf> {
    fn walk(path: PathBuf, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(&path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(PathBuf::from(root), &mut out);
    out
}

fn finish_check<T: std::fmt::Display>(name: &str, failures: Vec<T>) {
    if failures.is_empty() {
        println!("{name}: ok");
        return;
    }
    for failure in failures {
        eprintln!("{name}: {failure}");
    }
    std::process::exit(1);
}
