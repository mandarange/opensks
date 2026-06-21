use std::process;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json_errors = args.iter().any(|arg| arg == "--json");
    let double_click_launch = args.is_empty();
    let cwd = match if double_click_launch {
        opensks::default_launch_cwd()
    } else {
        opensks::default_cwd()
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
