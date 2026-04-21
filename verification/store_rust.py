from collections import deque
import struct
import os
import time
from pathlib import Path

# Try to import database packages
try:
    import rocksdict
    ROCKSDB_AVAILABLE = True
except ImportError:
    ROCKSDB_AVAILABLE = False

# Try to import serialization packages
try:
    from kaspa import deserialize_header, deserialize_block
    KASPA_PACKAGE_AVAILABLE = True
except ImportError:
    KASPA_PACKAGE_AVAILABLE = False

# Database prefixes from the Rust implementation
# Based on database/src/registry.rs
# The database registry defines how to format database keys
class DatabaseStorePrefixes:
    ACCEPTANCE_DATA = 1
    BLOCK_TRANSACTIONS = 2
    NON_DAA_MERGESET = 3
    BLOCK_DEPTH = 4
    GHOSTDAG = 5
    GHOSTDAG_COMPACT = 6
    HEADERS_SELECTED_TIP = 7
    HEADERS = 8
    HEADERS_COMPACT = 9
    COMPRESSED_HEADERS = 32
    PAST_PRUNING_POINTS = 10
    PRUNING_UTXOSET = 11
    PRUNING_UTXOSET_POSITION = 12
    PRUNING_POINT = 13
    RETENTION_CHECKPOINT = 14
    REACHABILITY = 15
    REACHABILITY_REINDEX_ROOT = 16
    REACHABILITY_RELATIONS = 17
    RELATIONS_PARENTS = 18
    RELATIONS_CHILDREN = 19
    CHAIN_HASH_BY_INDEX = 20
    CHAIN_INDEX_BY_HASH = 21
    CHAIN_HIGHEST_INDEX = 22
    STATUSES = 23
    TIPS = 24
    UTXO_DIFFS = 25
    UTXO_MULTISETS = 26
    VIRTUAL_UTXOSET = 27
    VIRTUAL_STATE = 28
    PRUNING_SAMPLES = 29
    
    # Metadata
    MULTI_CONSENSUS_METADATA = 124
    CONSENSUS_ENTRIES = 125
    
    # Components
    ADDRESSES = 128
    BANNED_ADDRESSES = 129
    
    # Indexes
    UTXO_INDEX = 192
    UTXO_INDEX_TIPS = 193
    CIRCULATING_SUPPLY = 194

# Database key separator (same as Rust)
SEPARATOR = 255  # u8::MAX
MAX_OPEN_FILES = 128
OPEN_RETRY_ATTEMPTS = 3
OPEN_RETRY_DELAY_SECONDS = 0.25
LEGACY_MULTI_CONSENSUS_METADATA_KEY = b'multi-consensus-metadata-key'
LEGACY_CONSENSUS_ENTRIES_PREFIX = b'consensus-entries-prefix'


class ActiveConsensusResolutionError(RuntimeError):
    """Raised when the meta DB exists but the active consensus directory cannot be resolved."""

def _create_rocksdb_options(max_open_files=MAX_OPEN_FILES):
    """Create RocksDB options for raw-byte access to live Kaspa databases."""
    options = rocksdict.Options(raw_mode=True)
    options.set_max_open_files(max_open_files)
    return options

def _open_rocksdb_readonly(db_path, max_open_files=MAX_OPEN_FILES):
    """Open a RocksDB database in read-only raw mode."""
    last_error = None

    for attempt in range(OPEN_RETRY_ATTEMPTS):
        try:
            return rocksdict.Rdict(
                str(db_path),
                options=_create_rocksdb_options(max_open_files),
                access_type=rocksdict.AccessType.read_only(),
            )
        except Exception as exc:
            last_error = exc
            if attempt == OPEN_RETRY_ATTEMPTS - 1:
                break

            # Live nodes can compact or rotate SST files while we are opening the DB.
            time.sleep(OPEN_RETRY_DELAY_SECONDS * (attempt + 1))

    raise last_error

def _extract_sortable_block_hash(data):
    """Extract the hash field from a bincode-serialized SortableBlock value."""
    if not data:
        return b''
    return data[:32] if len(data) >= 32 else data

