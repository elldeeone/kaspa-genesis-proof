# Kaspa Genesis Proof

A comprehensive toolkit for verifying the cryptographic integrity of the Kaspa blockchain, supporting both Go-based kaspad and Rust-based rusty-kaspa nodes.

## Overview

This repository provides tools to cryptographically verify that the current UTXO set of Kaspa has naturally evolved from an empty UTXO set, validating the assertion that there was no premine.

## Trusted Source Reference

**This work is based on the original genesis proof by Shai Wyborski and Michael Sutton:**
- **Original Source:** https://github.com/kaspagang/kaspad-py-explorer/blob/main/src/genesis_proof.ipynb
- **Authors:** Shai Wyborski, Michael Sutton
- **Trust Chain:** This is the authoritative genesis proof implementation used by the Kaspa community

**My Contribution:** I have extended the original work to support Rust-based rusty-kaspa nodes while maintaining 100% of the original verification logic and cryptographic guarantees.

## Quick Start

### Prerequisites
- **Fully synced Kaspa node** (Rust or Go)
- **Python 3.8+**

### 1. Clone and Setup
```bash
git clone https://github.com/elldeeone/kaspa-genesis-proof
cd kaspa-genesis-proof
```

### 2. Install Dependencies

**Recommended: Use a virtual environment:**
```bash
python3 -m venv kaspa-genesis-env
source kaspa-genesis-env/bin/activate  # On Windows: kaspa-genesis-env\Scripts\activate
```

**For Rust-based rusty-kaspa nodes:**
```bash
pip install -r requirements.txt
# Or manually: pip install numpy pandas rocksdict tqdm notebook
```

**For Go-based kaspad nodes:**
```bash
pip install numpy pandas plyvel protobuf==3.20.0 tqdm notebook
```

### 3. Configure and Run
```bash
# Edit genesis_proof.ipynb cell 2 to update your database path
# Then run the verification:
jupyter notebook genesis_proof.ipynb
```

**Common database paths:**
- **Rust nodes**: `~/.rusty-kaspa/kaspa-mainnet/datadir/consensus/consensus-003`
- **Go nodes**: `~/.kaspad/kaspa-mainnet/datadir2`

## Repository Structure

```
kaspa-genesis-proof/
├── verification/
│   ├── genesis_proof.ipynb        # Main verification notebook (supports both node types)
│   ├── store_rust.py              # RocksDB + Bincode support for Rust nodes
│   └── store.py                   # LevelDB + Protobuf support for Go nodes
└── docs/
    └── SETUP_GUIDE.md             # Detailed setup instructions
```

## Verification Process

The verification process follows these cryptographic steps:

1. **Hash Chain Verification:** Verify the hashes of the pruning block chain
2. **Coinbase Transaction Verification:** Reconstruct and verify genesis coinbase transactions
3. **Checkpoint Verification:** Verify checkpoint block hash matches Discord announcement
4. **Bitcoin Timestamp Verification:** Verify Bitcoin block references for timestamp validation
5. **UTXO Set Verification:** Verify UTXO set consistency between checkpoint and genesis
6. **Original Genesis Verification:** Verify original genesis has empty UTXO set

## Security Guarantees

This verification provides cryptographic proof of Kaspa's integrity relying on:
- **Blake2b collision resistance** for chain integrity
- **MuHash collision resistance** for UTXO set consistency  
- **Bitcoin blockchain integrity** for timestamp validation

## Node Compatibility

| Node Type | Database | Serialization | Status |
|-----------|----------|---------------|---------|
| Go-based kaspad | LevelDB | Protobuf | Supported |
| Rust-based rusty-kaspa | RocksDB | Bincode | Supported |

## Documentation

- **[Setup Guide](docs/SETUP_GUIDE.md)** - Detailed installation and usage instructions
- **[Verification Notebook](verification/genesis_proof.ipynb)** - Interactive step-by-step verification

## Features

- **Dual Node Support:** Works with both Go-based kaspad and Rust-based rusty-kaspa nodes
- **Pure Python Implementation:** No external compilers or build tools required
- **User-Friendly:** Clear configuration in notebook cell 2, comprehensive error messages
- **Platform Support:** Works on macOS, Linux, and Windows with platform-specific paths
- **Google Colab Ready:** Complete instructions for cloud-based verification
- **Comprehensive Testing:** Built-in database connectivity tests and troubleshooting guides

## Authors

- **Original Work:** Shai Wyborski, Michael Sutton
- **Rust Node Support:** Luke Dunshea