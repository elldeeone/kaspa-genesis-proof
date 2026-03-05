Kaspa Genesis Proof - Rust Native Verifier

Quick start (no flags required):

- Windows: double-click run-verifier.bat
- macOS/Linux: run ./run-verifier.sh from a terminal

What happens:

1. The verifier auto-detects your Kaspa data directory.
2. It auto-detects Rust or Go node layout.
3. It runs all verification steps and prints each step to the terminal.

If auto-detection fails, run with explicit path:

rust-native-verifier --node-type auto --datadir <path-to-kaspa-datadir>

Examples:

rust-native-verifier --node-type rust --datadir ~/.rusty-kaspa/kaspa-mainnet/datadir
rust-native-verifier --node-type go --datadir ~/.kaspad/kaspa-mainnet/datadir2
