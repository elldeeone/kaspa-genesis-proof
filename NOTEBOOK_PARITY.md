# Notebook Parity

Source of truth:
- Notebook: `https://github.com/kaspagang/kaspad-py-explorer/blob/main/src/genesis_proof.ipynb`
- Paired Go store: `https://github.com/kaspagang/kaspad-py-explorer/blob/main/src/store.py`

Rule:
- The notebook's verification behavior is the law.
- Rust-native features may be added around it, but they must not remove, weaken, or block any notebook verification step.

## Approved Mapping

Cell 7: `assert_cryptographic_hash_chain_to_genesis`
- Rust equivalent: `src/verify.rs` `assert_cryptographic_hash_chain_to_genesis`
- Status: approved

Cell 10: hardwired genesis payload constants
- Rust equivalent: `HARDWIRED_GENESIS_TX_PAYLOAD_HEX`, `HARDWIRED_GENESIS_BITCOIN_BLOCK_HASH_HEX`, `CHECKPOINT_HASH_HEX`
- Status: approved

Cell 12: build hardwired genesis coinbase transaction
- Rust equivalent: `src/verify.rs` `hardwired_genesis_coinbase_tx`
- Status: approved

Cell 14: current store + pre-checkpoint store are both real inputs
- Rust equivalent:
  - current store: `--datadir`
  - pre-checkpoint store: `--pre-checkpoint-datadir`
  - embedded checkpoint data remains supported as an additive convenience path
- Status: approved

Cell 16: verify current genesis header hash
- Rust equivalent: Step 3 in `src/verify.rs`
- Status: approved

Cell 18: verify hardwired genesis coinbase tx hash equals genesis merkle root
- Rust equivalent: Step 4 in `src/verify.rs`
- Status: approved

Cell 20: read `tips, hst = current_store.tips()` and verify from `tips[0]`
- Rust equivalent:
  - `HeaderStore::tips()`
  - `choose_chain_tip_for_verification` in `src/verify.rs`
- Status: approved
- Constraints:
  - no HST substitution when DAG tips are empty
  - no extra tip reordering layer beyond store output

Cell 22: verify checkpoint header hash and UTXO commitment equality
- Rust equivalent: Step 7 in `src/verify.rs`
- Status: approved

Cell 24: verify original genesis header hash and original coinbase tx hash/reference
- Rust equivalent:
  - `original_genesis_coinbase_tx`
  - original-genesis verification block in Step 7
- Status: approved

Cell 26: verify checkpoint-to-original-genesis hash chain
- Rust equivalent: Step 7 in `src/verify.rs`
- Status: approved

Cell 28: verify original genesis empty MuHash
- Rust equivalent: Step 7 in `src/verify.rs`
- Status: approved

Cell 30: close resources
- Rust equivalent: Rust/LevelDB handles are dropped at process end / scope end
- Status: approved

## Additional Features Allowed

These features are allowed because they do not remove notebook verification:
- auto-detection of Go vs Rust datadirs
- JSON report output
- non-interactive mode
- sync freshness advisory, provided it is warning-only
- embedded `checkpoint_data.json` fallback when `--pre-checkpoint-datadir` is not supplied

## Guardrails

The following behaviors are not allowed:
- replacing `tips[0]` semantics with `headers_selected_tip`
- reordering tips in a way the notebook/store pair does not do
- skipping original-genesis verification
- making sync advisories able to abort proof execution
- removing the real external pre-checkpoint-store path
