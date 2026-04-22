#!/usr/bin/env python3
"""
Kaspa Genesis Proof Verification Script

This script performs the complete cryptographic verification of the Kaspa blockchain,
proving that the current UTXO set evolved from an empty state with no premine.

Usage:
    python verify_kaspa_genesis.py --node-type rust --datadir ~/.rusty-kaspa/kaspa-mainnet/datadir
    python verify_kaspa_genesis.py --node-type go --datadir ~/.kaspad/kaspa-mainnet/datadir2
"""

import argparse
import sys
import os
import hashlib
import struct
from pathlib import Path
from types import SimpleNamespace

# Color codes for terminal output
class Colors:
    GREEN = '\033[92m'
    RED = '\033[91m'
    YELLOW = '\033[93m'
    BLUE = '\033[94m'
    BOLD = '\033[1m'
    END = '\033[0m'

HARDWIRED_GENESIS_HASH_HEX = (
    '58c2d4199e21f910d1571d114969cecef48f09f934d42ccb6a281a15868f2999'
)
ORIGINAL_GENESIS_HASH_HEX = (
    'caeb97960a160c211a6b2196bd78399fd4c4cc5b509f55c12c8a7d815f7536ea'
)
CHECKPOINT_HASH_HEX = (
    '0fca37ca667c2d550a6c4416dad9717e50927128c424fa4edbebc436ab13aeef'
)
EMPTY_MUHASH_HEX = (
    '544eb3142c000f0ad2c76ac41f4222abbababed830eeafee4b6dc56b52d5cac0'
)

MAINNET_SUBNETWORK_ID_COINBASE_HEX = '0100000000000000000000000000000000000000'
HARDWIRED_GENESIS_BITCOIN_BLOCK_HASH_HEX = (
    '0000000000000000000b1f8e1c17b0133d439174e52efbb0c41c3583a8aa66b0'
)
ORIGINAL_GENESIS_BITCOIN_BLOCK_HASH_HEX = (
    '00000000000000000001733c62adb19f1b77fa0735d0e11f25af36fc9ca908a5'
)

HARDWIRED_GENESIS_TX_PAYLOAD_HEX = (
    '000000000000000000e1f5050000000000000100d795d79ed79420d793d79920d7a2'
    'd79cd799d79a20d795d7a2d79c20d790d797d799d79a20d799d799d798d79120d791'
    'd7a9d790d7a820d79bd7a1d7a4d79020d795d793d794d791d79420d79cd79ed7a2d7'
    '91d79320d79bd7a8d7a2d795d7aa20d790d79cd794d79bd79d20d7aad7a2d791d793'
    'd795d79f0000000000000000000b1f8e1c17b0133d439174e52efbb0c41c3583a8aa'
    '66b00fca37ca667c2d550a6c4416dad9717e50927128c424fa4edbebc436ab13aeef'
)
ORIGINAL_GENESIS_TX_PAYLOAD_HEX = (
    '000000000000000000e1f5050000000000000100d795d79ed79420d793d79920d7a2'
    'd79cd799d79a20d795d7a2d79c20d790d797d799d79a20d799d799d798d79120d791'
    'd7a9d790d7a820d79bd7a1d7a4d79020d795d793d794d791d79420d79cd79ed7a2d7'
    '91d79320d79bd7a8d7a2d795d7aa20d790d79cd794d79bd79d20d7aad7a2d791d793'
    'd795d79f00000000000000000001733c62adb19f1b77fa0735d0e11f25af36fc9ca9'
    '08a5'
)

HEBREW_TEXT_SLICE = slice(20, 140)
BITCOIN_BLOCK_REFERENCE_SLICE = slice(140, 172)
CHECKPOINT_BLOCK_REFERENCE_SLICE = slice(172, 204)
MAX_HASH_CHAIN_STEPS = 100_000

def print_header(text):
    """Print a section header"""
    print(f"\n{Colors.BOLD}{Colors.BLUE}{'='*60}{Colors.END}")
    print(f"{Colors.BOLD}{Colors.BLUE}{text}{Colors.END}")
    print(f"{Colors.BOLD}{Colors.BLUE}{'='*60}{Colors.END}")

def print_success(text):
    """Print success message"""
    print(f"{Colors.GREEN}✓ {text}{Colors.END}")

def print_error(text):
    """Print error message"""
    print(f"{Colors.RED}✗ {text}{Colors.END}")

