use std::path::PathBuf;

pub fn fixture_workspace(name: &str) -> PathBuf {
    let sanitized = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    std::env::temp_dir().join(format!("opensks-fixture-{sanitized}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_names_are_portable() {
        let path = fixture_workspace("daemon health");
        assert!(path.to_string_lossy().contains("daemon-health"));
    }
}
