# Kaspa Genesis Proof (Rust CLI)

Rust CLI for running the Kaspa genesis proof against `rusty-kaspa` and legacy `kaspad` data.

## Origin

- Original proof notebook by Shai Wyborski and Michael Sutton
- Notebook: `https://github.com/kaspagang/kaspad-py-explorer/blob/main/src/genesis_proof.ipynb`
- Paired Go store: `https://github.com/kaspagang/kaspad-py-explorer/blob/main/src/store.py`
- The notebook is the source of truth

This Rust CLI follows that proof flow. It also adds one extra verification step: it verifies the canonical historical `utxos.gz` against the hardwired checkpoint UTXO commitment before reporting the checkpoint total.

## Supported Inputs

- `--node-type rust` for `rusty-kaspa` RocksDB datadirs
- `--node-type go` for legacy `kaspad` LevelDB datadirs
- `--node-type auto` to auto-detect the layout

## Quick Start

Build from source:

```bash
cargo build --release
```

Run in default mode:

```bash
./target/release/rust-native-verifier
```

Run with an explicit datadir:

```bash
# rusty-kaspa
./target/release/rust-native-verifier --node-type rust --datadir ~/.rusty-kaspa/kaspa-mainnet/datadir

# kaspad
./target/release/rust-native-verifier --node-type go --datadir ~/.kaspad/kaspa-mainnet/datadir2
```

Non-interactive run with JSON output:

```bash
./target/release/rust-native-verifier --no-input --json-out ./kaspa-proof-report.json
```

If you are using a release archive instead of building from source:

- macOS/Linux: run `./run-verifier.sh`
- Windows: run `run-verifier.bat`

## Independent Inputs

Default mode uses embedded verification data for convenience:

- embedded `checkpoint_data.json`
- embedded canonical `kaspad v0.11.5-2` `utxos.gz`

If you want to supply your own inputs, use:

```bash
./target/release/rust-native-verifier \
  --pre-checkpoint-datadir /path/to/datadir2 \
  --checkpoint-utxos-gz /path/to/utxos.gz
```

The second path should be the historical file from:

`https://raw.githubusercontent.com/kaspanet/kaspad/v0.11.5-2/domain/consensus/processes/blockprocessor/resources/utxos.gz`

## What It Verifies

1. current tip to active genesis
2. hardwired genesis coinbase linkage
3. checkpoint header linkage back to original empty genesis
4. checkpoint `utxos.gz` MuHash matches the hardwired checkpoint commitment
5. checkpoint total is summed from that verified dump

The checkpoint total reported by the Rust CLI is:

- `98,422,254,404,487,171` sompi
- `984,222,544.04487171` KAS

## Releases

Prebuilt release artifacts are published by `.github/workflows/rust-native-release.yml` for:

- Linux x86_64
- macOS x86_64
- macOS aarch64
- Windows x86_64

## Testing

```bash
cargo fmt --check
cargo test --locked
```