def _read_optional_u64(data, pos):
    """Decode a bincode-serialized Option<u64>."""
    if pos >= len(data):
        raise ValueError('metadata ended before Option<u64> tag')

    tag = data[pos]
    pos += 1

    if tag == 0:
        return None, pos
    if tag != 1:
        raise ValueError(f'invalid Option<u64> tag: {tag}')
    if pos + 8 > len(data):
        raise ValueError('metadata ended before Option<u64> value')

    value = struct.unpack('<Q', data[pos:pos+8])[0]
    pos += 8
    return value, pos

def _parse_current_consensus_key(metadata_bytes):
    """Parse just the current_consensus_key from MultiConsensusMetadata."""
    if not metadata_bytes:
        return None

    current_consensus_key, _ = _read_optional_u64(metadata_bytes, 0)
    return current_consensus_key

def _parse_consensus_entry(entry_bytes):
    """Parse a ConsensusEntry value and return its fields."""
    if not entry_bytes or len(entry_bytes) < 24:
        return None

    pos = 0
    key = struct.unpack('<Q', entry_bytes[pos:pos+8])[0]
    pos += 8
    name_len = struct.unpack('<Q', entry_bytes[pos:pos+8])[0]
    pos += 8

    if pos + name_len + 8 > len(entry_bytes):
        return None

    directory_name = entry_bytes[pos:pos+name_len].decode('utf-8')
    pos += name_len
    creation_timestamp = struct.unpack('<Q', entry_bytes[pos:pos+8])[0]

    return {
        'key': key,
        'directory_name': directory_name,
        'creation_timestamp': creation_timestamp,
    }

def find_active_consensus_dir(datadir_root):
    """
    Resolve the active Rust consensus directory from the meta database.
    Returns the full consensus path or None if it cannot be determined.
    Raises ActiveConsensusResolutionError if metadata exists but is unreadable or inconsistent.
    """
    if not ROCKSDB_AVAILABLE:
        return None

    datadir_root = Path(datadir_root).expanduser()
    meta_dir = datadir_root / 'meta'
    if not meta_dir.is_dir():
        return None

    db = None
    try:
        db = _open_rocksdb_readonly(meta_dir, max_open_files=64)
    except Exception as exc:
        raise ActiveConsensusResolutionError(
            f'failed to open meta database at {meta_dir}: {exc}'
        ) from exc

    try:

        metadata_bytes = None
        for metadata_key in (
            bytes([DatabaseStorePrefixes.MULTI_CONSENSUS_METADATA]),
            LEGACY_MULTI_CONSENSUS_METADATA_KEY,
        ):
            metadata_bytes = db.get(metadata_key)
            if metadata_bytes is not None:
                break

        current_consensus_key = _parse_current_consensus_key(metadata_bytes)
        if current_consensus_key is None:
            return None

        current_consensus_key_bytes = current_consensus_key.to_bytes(8, 'little')
        entry_bytes = None
        for entry_key in (
            bytes([DatabaseStorePrefixes.CONSENSUS_ENTRIES]) + current_consensus_key_bytes,
            LEGACY_CONSENSUS_ENTRIES_PREFIX + current_consensus_key_bytes,
        ):
            entry_bytes = db.get(entry_key)
            if entry_bytes is not None:
                break

        if entry_bytes is None:
            raise ActiveConsensusResolutionError(
                f'active consensus entry {current_consensus_key} not found in {meta_dir}'
            )

        entry = _parse_consensus_entry(entry_bytes)
        if not entry:
            raise ActiveConsensusResolutionError(
                f'active consensus entry {current_consensus_key} in {meta_dir} is malformed'
            )

        consensus_dir = datadir_root / 'consensus' / entry['directory_name']
        if consensus_dir.is_dir():
            return str(consensus_dir)

        raise ActiveConsensusResolutionError(
            f'metadata points to missing consensus directory: {consensus_dir}'
        )
    except ActiveConsensusResolutionError:
        raise
    except Exception as exc:
        raise ActiveConsensusResolutionError(
            f'failed to read active consensus metadata from {meta_dir}: {exc}'
        ) from exc
    finally:
        if db is not None:
            db.close()

class Block:
    """
    Class representing block relations
    """
    def __init__(self):
        self.parents = []
        self.children = []