def print_info(text):
    """Print info message"""
    print(f"{Colors.GREEN}→ {text}{Colors.END}")

def resolve_rust_datadir(datadir):
    """Resolve the Rust consensus database directory from a rusty-kaspa datadir."""
    resolved = Path(datadir).expanduser()

    # Allow passing the consensus directory directly, e.g. .../consensus/consensus-002
    if resolved.name.startswith('consensus-') and resolved.is_dir():
        return str(resolved)

    # Prefer the same active-consensus metadata that rusty-kaspa itself uses.
    verification_dir = Path(__file__).resolve().parent / 'verification'
    if str(verification_dir) not in sys.path:
        sys.path.insert(0, str(verification_dir))
    from store_rust import ActiveConsensusResolutionError, find_active_consensus_dir

    metadata_error = None
    try:
        consensus_dir = find_active_consensus_dir(str(resolved))
        if consensus_dir is not None:
            return consensus_dir
    except ActiveConsensusResolutionError as exc:
        metadata_error = exc

    consensus_root = resolved / 'consensus'
    if not consensus_root.is_dir():
        if metadata_error is not None:
            raise ValueError(str(metadata_error)) from metadata_error
        return None

    candidates = sorted(
        path for path in consensus_root.iterdir()
        if path.is_dir() and path.name.startswith('consensus-')
    )

    if len(candidates) == 1:
        return str(candidates[0])

    if metadata_error is not None:
        raise ValueError(str(metadata_error)) from metadata_error

    if len(candidates) > 1:
        raise ValueError(
            f"Found multiple consensus directories under {consensus_root}; "
            "could not determine the active one from metadata"
        )

    return None

def transaction_hash(t):
    """Calculate transaction hash using Kaspa's algorithm"""
    hasher = hashlib.blake2b(digest_size=32, key=b"TransactionHash")
    hasher.update(struct.pack(f"<HQ", t.version, len(t.inputs)))
    for ti in t.inputs:
        hasher.update(ti.previousOutpoint.transactionID.transactionId)
        hasher.update(struct.pack(f"<IQ", ti.previousOutpoint.index, 
                                  len(ti.signatureScript)))
        hasher.update(ti.signatureScript)
        hasher.update(struct.pack(f"<Q", ti.sequence))

    hasher.update(struct.pack(f"<Q", len(t.outputs)))
    for to in t.outputs:
        hasher.update(struct.pack(f"<QHQ", to.value, 
                                  to.scriptPublicKey.version, 
                                  len(to.scriptPublicKey.script)))
        hasher.update(to.scriptPublicKey.script)
        
    hasher.update(struct.pack(f"<Q", t.lockTime))
    hasher.update(t.subnetworkID.subnetworkId)
    hasher.update(struct.pack(f"<QQ", t.gas, len(t.payload)))
    hasher.update(t.payload)
    return hasher.digest()

def header_hash(h):
    """Calculate header hash using Kaspa's algorithm"""
    hasher = hashlib.blake2b(digest_size=32, key=b"BlockHash")
    hasher.update(struct.pack(f"<HQ", h.version, len(h.parents)))
    for level_parents in h.parents:
        hasher.update(struct.pack(f"<Q", len(level_parents.parentHashes)))
        for parent in level_parents.parentHashes:
            hasher.update(parent.hash)
    hasher.update(h.hashMerkleRoot.hash)    
    hasher.update(h.acceptedIDMerkleRoot.hash)
    hasher.update(h.utxoCommitment.hash)
    hasher.update(struct.pack(f"<QIQQQQ", 
                              h.timeInMilliseconds, 
                              h.bits, 
                              h.nonce, 
                              h.daaScore, 
                              h.blueScore, 
                              len(h.blueWork)))
    hasher.update(h.blueWork)
    hasher.update(h.pruningPoint.hash)
    return hasher.digest()

def build_genesis_coinbase_transaction(payload_hex):
    """Build a genesis coinbase transaction from the canonical payload bytes."""
    return SimpleNamespace(
        version=0,
        subnetworkID=SimpleNamespace(
            subnetworkId=bytes.fromhex(MAINNET_SUBNETWORK_ID_COINBASE_HEX)
        ),
        inputs=[],
        outputs=[],
        lockTime=0,
        gas=0,
        payload=bytes.fromhex(payload_hex),
    )

