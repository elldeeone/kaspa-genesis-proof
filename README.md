# Kaspa Genesis Proof (Rust-Native Verifier)

Rust-native CLI for cryptographically verifying Kaspa genesis integrity and proving no premine, with full step-by-step terminal output.

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
```

## UX Behavior

- Shows every verification step in the terminal.
- If node appears behind tip, prompts whether to continue against latest local synced tip.
- At end of run, prompts whether to export a JSON report.
- JSON export includes structured fields plus full on-screen output transcript (excluding interactive prompts).

## Releases

GitHub release artifacts are built by `.github/workflows/rust-native-release.yml` for:

- Windows x86_64
- Linux x86_64
- macOS Intel (x86_64)
- macOS Apple Silicon (aarch64)
