# Kaspa Genesis Proof (Rust-Native Verifier)

Rust-native CLI for cryptographically verifying Kaspa genesis integrity and proving no premine, with full step-by-step terminal output.

## Supported Inputs

- `--node-type rust`: `rusty-kaspa` RocksDB datadirs
- `--node-type go`: legacy `kaspad` LevelDB datadirs
- `--node-type auto`: probes and selects the matching store type automatically

## Quick Start

### Build
```bash
cargo build --release
```

### Run (auto-detect node/datadir)
```bash
./target/release/rust-native-verifier
```

### Run with explicit settings
```bash
# Rust node
./target/release/rust-native-verifier --node-type rust --datadir ~/.rusty-kaspa/kaspa-mainnet/datadir

# Go node (legacy)
./target/release/rust-native-verifier --node-type go --datadir ~/.kaspad/kaspa-mainnet/datadir2

# Non-interactive run with JSON report output
./target/release/rust-native-verifier --no-input --json-out ./kaspa-proof-report.json
```

## Verification Flow

The verifier checks:

1. node database connectivity and layout detection
2. current tip state and sync advisory
3. active genesis header hash
4. hardwired genesis coinbase linkage when applicable
5. pruning-point hash chain from current tip back to genesis
6. genesis UTXO commitment analysis
7. embedded checkpoint chain back to original empty genesis

## UX Behavior

- Shows every verification step in the terminal.
- If `--datadir` is omitted, default OS Kaspa paths are probed automatically.
- If node appears behind tip, prompts whether to continue against latest local synced tip.
- `--no-input` disables prompts and continues automatically through sync advisories.
- `--json-out PATH` writes a JSON report without prompting.
- Without `--json-out`, interactive runs prompt whether to export a JSON report at the end.
- JSON export includes structured fields plus full on-screen output transcript (excluding interactive prompts).

## Project Layout

- `src/main.rs`: CLI entrypoint and shared runtime constants
- `src/store.rs`: Rust/Go store opening, path resolution, and database decoding
- `src/hashing.rs`: header/transaction hashing and Rust header decoding helpers
- `src/verify.rs`: end-to-end verification flow
- `src/model.rs`: shared data structures and report types
- `src/output.rs`: terminal output and JSON report helpers

## Testing

```bash
cargo fmt --check
cargo test --locked
```

The test suite includes:

- store/path resolution regression tests
- Go fixture compatibility tests
- verification-flow tests in `src/verify.rs`
- process-level CLI tests in `tests/cli.rs` covering `--no-input` and `--json-out`

## Releases

GitHub release artifacts are built by `.github/workflows/rust-native-release.yml` for:

- Windows x86_64
- Linux x86_64
- macOS Intel (x86_64)
- macOS Apple Silicon (aarch64)

Release packaging is gated by formatting and test checks in GitHub Actions before artifacts are built.
