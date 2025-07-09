# Kaspa Genesis Proof

Tools for cryptographically verifying the integrity of the Kaspa blockchain, supporting both Go-based kaspad and Rust-based rusty-kaspa nodes.

## Overview

This repository provides tools to cryptographically verify that the current UTXO set of Kaspa has naturally evolved from an empty UTXO set, validating that there was no premine.

## Trusted Source Reference

**Based on the original genesis proof by Shai Wyborski and Michael Sutton:**
- **Original Source:** https://github.com/kaspagang/kaspad-py-explorer/blob/main/src/genesis_proof.ipynb
- **Authors:** Shai Wyborski, Michael Sutton

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

### 3. Run Verification

**Option A: Command-line script (recommended for quick verification)**
```bash
# For Rust nodes:
python verify_kaspa_genesis.py --node-type rust --datadir ~/.rusty-kaspa/kaspa-mainnet/datadir

# For Go nodes:
python verify_kaspa_genesis.py --node-type go --datadir ~/.kaspad/kaspa-mainnet/datadir2
```

**Option B: Interactive notebook (recommended for detailed exploration)**
```bash
# Edit genesis_proof.ipynb cell 2 to update your database path
# Then run the verification:
jupyter notebook verification/genesis_proof.ipynb
```

**Common database paths:**
- **Rust nodes**: `~/.rusty-kaspa/kaspa-mainnet/datadir/consensus/consensus-003`
- **Go nodes**: `~/.kaspad/kaspa-mainnet/datadir2`

## Repository Structure

```
kaspa-genesis-proof/
├── verify_kaspa_genesis.py           # Command-line verification script
├── verification/
│   ├── genesis_proof.ipynb           # Interactive verification notebook
│   ├── store_rust.py                 # RocksDB + Bincode support for Rust nodes
│   ├── store_checkpoint.py           # Optimized checkpoint data reader
│   ├── checkpoint_data.json          # Pre-extracted headers (avoids 1GB download)
│   └── store.py                      # LevelDB + Protobuf support for Go nodes
└── docs/
    ├── SETUP_GUIDE.md                # Detailed setup instructions
    └── TECHNICAL_NOTES.md            # Design decisions and implementation details
```

## Verification Process

The verification follows these cryptographic steps:

1. **Database Connectivity:** Verify connection to Kaspa node database
2. **Current Chain State:** Read current DAG tips and headers selected tip
3. **Genesis Header Verification:** Load and verify current genesis block hash
4. **Genesis Coinbase Transaction:** Verify genesis transaction hash matches merkle root
5. **Hash Chain Verification:** Verify pruning chain from current tip to genesis
6. **UTXO Commitment Analysis:** Analyze genesis UTXO commitment (non-empty due to checkpoint)
7. **Pre-Checkpoint Verification:** Verify chain from checkpoint to original genesis with empty UTXO set

## Node Compatibility

| Node Type | Database | Serialization | Status |
|-----------|----------|---------------|---------|
| Go-based kaspad | LevelDB | Protobuf | Supported |
| Rust-based rusty-kaspa | RocksDB | Bincode | Supported |

## Documentation

- **[Setup Guide](docs/SETUP_GUIDE.md)** - Detailed installation and usage instructions
- **[Verification Notebook](verification/genesis_proof.ipynb)** - Interactive step-by-step verification