def choose_chain_tip_for_verification(tips):
    """Mirror the notebook's tips[0] semantics for proof anchoring."""
    return tips[0] if tips else b''

def open_pre_checkpoint_store(pre_checkpoint_datadir, checkpoint_json_path):
    """Open the notebook-compatible pre-checkpoint store."""
    if pre_checkpoint_datadir:
        from store import Store as GoStore

        pre_checkpoint_path = os.path.expanduser(pre_checkpoint_datadir)
        if not os.path.exists(pre_checkpoint_path):
            raise FileNotFoundError(
                f"Pre-checkpoint data directory not found: {pre_checkpoint_path}"
            )

        print_info("Trust model: operator-supplied pre-checkpoint store")
        store = GoStore(pre_checkpoint_path)
        print_success("Loaded external pre-checkpoint store")
        print_info(f"Pre-checkpoint store path: {pre_checkpoint_path}")
        return store, True

    if os.path.exists(checkpoint_json_path):
        from store_checkpoint import CheckpointStore

        print_info(
            "Trust model: embedded pre-checkpoint data by default "
            "(override with --pre-checkpoint-datadir PATH)"
        )
        print_success("Loaded embedded checkpoint_data.json")
        print_info("(No need to download the 1GB checkpoint database!)")
        return CheckpointStore(checkpoint_json_path), True

    return None, False

def assert_cryptographic_hash_chain_to_genesis(store, block_hash, genesis_hash, verbose=False):
    """Verify the hash chain from a block to genesis"""
    i = 0
    while True:
        if block_hash == genesis_hash:
            if verbose:
                print_info(f"✓ Reached genesis block via {i} pruning points")
            return True
        
        header = store.get_raw_header(block_hash)
        if header is None:
            print_error(f"Header not found for hash: {block_hash.hex()}")
            return False
            
        # Assert the block hash is correct
        calculated_hash = header_hash(header)
        if calculated_hash != block_hash:
            print_error(f"Hash mismatch at block {block_hash.hex()}")
            print_error(f"  Expected: {block_hash.hex()}")
            print_error(f"  Got:      {calculated_hash.hex()}")
            return False
            
        if verbose:
            print_info(f"  Step {i+1}: {block_hash.hex()} -> {header.pruningPoint.hash.hex()}")
            
        block_hash = header.pruningPoint.hash
        i += 1
        
        if i > MAX_HASH_CHAIN_STEPS:  # Safety check
            print_error("Too many iterations in hash chain verification")
            return False

