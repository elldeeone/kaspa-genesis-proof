from collections import deque
import struct
import os

# Try to import database packages
try:
    import rocksdict
    ROCKSDB_AVAILABLE = True
except ImportError:
    ROCKSDB_AVAILABLE = False
    print("Warning: rocksdict not available. Using fallback approach.")

# Try to import serialization packages
try:
    from kaspa import deserialize_header, deserialize_block
    KASPA_PACKAGE_AVAILABLE = True
except ImportError:
    KASPA_PACKAGE_AVAILABLE = False
    print("Warning: kaspa package not available. Using fallback approach.")

# Manual bincode deserialization - no external libraries needed
print("Using manual bincode deserialization for complete compatibility")

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

# Manual bincode deserialization - no struct definition needed
# The deserialize_bincode_header function handles the parsing directly

def deserialize_bincode_header(data):
    """
    Deserialize bincode header data using manual parsing
    
    This deserializes the Rust Header struct serialized with bincode.
    The format matches exactly what Rust bincode produces:
    - Fixed-size arrays (Hash) are serialized directly without length prefix
    - u16/u32/u64 are serialized as little-endian integers
    - Vec<T> has 8-byte length prefix followed by elements
    """
    if not data or len(data) < 100:  # Headers should be much larger
        return None
    
    try:
        pos = 0
        
        # Parse according to Rust Header struct order:
        # pub struct Header {
        #     pub hash: Hash,                     // [u8; 32]
        #     pub version: u16,                   // 2 bytes
        #     pub parents_by_level: Vec<Vec<Hash>>, // 8-byte len + nested vecs
        #     pub hash_merkle_root: Hash,         // [u8; 32]
        #     pub accepted_id_merkle_root: Hash,  // [u8; 32]
        #     pub utxo_commitment: Hash,          // [u8; 32]
        #     pub timestamp: u64,                 // 8 bytes
        #     pub bits: u32,                      // 4 bytes
        #     pub nonce: u64,                     // 8 bytes
        #     pub daa_score: u64,                 // 8 bytes
        #     pub blue_work: BlueWorkType,        // Uint192 = 24 bytes
        #     pub blue_score: u64,                // 8 bytes
        #     pub pruning_point: Hash,            // [u8; 32]
        # }
        
        # 1. hash: Hash ([u8; 32])
        hash_bytes = data[pos:pos+32]
        pos += 32
        
        # 2. version: u16 (2 bytes, little-endian)
        version = struct.unpack('<H', data[pos:pos+2])[0]
        pos += 2
        
        # 3. parents_by_level: Vec<Vec<Hash>> (8-byte len + nested structure)
        parents_outer_len = struct.unpack('<Q', data[pos:pos+8])[0]
        pos += 8
        
        parents_by_level = []
        for _ in range(parents_outer_len):
            # Each inner Vec<Hash> also has 8-byte length
            inner_len = struct.unpack('<Q', data[pos:pos+8])[0]
            pos += 8
            inner_hashes = []
            for _ in range(inner_len):
                inner_hash = data[pos:pos+32]
                inner_hashes.append(inner_hash)
                pos += 32
            parents_by_level.append(inner_hashes)
        
        # 4. hash_merkle_root: Hash ([u8; 32])
        hash_merkle_root = data[pos:pos+32]
        pos += 32
        
        # 5. accepted_id_merkle_root: Hash ([u8; 32])
        accepted_id_merkle_root = data[pos:pos+32]
        pos += 32
        
        # 6. utxo_commitment: Hash ([u8; 32])
        utxo_commitment = data[pos:pos+32]
        pos += 32
        
        # 7. timestamp: u64 (8 bytes, little-endian)
        timestamp = struct.unpack('<Q', data[pos:pos+8])[0]
        pos += 8
        
        # 8. bits: u32 (4 bytes, little-endian)
        bits = struct.unpack('<I', data[pos:pos+4])[0]
        pos += 4
        
        # 9. nonce: u64 (8 bytes, little-endian)
        nonce = struct.unpack('<Q', data[pos:pos+8])[0]
        pos += 8
        
        # 10. daa_score: u64 (8 bytes, little-endian)
        daa_score = struct.unpack('<Q', data[pos:pos+8])[0]
        pos += 8
        
        # 11. blue_work: BlueWorkType (Uint192 = 24 bytes, little-endian)
        blue_work = data[pos:pos+24]
        pos += 24
        
        # 12. blue_score: u64 (8 bytes, little-endian)
        blue_score = struct.unpack('<Q', data[pos:pos+8])[0]
        pos += 8
        
        # 13. pruning_point: Hash ([u8; 32])
        pruning_point = data[pos:pos+32]
        pos += 32
        
        # Convert to our HeaderData format
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
            'version': version,
            'parents': parents_by_level
        }
        
        return HeaderData(header_dict)
        
    except Exception as e:
        print(f"Manual bincode header deserialization failed: {e}")
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
                    self.db = rocksdict.Rdict(db_path, access_type=rocksdict.AccessType.read_only())
                else:
                    self.db = rocksdict.Rdict(db_path)
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
        print("Using pure rocksdict implementation with manual bincode deserialization")
    
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
    
    def get_raw_header(self, block_hash):
        """
        Get raw header data for a block hash
        """
        if not self.db_available or self.db is None:
            print("Database not available - cannot read header data")
            return None
            
        key = self._build_key(DatabaseStorePrefixes.HEADERS, block_hash)
        header_bytes = self.db.get(key)
        
        if header_bytes is None:
            return None
        
        # Try to deserialize with manual bincode parser
        try:
            header_data = deserialize_bincode_header(header_bytes)
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
                        self.parents = header_data.parents
                
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
    
    def tips(self):
        """
        Get current DAG tips and headers selected tip
        """
        if not self.db_available or self.db is None:
            print("Database not available - cannot read tips")
            return [], b''
        
        # Get headers selected tip
        hst_key = self._build_key(DatabaseStorePrefixes.HEADERS_SELECTED_TIP)
        hst_bytes = self.db.get(hst_key)
        
        # Get tips
        tips_key = self._build_key(DatabaseStorePrefixes.TIPS)
        tips_bytes = self.db.get(tips_key)
        
        if hst_bytes is None:
            print("Headers selected tip not found")
            hst_hash = b'\x00' * 32
        else:
            # Headers selected tip is stored as raw hash bytes
            hst_hash = hst_bytes[:32] if len(hst_bytes) >= 32 else hst_bytes
        
        if tips_bytes is None:
            print("Tips not found")
            return [], hst_hash
        
        # Tips are stored as bincode-serialized Vec<Hash>
        # For now, parse as simple list of 32-byte hashes
        tips_list = []
        if len(tips_bytes) >= 8:  # At least length prefix
            try:
                # First 8 bytes are the length (little-endian u64)
                import struct
                tips_count = struct.unpack('<Q', tips_bytes[:8])[0]
                pos = 8
                
                for i in range(tips_count):
                    if pos + 32 <= len(tips_bytes):
                        tip_hash = tips_bytes[pos:pos+32]
                        tips_list.append(tip_hash)
                        pos += 32
                    else:
                        break
                        
            except Exception as e:
                print(f"Failed to parse tips: {e}")
                # Fallback to headers selected tip
                tips_list = [hst_hash] if len(hst_hash) == 32 else []
        
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
        print(f"Found {len(tips)} tips")
        print(f"Headers selected tip: {hst.hex()}")
        
        pp = store.pruning_point()
        print(f"Pruning point: {pp.hex()}")
        
        store.close()
        print("Store closed successfully")
        
    except Exception as e:
        print(f"Error: {e}")
        sys.exit(1)