use std::{path::PathBuf, process};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json_errors = args.iter().any(|arg| arg == "--json");
    let double_click_launch = args.is_empty();
    let cwd = match if double_click_launch {
        opensks::default_launch_cwd()
    } else {
        cli_cwd(&args)
    } {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("failed to read current directory: {error}");
            process::exit(1);
        }
    };

    if opensks::is_daemon_stdio_invocation(&args) {
        if let Err(error) = opensks::run_daemon_stdio_stream(&args, &cwd) {
            if json_errors {
                eprint!("{}", opensks::cli_error_json(&error, 1));
            } else {
                eprintln!("{error}");
            }
            process::exit(1);
        }
        return;
    }

    match opensks::run_cli(args, &cwd) {
        Ok(output) => {
            print!("{}", output.stdout);
            // A no-argument launch (double-click or `./opensks`) builds the
            // macOS app bundle and opens it, unless suppressed for headless runs.
            if double_click_launch && std::env::var_os("OPENSKS_SKIP_DASHBOARD_OPEN").is_none() {
                let app_bundle = opensks::native_app_bundle_path(&cwd);
                if let Err(error) = opensks::open_path_for_user(&app_bundle) {
                    eprintln!("failed to open OpenSKS.app: {error}");
                    eprintln!("app: {}", app_bundle.display());
                }
            }
        }
        Err(error) => {
            let code = match error {
                opensks::OpenSksError::Usage(_) => 2,
                _ => 1,
            };
            if json_errors {
                eprint!("{}", opensks::cli_error_json(&error, code));
            } else {
                eprintln!("{error}");
            }
            process::exit(code);
        }
    }
}

fn cli_cwd(args: &[String]) -> Result<PathBuf, opensks::OpenSksError> {
    if let Some(value) = std::env::var_os(opensks::OPENSKS_WORKSPACE_ENV) {
        let workspace = PathBuf::from(value);
        if !workspace.as_os_str().is_empty() {
            return Ok(workspace);
        }
    }
    if args.first().is_some_and(|arg| arg == "app-data") {
        if let Some(workspace) = args.get(1).filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(workspace));
        }
    }
    if let Some(workspace) = args.windows(2).find_map(|window| {
        (window[0] == "--workspace" && !window[1].is_empty()).then(|| PathBuf::from(&window[1]))
    }) {
        return Ok(workspace);
    }
    opensks::default_cwd()
}