class HeaderData:
    """
    Class representing header data
    """
    def __init__(self, header_dict):
        """
        Initialize from deserialized header dictionary
        """
        self.hashMerkleRoot = header_dict.get('hashMerkleRoot', b'')
        self.acceptedIDMerkleRoot = header_dict.get('acceptedIDMerkleRoot', b'')
        self.utxoCommitment = header_dict.get('utxoCommitment', b'')
        self.pruningPoint = header_dict.get('pruningPoint', b'')
        self.timeInMilliseconds = header_dict.get('timeInMilliseconds', 0)
        self.bits = header_dict.get('bits', 0)
        self.difficulty = HeaderData.bits_to_difficulty(self.bits)
        self.nonce = header_dict.get('nonce', 0)
        self.daaScore = header_dict.get('daaScore', 0)
        self.blueWork = header_dict.get('blueWork', 0)
        self.blueScore = header_dict.get('blueScore', 0)
        self.version = header_dict.get('version', 0)
        self.parents = header_dict.get('parents', [])

    @staticmethod
    def bits_to_difficulty(bits_field):
        if bits_field == 0:
            return 0.0
        target = HeaderData.compact_to_big(bits_field)
        if target == 0:
            return 0.0
        pow_max = 2 ** 255 - 1
        difficulty = pow_max / target
        difficulty = round(difficulty, 2)
        return difficulty

    @staticmethod
    def compact_to_big(compact):
        mantissa = compact & 0x007fffff
        exponent = compact >> 24
        if exponent <= 3:
            destination = mantissa >> 8 * (3 - exponent)
        else:
            destination = mantissa << 8 * (exponent - 3)
        if compact & 0x00800000 != 0:
            return -destination
        else:
            return destination

class BlockData:
    """
    Class representing block data
    """
    def __init__(self, block_dict):
        self.header = HeaderData(block_dict.get('header', {}))
        self.transactions = block_dict.get('transactions', [])
        self.num_txs = len(self.transactions)
        
        # Parse coinbase transaction data if available
        if self.transactions:
            coinbase_tx = self.transactions[0]
            payload = coinbase_tx.get('payload', b'')
            
            # Parse payload structure (similar to original)
            if len(payload) > 20:
                uint64_len = 8
                uint16_len = 2
                subsidy_len = uint64_len
                pubkey_len_len = 1
                pubkey_version_len = uint16_len
                
                self.pubkey_version = payload[uint64_len + subsidy_len]
                pubkey_length = payload[uint64_len + subsidy_len + pubkey_version_len]
                pubkey_start = uint64_len + subsidy_len + pubkey_version_len + pubkey_len_len
                pubkey_end = pubkey_start + pubkey_length
                self.pubkey_script = payload[pubkey_start:pubkey_end]
                
                # Extract version info
                self.kaspad_version = 'unknown'
                self.miner_version = 'unknown'
                
                if len(payload) > pubkey_end:
                    try:
                        extra_data = payload[pubkey_end:].decode("utf-8")
                        if '/' in extra_data:
                            index_of_sep = extra_data.index('/')
                            self.kaspad_version = extra_data[:index_of_sep]
                            self.miner_version = extra_data[index_of_sep+1:]
                        else:
                            self.kaspad_version = extra_data
                    except:
                        pass

class UTXOKey:
    def __init__(self, utxo_dict):
        self.transactionId = utxo_dict.get('transactionId', b'')
        self.index = utxo_dict.get('index', 0)

class UTXOEntry:
    def __init__(self, utxo_dict):
        self.amount = utxo_dict.get('amount', 0)
        self.pubkey_script = utxo_dict.get('scriptPublicKey', b'')
        self.blockDaaScore = utxo_dict.get('blockDaaScore', 0)
        self.isCoinbase = utxo_dict.get('isCoinbase', False)

def _decode_blue_work(blue_work_bytes):
    """Convert little-endian Uint192 bytes to the trimmed big-endian form used by hashing."""
    blue_work_be = blue_work_bytes[::-1]
    start = 0
    for i, byte in enumerate(blue_work_be):
        if byte != 0:
            start = i
            break
    else:
        start = len(blue_work_be)
    return blue_work_be[start:] if start < len(blue_work_be) else b''

