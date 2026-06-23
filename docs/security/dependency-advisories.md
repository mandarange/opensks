# Dependency Advisory Posture

This document records how OpenSKS handles **external dependency advisories** and
supply-chain risk. It is the human-readable companion to the machine-readable
[`deny.toml`](../../deny.toml) at the workspace root.

## Scanners

The CI security gate combines dependency advisory scanners, secret scanning, and
release artifact generation:

- **`cargo deny check`** — reads [`deny.toml`](../../deny.toml) and gates on four
  dimensions: known advisories, banned/duplicate crates, license allow-list, and
  trusted source registries.
- **`cargo audit`** — cross-checks the dependency tree against the
  [RustSEC advisory database](https://github.com/rustsec/advisory-db) for known
  vulnerabilities and unmaintained crates.
- **Gitleaks** — scans the checked-out tree with the repository
  [`.gitleaks.toml`](../../.gitleaks.toml) allowlist and writes a redacted JSON
  report artifact.
- **`cargo cyclonedx`** — generates CycloneDX JSON SBOM files for the Cargo
  workspace so release proof can bind dependency evidence to the candidate SHA.
- **CodeQL** — analyzes Rust and Swift through the dedicated `codeql` workflow.

Run them locally before pushing:

```sh
cargo install cargo-deny cargo-audit
cargo deny check
cargo audit
docker run --rm -v "$PWD:/repo" ghcr.io/gitleaks/gitleaks:v8.30.1 detect --source=/repo --config=/repo/.gitleaks.toml --no-git --redact
cargo install cargo-cyclonedx --version 0.5.9 --locked
cargo cyclonedx --format json --spec-version 1.5 --all-features
```

A new advisory **fails CI** until it is either fixed (bump the dependency) or
explicitly accepted with an owner and a tracking note in the `advisories.ignore`
list of `deny.toml`. Deny-by-default: nothing is silently ignored.

## Crypto cluster derives from the vetted `age` crate

All vault encryption goes through the well-vetted [`age`](https://crates.io/crates/age)
crate (X25519 recipients + authenticated ChaCha20-Poly1305 via the age format).
This is a deliberate, load-bearing decision:

- We **never roll our own** cipher, KDF, MAC, nonce, or key-exchange.
- The crypto-relevant transitive dependencies (the RustCrypto cluster pulled in
  by `age`) are therefore the part of the graph that advisory scanning matters
  most for, and they are covered by both scanners above.
- `age` is declared with default features only in the workspace `Cargo.toml`;
  we deliberately avoid `cli-common` so no console/tty dependencies leak in.

Because the crypto surface is concentrated in one vetted upstream crate, a
RustSEC advisory against `age` or its RustCrypto deps is the highest-signal
event this posture is designed to catch.

## License policy

Only OSI-approved permissive licenses are allowed (see the `licenses.allow` list
in `deny.toml`). Copyleft (other than MPL-2.0 file-level) and unknown licenses
fail CI. The workspace itself is MIT.

## Relationship to the `security` audit gate

The Rust `opensks security report` / `opensks security audit` commands emit a
structured `opensks.security-report.v1` report. One of its built-in checks,
`dependency_advisories_scanned`, asserts that this advisory posture is in place;
the report also carries an accepted (non-gating) `dependency-advisory-posture`
finding that points back to this document. The cheap built-in audit gate fails
nonzero when any `critical`/`high` finding is still `open`, while the deeper
advisory scanning above is owned by `cargo deny` / `cargo audit` in CI.