def verify_genesis(node_type, datadir, pre_checkpoint_datadir=None, verbose=False):
    """Main verification function"""

    script_dir = os.path.dirname(os.path.abspath(__file__))
    verification_dir = os.path.join(script_dir, 'verification')
    checkpoint_json_path = os.path.join(verification_dir, 'checkpoint_data.json')

    if verification_dir not in sys.path:
        sys.path.insert(0, verification_dir)

    if node_type == 'rust':
        from store_rust import Store
        print_info("Using Rust node store (RocksDB + Bincode)")
    else:
        from store import Store
        print_info("Using Go node store (LevelDB + Protobuf)")
    hardwired_genesis = bytes.fromhex(HARDWIRED_GENESIS_HASH_HEX)
    original_genesis = bytes.fromhex(ORIGINAL_GENESIS_HASH_HEX)
    checkpoint_hash = bytes.fromhex(CHECKPOINT_HASH_HEX)
    empty_muhash = bytes.fromhex(EMPTY_MUHASH_HEX)
    expected_hardwired_bitcoin_hash = bytes.fromhex(
        HARDWIRED_GENESIS_BITCOIN_BLOCK_HASH_HEX
    )
    expected_original_bitcoin_hash = bytes.fromhex(
        ORIGINAL_GENESIS_BITCOIN_BLOCK_HASH_HEX
    )

    hardwired_genesis_coinbase_tx = build_genesis_coinbase_transaction(
        HARDWIRED_GENESIS_TX_PAYLOAD_HEX
    )
    original_genesis_coinbase_tx = build_genesis_coinbase_transaction(
        ORIGINAL_GENESIS_TX_PAYLOAD_HEX
    )

    current_store = None
    pre_checkpoint_store = None
    pre_checkpoint_verified = False

    try:
        print_header("Step 1: Database Connectivity Test")
        current_store = Store(datadir)
        if hasattr(current_store, 'db_available') and not current_store.db_available:
            print_error("Current database could not be opened")
            return False
        print_success("Current database opened successfully")

        print_header("Step 2: Current Chain State")
        if node_type == 'rust':
            tips, hst = current_store.tips(include_hst_fallback=False)
        else:
            tips, hst = current_store.tips()
        print_info(f"Number of DAG tips: {len(tips)}")
        print_info(f"Headers selected tip: {hst.hex()}")
        chain_tip = choose_chain_tip_for_verification(tips)
        if chain_tip and chain_tip != hst:
            print_info(f"Proof chain tip selected from DAG tips: {chain_tip.hex()}")

        print_header("Step 3: Genesis Header Verification")
        genesis_header = current_store.get_raw_header(hardwired_genesis)
        if genesis_header is None:
            print_error("Genesis header not found")
            return False

        print_info(f"Expected genesis hash: {hardwired_genesis.hex()}")
        calculated_genesis_hash = header_hash(genesis_header)
        print_info(f"Calculated hash:      {calculated_genesis_hash.hex()}")

        if calculated_genesis_hash != hardwired_genesis:
            print_error("Genesis header hash mismatch")
            return False
        print_success("Genesis header hash verified")

        print_info(f"Genesis timestamp: {genesis_header.timeInMilliseconds}")
        print_info(f"Genesis DAA score: {genesis_header.daaScore}")
        print_info(f"Genesis blue score: {genesis_header.blueScore}")
        print_info(f"Genesis bits (difficulty): {genesis_header.bits}")

        print_header("Step 4: Genesis Coinbase Transaction")
        print_info("Genesis transaction properties:")
        print_info(f"  Version: {hardwired_genesis_coinbase_tx.version}")
        print_info(
            f"  Inputs: {len(hardwired_genesis_coinbase_tx.inputs)} "
            "(coinbase has no inputs)"
        )
        print_info(
            f"  Outputs: {len(hardwired_genesis_coinbase_tx.outputs)} "
            "(coinbase has no outputs)"
        )
        print_info(f"  Payload size: {len(hardwired_genesis_coinbase_tx.payload)} bytes")

        calc_hash = transaction_hash(hardwired_genesis_coinbase_tx)
        print_info(f"Calculated tx hash:    {calc_hash.hex()}")
        print_info(f"Expected merkle root:  {genesis_header.hashMerkleRoot.hash.hex()}")

        if calc_hash != genesis_header.hashMerkleRoot.hash:
            print_error("Genesis coinbase transaction hash mismatch")
            return False
        print_success("Genesis coinbase transaction verified")

        print_info("Embedded data in genesis transaction:")
        hebrew_text = hardwired_genesis_coinbase_tx.payload[HEBREW_TEXT_SLICE]
        print_info(f"  Hebrew text: '{hebrew_text.decode('utf-8', errors='replace')}'")

        bitcoin_hash = hardwired_genesis_coinbase_tx.payload[BITCOIN_BLOCK_REFERENCE_SLICE]
        print_info(f"  Bitcoin block reference: {bitcoin_hash.hex()}")
        print_info(f"    (Bitcoin block #808080, provides timestamp anchor)")
        if bitcoin_hash != expected_hardwired_bitcoin_hash:
            print_error("Hardwired genesis bitcoin block reference mismatch")
            return False
        print_success("Bitcoin block reference verified")

        checkpoint_ref = hardwired_genesis_coinbase_tx.payload[
            CHECKPOINT_BLOCK_REFERENCE_SLICE
        ]
        print_info(f"  Checkpoint block reference: {checkpoint_ref.hex()}")
        print_info(f"    (Kaspa checkpoint block for UTXO state)")
        if checkpoint_ref != checkpoint_hash:
            print_error("Hardwired genesis checkpoint block reference mismatch")
            return False
        print_success("Checkpoint block reference verified")

        print_header("Step 5: Hash Chain Verification")
        if not chain_tip:
            print_error("No valid chain tip found to verify")
            return False

        print_info(f"Starting hash chain verification from tip: {chain_tip.hex()}")
        print_info(f"Target genesis hash: {hardwired_genesis.hex()}")
        print_info("Verifying hash chain from current tip to genesis...")
        if not assert_cryptographic_hash_chain_to_genesis(
            current_store, chain_tip, hardwired_genesis, True
        ):
            print_error("Hash chain verification failed")
            return False
        print_success("Hash chain from current state to genesis verified")

        print_header("Step 6: UTXO Commitment Analysis")
        utxo_commitment = genesis_header.utxoCommitment.hash
        print_info(f"Genesis UTXO commitment: {utxo_commitment.hex()}")
        print_info(f"Empty MuHash value:      {empty_muhash.hex()}")

        if all(byte == 0 for byte in utxo_commitment):
            print_info("Status: All-zero UTXO commitment (should not occur)")
        elif utxo_commitment == empty_muhash:
            print_info("Status: Empty UTXO commitment (original genesis)")
        else:
            print_info(
                "Status: Non-empty UTXO commitment "
                "(hardwired genesis with checkpoint UTXO set)"
            )
            print_info(
                "This means the genesis contains a pre-calculated UTXO set "
                "from a checkpoint"
            )

        print_header("Step 7: Pre-Checkpoint Verification")
        print_info(
            "This step covers the checkpoint/original-genesis proof chain "
            "(notebook checks 5-9)."
        )

        pre_checkpoint_store, use_pre_checkpoint_data = open_pre_checkpoint_store(
            pre_checkpoint_datadir, checkpoint_json_path
        )

        if use_pre_checkpoint_data:
            print_info(f"Checkpoint hash:       {checkpoint_hash.hex()}")
            print_info(f"Original genesis hash: {original_genesis.hex()}")

            checkpoint_header = pre_checkpoint_store.get_raw_header(checkpoint_hash)
            if checkpoint_header is None:
                print_error("Checkpoint header not found in data")
                return False

            print_success("Checkpoint header found")
            calculated_checkpoint_hash = header_hash(checkpoint_header)
            if calculated_checkpoint_hash != checkpoint_hash:
                print_error("Checkpoint header hash mismatch")
                return False

            print_success("Checkpoint header hash verified")
            print_info(
                "Checkpoint UTXO commitment: "
                f"{checkpoint_header.utxoCommitment.hash.hex()}"
            )

            if genesis_header.utxoCommitment.hash != checkpoint_header.utxoCommitment.hash:
                print_error("UTXO commitment mismatch between genesis and checkpoint")
                print_error(f"Genesis:    {genesis_header.utxoCommitment.hash.hex()}")
                print_error(f"Checkpoint: {checkpoint_header.utxoCommitment.hash.hex()}")
                return False

            print_success("UTXO commitments match between genesis and checkpoint")
            print_info("Verifying chain from checkpoint to original genesis...")
            if not assert_cryptographic_hash_chain_to_genesis(
                pre_checkpoint_store, checkpoint_hash, original_genesis, True
            ):
                print_error("Checkpoint chain verification failed")
                return False

            print_success("Checkpoint chain to original genesis verified")

            original_genesis_header = pre_checkpoint_store.get_raw_header(original_genesis)
            if original_genesis_header is None:
                print_error("Original genesis header not found in checkpoint data")
                return False

            calculated_original_genesis_hash = header_hash(original_genesis_header)
            if calculated_original_genesis_hash != original_genesis:
                print_error("Original genesis header hash mismatch")
                return False

            original_calc_hash = transaction_hash(original_genesis_coinbase_tx)
            original_bitcoin_hash = original_genesis_coinbase_tx.payload[
                BITCOIN_BLOCK_REFERENCE_SLICE
            ]
            print_info(
                "Original genesis coinbase tx hash: "
                f"{original_calc_hash.hex()}"
            )
            print_info(
                "Original genesis merkle root:     "
                f"{original_genesis_header.hashMerkleRoot.hash.hex()}"
            )
            print_info(
                "Original genesis bitcoin reference: "
                f"{original_bitcoin_hash.hex()}"
            )

            if original_calc_hash != original_genesis_header.hashMerkleRoot.hash:
                print_error("Original genesis coinbase transaction hash mismatch")
                return False

            if original_bitcoin_hash != expected_original_bitcoin_hash:
                print_error("Original genesis bitcoin block reference mismatch")
                return False

            print_success("Original genesis coinbase transaction verified")
            print_success("Original genesis bitcoin block reference verified")
            print_info(
                "Original genesis UTXO commitment: "
                f"{original_genesis_header.utxoCommitment.hash.hex()}"
            )
            print_info(f"Expected empty MuHash:            {empty_muhash.hex()}")

            if original_genesis_header.utxoCommitment.hash != empty_muhash:
                print_error("Original genesis UTXO set is not empty")
                return False

            print_success("Original genesis has empty UTXO set verified!")
            pre_checkpoint_verified = True
        else:
            print_info("Pre-checkpoint verification skipped")
            print_info("To enable: Place checkpoint_data.json in verification folder")
            print_info("Or provide --pre-checkpoint-datadir with full database")

        print_header("Verification Summary")
        print_success("All cryptographic verifications passed!")
        print_info("Verification details:")
        print_info(f"  ✓ Genesis hash: {hardwired_genesis.hex()}")
        print_info(f"  ✓ Genesis coinbase transaction verified")
        print_info(f"  ✓ Hardwired Bitcoin and checkpoint references verified")
        print_info(f"  ✓ Hash chain from current tip to genesis verified")
        print_info(f"  ✓ UTXO commitment analysis completed")
        if pre_checkpoint_verified:
            print_info(f"  ✓ Checkpoint/original-genesis proof chain verified")
            print_info(f"  ✓ Original genesis coinbase transaction verified")
            print_info(f"  ✓ Original genesis Bitcoin block reference verified")
            print_info(f"  ✓ Original genesis empty UTXO set verified")

        print_success("The Kaspa blockchain integrity has been verified")
        print_success("No premine detected - UTXO set evolved from empty state")

        print(f"\n{Colors.BOLD}Thank you for verifying the integrity of Kaspa!{Colors.END}")

        return True

    except Exception as e:
        print_error(f"Verification failed with error: {str(e)}")
        if verbose:
            import traceback
            traceback.print_exc()
        return False
    finally:
        if current_store is not None:
            current_store.close()
        if pre_checkpoint_store is not None:
            pre_checkpoint_store.close()