def _parse_common_header_fields(data, pos, parents_by_level):
    """Parse the common Header fields after the parent list."""
    hash_merkle_root = data[pos:pos+32]
    pos += 32
    accepted_id_merkle_root = data[pos:pos+32]
    pos += 32
    utxo_commitment = data[pos:pos+32]
    pos += 32
    timestamp = struct.unpack('<Q', data[pos:pos+8])[0]
    pos += 8
    bits = struct.unpack('<I', data[pos:pos+4])[0]
    pos += 4
    nonce = struct.unpack('<Q', data[pos:pos+8])[0]
    pos += 8
    daa_score = struct.unpack('<Q', data[pos:pos+8])[0]
    pos += 8
    blue_work = _decode_blue_work(data[pos:pos+24])
    pos += 24
    blue_score = struct.unpack('<Q', data[pos:pos+8])[0]
    pos += 8
    pruning_point = data[pos:pos+32]
    pos += 32

    header_dict = {
        'hashMerkleRoot': hash_merkle_root,
        'acceptedIDMerkleRoot': accepted_id_merkle_root,
        'utxoCommitment': utxo_commitment,
        'pruningPoint': pruning_point,
        'timeInMilliseconds': timestamp,
        'bits': bits,
        'nonce': nonce,
        'daaScore': daa_score,
        'blueWork': blue_work,
        'blueScore': blue_score,
        'parents': parents_by_level,
    }
    return header_dict, pos

def deserialize_bincode_header_legacy(data):
    """Deserialize the legacy Header format with Vec<Vec<Hash>> parents."""
    pos = 0
    pos += 32  # Skip cached hash
    version = struct.unpack('<H', data[pos:pos+2])[0]
    pos += 2

    parents_outer_len = struct.unpack('<Q', data[pos:pos+8])[0]
    pos += 8
    parents_by_level = []
    for _ in range(parents_outer_len):
        inner_len = struct.unpack('<Q', data[pos:pos+8])[0]
        pos += 8
        inner_hashes = []
        for _ in range(inner_len):
            inner_hashes.append(data[pos:pos+32])
            pos += 32
        parents_by_level.append(inner_hashes)

    header_dict, _ = _parse_common_header_fields(data, pos, parents_by_level)
    header_dict['version'] = version
    return HeaderData(header_dict)

def deserialize_bincode_header_compressed(data):
    """Deserialize the current HeaderWithBlockLevel format with compressed parents."""
    pos = 0
    pos += 32  # Skip cached hash
    version = struct.unpack('<H', data[pos:pos+2])[0]
    pos += 2

    runs_len = struct.unpack('<Q', data[pos:pos+8])[0]
    pos += 8
    compressed_runs = []
    for _ in range(runs_len):
        cumulative_levels = data[pos]
        pos += 1
        inner_len = struct.unpack('<Q', data[pos:pos+8])[0]
        pos += 8
        inner_hashes = []
        for _ in range(inner_len):
            inner_hashes.append(data[pos:pos+32])
            pos += 32
        compressed_runs.append((cumulative_levels, inner_hashes))

    parents_by_level = []
    previous_level = 0
    for cumulative_levels, level_hashes in compressed_runs:
        while previous_level < cumulative_levels:
            parents_by_level.append(level_hashes)
            previous_level += 1

    header_dict, pos = _parse_common_header_fields(data, pos, parents_by_level)
    header_dict['version'] = version

    if pos < len(data):
        # The remaining byte is the serialized block level from HeaderWithBlockLevel.
        pos += 1

    return HeaderData(header_dict)

def deserialize_bincode_header(data, header_format='auto'):
    """
    Deserialize Rust header data from either the current compressed format
    or the legacy uncompressed format.
    """
    if not data or len(data) < 100:
        return None

    parser_sets = {
        'auto': (deserialize_bincode_header_compressed, deserialize_bincode_header_legacy),
        'compressed': (deserialize_bincode_header_compressed,),
        'legacy': (deserialize_bincode_header_legacy,),
    }
    parsers = parser_sets.get(header_format)
    if parsers is None:
        raise ValueError(f'Unknown header format: {header_format}')

    for parser in parsers:
        try:
            return parser(data)
        except Exception:
            continue

    return None

