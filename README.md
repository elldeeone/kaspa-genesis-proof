# Kaspa Genesis Proof

Tools for cryptographically verifying the integrity of the Kaspa blockchain on both Go-based `kaspad` and Rust-based `rusty-kaspa` nodes. The proof shows that the current UTXO set naturally evolved from an empty UTXO set and that there was no premine.

## Trusted Source Reference

Based on the original genesis proof by Shai Wyborski and Michael Sutton:

- Original Source: `https://github.com/kaspagang/kaspad-py-explorer/blob/main/src/genesis_proof.ipynb`
- Authors: Shai Wyborski, Michael Sutton
- This repository adapts that proof flow into a maintained CLI for both `kaspad` and `rusty-kaspa` nodes. It preserves the original proof path and adds checkpoint `utxos.gz` commitment verification before reporting the checkpoint total.

## Quick Start

Build:

```bash
cargo build --release
```

Run:

```bash
./target/release/genesis-proof
```

Run with an explicit datadir:

```bash
# rusty-kaspa
./target/release/genesis-proof --node-type rust --datadir ~/.rusty-kaspa/kaspa-mainnet/datadir

# kaspad
./target/release/genesis-proof --node-type go --datadir ~/.kaspad/kaspa-mainnet/datadir2
```

Non-interactive run with JSON output:

```bash
./target/release/genesis-proof --no-input --json-out ./kaspa-proof-report.json
```

If you downloaded and extracted a release archive, the launcher scripts are at the archive root:

- macOS/Linux: run `./run-verifier.sh`
- Windows: run `run-verifier.bat`

In this source repository, those launcher scripts live under `dist/` and are copied into the release archive during packaging.

## Web Verifier

The repository also includes an experimental hosted web verifier:

```bash
cargo run --bin web
```

Open:

```text
http://127.0.0.1:8080
```

The web verifier asks the user for their node host and RPC port. By default it
uses:

- user node RPC: `16110`
- backend proof-source P2P: `16111`

The backend resolves Kaspa mainnet DNS seeders, tries public P2P peers in
parallel, downloads the first pruning proof that succeeds, and caches it. User
verification requests then use the user's RPC endpoint for live chain state and
the backend cache for historical pruning-proof headers.

Optional controls:

```bash
# Pin a specific backend proof source instead of DNS-seeded public peers.
KASPA_PROOF_SOURCE_ADDR=host:16111 cargo run --bin web

# Change how many public proof-source peers are raced during startup.
KASPA_PROOF_SOURCE_PARALLELISM=8 cargo run --bin web

# Change the background pruning-proof refresh interval. Default: 1800 seconds.
KASPA_PROOF_SOURCE_REFRESH_SECONDS=1800 cargo run --bin web

# Fail startup if the backend cannot warm the pruning-proof cache.
KASPA_PROOF_REQUIRE_SOURCE_WARMUP=1 cargo run --bin web
```

Proof execution is serialized inside the web server because the CLI output
capture is process-global. Concurrent users can submit requests, but proof jobs
queue so their JSON report logs do not overlap.

## Independent Verification Inputs

Default mode uses embedded verification data:

- embedded `resources/checkpoint_data.json`
- embedded canonical `kaspad v0.11.5-2` `utxos.gz`

If you want to supply your own inputs, use:

```bash
./target/release/genesis-proof \
  --pre-checkpoint-datadir /path/to/datadir2 \
  --checkpoint-utxos-gz /path/to/utxos.gz
```

Canonical checkpoint dump source:

`https://raw.githubusercontent.com/kaspanet/kaspad/v0.11.5-2/domain/consensus/processes/blockprocessor/resources/utxos.gz`

## What It Verifies

1. active genesis header hash
2. hardwired genesis coinbase hash, Bitcoin reference, and checkpoint reference
3. current tip to active genesis hash chain
4. checkpoint header hash and UTXO commitment match against hardwired genesis
5. checkpoint chain back to the original genesis, plus original genesis coinbase and empty UTXO commitment
6. checkpoint `utxos.gz` MuHash matches the hardwired checkpoint commitment, and the checkpoint total is summed from that verified dump

The checkpoint total reported by the verifier is:

- `98,422,254,404,487,171` sompi
- `984,222,544.04487171` KAS

## Releases

Prebuilt artifacts are published by the release workflow for:

- Linux x86_64
- macOS aarch64
- Windows x86_64
