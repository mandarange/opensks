use std::process;

fn main() {
    let cwd = match opensks::default_cwd() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("failed to read current directory: {error}");
            process::exit(1);
        }
    };

    match opensks::run_cli(std::env::args().skip(1), &cwd) {
        Ok(output) => {
            print!("{}", output.stdout);
        }
        Err(error) => {
            eprintln!("{error}");
            let code = match error {
                opensks::OpenSksError::Usage(_) => 2,
                _ => 1,
            };
            process::exit(code);
        }
    }
}
