# Setup Guide

## Prerequisites

### For Rust-based rusty-kaspa nodes (Recommended):
- **Fully synced rusty-kaspa node**
- **Python 3.8+**
- **Node data location**: `~/.rusty-kaspa/kaspa-mainnet/datadir/consensus/consensus-003`

### For Go-based kaspad nodes (Legacy):
- **Fully synced kaspad node**
- **Python 3.8+**
- **Node data location**: `~/.kaspad/kaspa-mainnet/datadir2`

## Quick Start

### 1. Clone and Setup
```bash
git clone https://github.com/elldeeone/kaspa-genesis-proof
cd kaspa-genesis-proof
```

### 2. Install Dependencies

**Recommended: Use a virtual environment to avoid conflicts:**
```bash
python3 -m venv kaspa-genesis-env
source kaspa-genesis-env/bin/activate  # On Windows: kaspa-genesis-env\Scripts\activate
```

**For Rust nodes:**
```bash
pip install numpy pandas rocksdict tqdm notebook
# Or install from requirements.txt:
pip install -r requirements.txt
```

**For Go nodes:**
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
# Edit verification/genesis_proof.ipynb cell 14 to update your database path
# Then run:
jupyter notebook verification/genesis_proof.ipynb
```

**Common paths by platform:**

| Platform | Rust Node Path | Go Node Path |
|----------|---------------|--------------|
| **macOS** | `~/.rusty-kaspa/kaspa-mainnet/datadir/consensus/consensus-003` | `~/.kaspad/kaspa-mainnet/datadir2` |
| **Linux** | `~/.rusty-kaspa/kaspa-mainnet/datadir/consensus/consensus-003` | `~/.kaspad/kaspa-mainnet/datadir2` |
| **Windows** | `%APPDATA%\.rusty-kaspa\kaspa-mainnet\datadir\consensus\consensus-003` | `%APPDATA%\.kaspad\kaspa-mainnet\datadir2` |

**Note:** The verification script will automatically detect the consensus database directory within your datadir.

## Troubleshooting

### Database Path Issues
- **Error**: `Database connectivity test failed`
- **Solution**: Verify your database path exists and contains data
- **Check**: Run `ls -la ~/.rusty-kaspa/kaspa-mainnet/datadir/consensus/consensus-003` (or equivalent path)

### Python RocksDB Issues
- **Error**: `rocksdict not available`
- **Solution**: Install rocksdict: `pip install rocksdict`
- **Check**: Ensure you're using Python 3.8+ as rocksdict requires modern Python

### Node Not Synced
- **Error**: `Genesis header not found`
- **Solution**: Wait for your node to fully sync (may take hours/days)
- **Check**: Verify your node is running and fully synced

## Google Colab Usage

For Google Colab users, use the commented section in **cell 2** of the notebook:

1. **Upload your datadir**: Zip your node's datadir and upload to Colab
2. **Uncomment Colab section**: In cell 2, uncomment the Google Colab installation commands
3. **Run verification**: Execute all cells normally

**Note:** The pre-extracted `checkpoint_data.json` file (128KB) eliminates the need to download the 1GB checkpoint database in most cases.

## Support

If you encounter issues:

1. **Check Prerequisites**: Ensure your node is fully synced
2. **Verify Paths**: Confirm database paths are correct for your system
3. **Review Error Messages**: Most issues are path-related
4. **GitHub Issues**: Report problems at https://github.com/elldeeone/kaspa-genesis-proof/issues

## Trust and Verification

This verification toolkit is based on the original authoritative work by **Shai Wyborski** and **Michael Sutton**. The cryptographic verification logic is identical to the original proof, extended only to support Rust-based rusty-kaspa nodes.

**Original Source**: https://github.com/kaspagang/kaspad-py-explorer/blob/main/src/genesis_proof.ipynb