class Store:
    """
    Class managing all accesses to the underlying Kaspa RocksDB
    """
    
    def __init__(self, db_path, print_freq=40000, read_only=True):
        self.db_path = db_path
        self.print_freq = print_freq
        self.read_only = read_only
        
        # Try to initialize RocksDB connection
        if ROCKSDB_AVAILABLE:
            try:
                # Open RocksDB database with rocksdict
                # Unlike Go-based kaspad, Rust-based kaspa allows opening DB while node is running
                if read_only:
                    # Open in read-only mode to avoid conflicts with running node
                    self.db = _open_rocksdb_readonly(db_path)
                else:
                    self.db = rocksdict.Rdict(db_path, options=_create_rocksdb_options())
                self.db_available = True
            except Exception as e:
                print(f"Failed to open RocksDB: {e}")
                self.db_available = False
                self.db = None
        else:
            print("RocksDB not available, using fallback approach")
            self.db_available = False
            self.db = None
        
        # Get active prefix (consensus entry)
        self.prefix = self._get_active_prefix()
        
        # Cache for loaded data
        self.blocks = {}
        self.headers = {}
        self.bodies = {}
        self.print_freq = print_freq
        
        # Pure rocksdict implementation - no Rust extractor needed
        # Using pure rocksdict implementation with manual bincode deserialization
    
    # Removed Rust extractor dependency - using pure rocksdict with manual bincode deserialization
    
    def _get_active_prefix(self):
        """
        Get the active consensus prefix from the database
        For direct consensus databases, we don't need a prefix
        """
        if not self.db_available or self.db is None:
            # Fallback to empty prefix when DB is not available
            return b''
        
        # For direct consensus databases (consensus-003), we use empty prefix
        # This is different from the meta database approach
        # Test if we can find data with empty prefix
        try:
            tips_key = bytes([DatabaseStorePrefixes.TIPS])
            if self.db.get(tips_key) is not None:
                return b''
        except:
            pass
        
        # Fallback to empty prefix
        return b''
    
    def _build_key(self, prefix, *components):
        """
        Build a database key with proper format
        For direct consensus databases, format is: [prefix] + [hash]
        """
        # Direct consensus databases use simple format: prefix + hash
        key = bytes([prefix])
        
        # For headers and other data, direct concatenation
        for component in components:
            key += component
        
        return key
    
    def close(self):
        """Close the database connection"""
        if self.db_available and self.db is not None:
            self.db.close()

    def _iter_prefix_entries(self, prefix):
        """Yield raw RocksDB key/value pairs for a direct-prefix store."""
        iterator = self.db.iter()
        iterator.seek(prefix)

        while iterator.valid():
            key = iterator.key()
            if key is None or not key.startswith(prefix):
                break

            yield key, iterator.value()
            iterator.next()
    
    def get_raw_header(self, block_hash):
        """
        Get raw header data for a block hash
        """
        if not self.db_available or self.db is None:
            print("Database not available - cannot read header data")
            return None

        header_bytes = None
        header_format = None
        for prefix, format_name in (
            (DatabaseStorePrefixes.COMPRESSED_HEADERS, 'compressed'),
            (DatabaseStorePrefixes.HEADERS, 'legacy'),
        ):
            key = self._build_key(prefix, block_hash)
            header_bytes = self.db.get(key)
            if header_bytes is not None:
                header_format = format_name
                break

        if header_bytes is None:
            return None

        # Parse using the format implied by the store prefix to avoid hanging on legacy bytes.
        try:
            header_data = deserialize_bincode_header(header_bytes, header_format=header_format)
            if header_data:
                # Convert HeaderData to RawHeader format for compatibility
                class ParsedHeader:
                    def __init__(self, header_data):
                        self.header_data = header_data
                        self.raw_data = header_bytes
                        # Map to expected interface
                        self.hashMerkleRoot = type('Hash', (), {'hash': header_data.hashMerkleRoot})()
                        self.acceptedIDMerkleRoot = type('Hash', (), {'hash': header_data.acceptedIDMerkleRoot})()
                        self.utxoCommitment = type('Hash', (), {'hash': header_data.utxoCommitment})()
                        self.pruningPoint = type('Hash', (), {'hash': header_data.pruningPoint})()
                        self.timeInMilliseconds = header_data.timeInMilliseconds
                        self.bits = header_data.bits
                        self.nonce = header_data.nonce
                        self.daaScore = header_data.daaScore
                        self.blueWork = header_data.blueWork
                        self.blueScore = header_data.blueScore
                        self.version = header_data.version
                        
                        # Convert parents structure to match expected format
                        # header_data.parents is a list of lists of hashes
                        # We need to wrap them in objects with parentHashes and hash attributes
                        self.parents = []
                        for level_hashes in header_data.parents:
                            level_obj = type('ParentLevel', (), {})()
                            level_obj.parentHashes = []
                            for parent_hash in level_hashes:
                                parent_obj = type('ParentHash', (), {'hash': parent_hash})()
                                level_obj.parentHashes.append(parent_obj)
                            self.parents.append(level_obj)
                
                return ParsedHeader(header_data)
        except Exception as e:
            print(f"Manual bincode deserialization failed: {e}")
        
        # Fallback: return raw bytes with minimal structure
        class RawHeader:
            def __init__(self, data):
                self.raw_data = data
                self.hashMerkleRoot = type('Hash', (), {'hash': b''})()
                self.acceptedIDMerkleRoot = type('Hash', (), {'hash': b''})()
                self.utxoCommitment = type('Hash', (), {'hash': b''})()
                self.pruningPoint = type('Hash', (), {'hash': b''})()
                self.timeInMilliseconds = 0
                self.bits = 0
                self.nonce = 0
                self.daaScore = 0
                self.blueWork = b''
                self.blueScore = 0
                self.version = 0
                self.parents = []
        
        return RawHeader(header_bytes)
    
    def get_raw_block(self, block_hash):
        """
        Get raw block data for a block hash
        """
        key = self._build_key(DatabaseStorePrefixes.BLOCK_TRANSACTIONS, block_hash)
        block_bytes = self.db.get(key)
        
        if block_bytes is None:
            return None
        
        if KASPA_PACKAGE_AVAILABLE:
            try:
                # Use kaspa package for deserialization
                block_data = deserialize_block(block_bytes)
                return block_data
            except:
                pass
        
        # Fallback: create minimal block object
        class MinimalBlock:
            def __init__(self):
                self.header = self.get_raw_header(block_hash)
                self.transactions = []
        
        return MinimalBlock()
    
    def tips(self, include_hst_fallback=True):
        """
        Get current DAG tips and headers selected tip.

        By default, preserve historical behavior and fall back to the headers-selected
        tip when the tips store is unreadable. Callers that need to distinguish real
        DAG tips from this fallback can pass include_hst_fallback=False.
        """
        if not self.db_available or self.db is None:
            print("Database not available - cannot read tips")
            return [], b''
        
        # Get headers selected tip
        hst_key = self._build_key(DatabaseStorePrefixes.HEADERS_SELECTED_TIP)
        hst_bytes = self.db.get(hst_key)
        
        if hst_bytes is None:
            print("Headers selected tip not found")
            hst_hash = b'\x00' * 32
        else:
            # Headers selected tip is stored as a bincode-serialized SortableBlock.
            hst_hash = _extract_sortable_block_hash(hst_bytes)

        # Get tips - in current Rust nodes, tips are stored as individual set entries.
        tips_list = []
        
        try:
            tips_prefix = self._build_key(DatabaseStorePrefixes.TIPS)
            seen = set()

            for key, _ in self._iter_prefix_entries(tips_prefix):
                tip_hash = key[len(tips_prefix):]
                if len(tip_hash) != 32 or tip_hash in seen:
                    continue
                seen.add(tip_hash)
                tips_list.append(tip_hash)

            # Legacy fallback: older layouts stored tips as a serialized Vec<Hash> value.
            if not tips_list:
                tips_bytes = self.db.get(tips_prefix)
                
                if tips_bytes and len(tips_bytes) >= 8:
                    tips_count = struct.unpack('<Q', tips_bytes[:8])[0]
                    pos = 8
                    
                    for _ in range(min(tips_count, 100)):
                        if pos + 32 > len(tips_bytes):
                            break
                        tip_hash = tips_bytes[pos:pos+32]
                        pos += 32
                        if tip_hash in seen:
                            continue
                        seen.add(tip_hash)
                        tips_list.append(tip_hash)

            if not tips_list and include_hst_fallback and len(hst_hash) == 32 and hst_hash != b'\x00' * 32:
                tips_list = [hst_hash]
                        
        except Exception as e:
            print(f"Error reading tips: {e}")
            if include_hst_fallback and len(hst_hash) == 32 and hst_hash != b'\x00' * 32:
                tips_list = [hst_hash]
        
        return tips_list, hst_hash
    
    def pruning_point(self):
        """
        Get current pruning point
        """
        if not self.db_available or self.db is None:
            print("Database not available - cannot read pruning point")
            return b''
        
        pp_key = self._build_key(DatabaseStorePrefixes.PRUNING_POINT)
        pp_bytes = self.db.get(pp_key)
        
        if pp_bytes is None:
            print("Pruning point not found")
            return b''
        
        # Pruning point is stored as raw hash bytes
        return pp_bytes[:32] if len(pp_bytes) >= 32 else pp_bytes
    
    def get_header_data(self, block_hash):
        """
        Get header data with caching - pure rocksdict with bincode deserialization
        """
        if block_hash in self.headers:
            return self.headers[block_hash]
        
        # Get raw header from database and deserialize it
        raw_header = self.get_raw_header(block_hash)
        if raw_header is None:
            return None
        
        # If we have parsed header data, use it
        if hasattr(raw_header, 'header_data'):
            header = raw_header.header_data
            self.headers[block_hash] = header
            return header
        
        # Fallback: create minimal header data
        header_dict = {
            'hashMerkleRoot': getattr(raw_header.hashMerkleRoot, 'hash', b''),
            'acceptedIDMerkleRoot': getattr(raw_header.acceptedIDMerkleRoot, 'hash', b''),
            'utxoCommitment': getattr(raw_header.utxoCommitment, 'hash', b''),
            'pruningPoint': getattr(raw_header.pruningPoint, 'hash', b''),
            'timeInMilliseconds': getattr(raw_header, 'timeInMilliseconds', 0),
            'bits': getattr(raw_header, 'bits', 0),
            'nonce': getattr(raw_header, 'nonce', 0),
            'daaScore': getattr(raw_header, 'daaScore', 0),
            'blueWork': getattr(raw_header, 'blueWork', b''),
            'blueScore': getattr(raw_header, 'blueScore', 0),
            'version': getattr(raw_header, 'version', 0),
            'parents': getattr(raw_header, 'parents', [])
        }
        
        header = HeaderData(header_dict)
        self.headers[block_hash] = header
        return header
    
    def get_block(self, block_hash):
        """
        Get block relations (parents/children)
        """
        if block_hash in self.blocks:
            return self.blocks[block_hash]
        
        # Get parent relations
        parents_key = self._build_key(DatabaseStorePrefixes.RELATIONS_PARENTS, block_hash)
        parents_bytes = self.db.get(parents_key)
        
        # Get children relations
        children_key = self._build_key(DatabaseStorePrefixes.RELATIONS_CHILDREN, block_hash)
        children_bytes = self.db.get(children_key)
        
        block = Block()
        
        # Parse parent and children data
        # This is simplified - actual implementation would deserialize the relations
        if parents_bytes:
            # Placeholder: extract parent hashes
            pass
        
        if children_bytes:
            # Placeholder: extract children hashes
            pass
        
        self.blocks[block_hash] = block
        return block
    
    def load_blocks(self, after_pruning_point=True):
        """
        Load blocks from the database
        """
        self.blocks = {}
        if after_pruning_point:
            self._load_blocks_from_pruning_point_up()
        else:
            self._load_blocks_from_tips_down()
    
    def _load_blocks_from_pruning_point_up(self):
        """
        Load blocks starting from pruning point
        """
        pp = self.pruning_point()
        print('Pruning point: ', pp.hex())
        
        # Implement block loading logic
        # This would traverse the DAG from pruning point upwards
        pass
    
    def _load_blocks_from_tips_down(self):
        """
        Load blocks starting from tips
        """
        tips, hst = self.tips()
        print('Number of DAG tips: ', len(tips))
        print('Headers selected tip: ', hst.hex())
        
        # Implement block loading logic
        # This would traverse the DAG from tips downwards
        pass

# Utility function for migration
def migrate_verification_data(old_store_path, new_store_path):
    """
    Helper function to migrate verification data between old and new store formats
    """
    print(f"Migrating verification data from {old_store_path} to {new_store_path}")
    
    # This would implement migration logic if needed
    # For now, just create the new store
    return Store(new_store_path)

if __name__ == "__main__":
    # Test the store implementation
    import sys
    
    if len(sys.argv) < 2:
        print("Usage: python store_rust.py <datadir_path>")
        sys.exit(1)
    
    datadir = sys.argv[1]
    
    try:
        store = Store(datadir)
        print("Successfully opened RocksDB store")
        
        # Test basic operations
        tips, hst = store.tips()
        print(f"Headers selected tip: {hst.hex()}")
        
        pp = store.pruning_point()
        print(f"Pruning point: {pp.hex()}")
        
        store.close()
        print("Store closed successfully")
        
    except Exception as e:
        print(f"Error: {e}")
        sys.exit(1)
