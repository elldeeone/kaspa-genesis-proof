Kaspa Genesis Proof - Rust Native Verifier

Quick start (no flags required):

- Windows: double-click run-verifier.bat
- macOS/Linux: run ./run-verifier.sh from a terminal

What happens:

1. The verifier auto-detects your Kaspa data directory.
2. It auto-detects Rust or Go node layout.
3. It runs all verification steps and prints each step to the terminal.
4. It verifies the bundled historical checkpoint `utxos.gz` against Kaspa's checkpoint UTXO commitment before reporting the checkpoint total.
5. The pre-checkpoint data and checkpoint `utxos.gz` both default to embedded copies, but both can be replaced with operator-supplied inputs for independent verification.

If auto-detection fails, run with explicit path:

rust-native-verifier --node-type auto --datadir <path-to-kaspa-datadir>

If you prefer to download the historical checkpoint dump yourself, you can point the verifier at it:

rust-native-verifier --checkpoint-utxos-gz <path-to-utxos.gz>

Examples:

rust-native-verifier --node-type rust --datadir ~/.rusty-kaspa/kaspa-mainnet/datadir
rust-native-verifier --node-type go --datadir ~/.kaspad/kaspa-mainnet/datadir2