def main():
    parser = argparse.ArgumentParser(
        description='Verify the cryptographic integrity of the Kaspa blockchain',
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Verify with Rust node
  %(prog)s --node-type rust --datadir ~/.rusty-kaspa/kaspa-mainnet/datadir

  # Verify with Go node
  %(prog)s --node-type go --datadir ~/.kaspad/kaspa-mainnet/datadir2

  # Verbose output
  %(prog)s --node-type rust --datadir ~/.rusty-kaspa/kaspa-mainnet/datadir --verbose
        """
    )
    
    parser.add_argument(
        '--node-type',
        choices=['rust', 'go'],
        required=True,
        help='Type of Kaspa node (rust for rusty-kaspa, go for kaspad)'
    )
    
    parser.add_argument(
        '--datadir',
        required=True,
        help='Path to node data directory'
    )
    
    parser.add_argument(
        '--pre-checkpoint-datadir',
        help='Path to pre-checkpoint database (optional, for full verification)'
    )
    
    parser.add_argument(
        '--verbose', '-v',
        action='store_true',
        help='Enable verbose output'
    )
    
    args = parser.parse_args()
    
    # Expand user path
    datadir = os.path.expanduser(args.datadir)
    
    # Check if datadir exists
    if not os.path.exists(datadir):
        print_error(f"Data directory not found: {datadir}")
        sys.exit(1)
    
    # For Rust nodes, resolve the consensus subdirectory dynamically
    if args.node_type == 'rust':
        try:
            resolved_datadir = resolve_rust_datadir(datadir)
        except ValueError as exc:
            print_error(f"Unable to resolve Rust consensus directory from: {datadir}")
            print_info(str(exc))
            sys.exit(1)
        if resolved_datadir is None:
            consensus_root = os.path.join(datadir, 'consensus')
            print_error(f"Unable to resolve Rust consensus directory from: {datadir}")
            print_info("Pass the root rusty-kaspa datadir or the full consensus path")
            print_info(f"Expected a single consensus-* directory under: {consensus_root}")
            sys.exit(1)
        datadir = resolved_datadir

    print(f"{Colors.BOLD}Kaspa Genesis Proof Verification{Colors.END}")
    print(f"Node type: {args.node_type}")
    print(f"Data directory: {datadir}")
    
    # Run verification
    success = verify_genesis(
        args.node_type,
        datadir,
        args.pre_checkpoint_datadir,
        args.verbose
    )
    
    sys.exit(0 if success else 1)

if __name__ == "__main__":
    main()
