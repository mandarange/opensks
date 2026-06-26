# OpenSKS Terminal Command Catalog

## Git
- git status
- git diff
- git diff --stat
- git log --oneline -20
- git branch --show-current

## Rust / Cargo
- cargo fmt
- cargo fmt --check
- cargo check
- cargo test
- cargo test -p <package>
- cargo clippy --all-targets --all-features

## OpenSKS
- cargo run -- terminal smoke
- cargo run -- provider list
- cargo run -- provider probe
- cargo run -- qa run
- cargo run -- codegraph index
- cargo run -- codegraph query <text>
- cargo run -- daemon --stdio --workspace "$PWD"

## Safety
- destructive commands require approval
- secret exposure commands must be blocked or approval-gated
