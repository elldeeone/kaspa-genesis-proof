# Technical Notes: Kaspa Genesis Proof Rust Node Support

This document captures key technical learnings and design decisions made while extending the genesis proof verification to support Rust-based rusty-kaspa nodes.

## Overview

The original genesis proof was designed for Go-based kaspad nodes which use LevelDB and Protocol Buffers. The Rust implementation uses RocksDB and bincode serialization, requiring significant technical adaptations.

## Key Technical Differences

### Database Systems
- **Go nodes**: LevelDB with custom key-value structure
- **Rust nodes**: RocksDB with different key formatting

### Serialization Formats
- **Go nodes**: Protocol Buffers (protobuf)
- **Rust nodes**: Bincode (Rust's binary serialization format)

### Database Access
- **Go nodes**: Database must be closed (node stopped)
- **Rust nodes**: Database can be accessed read-only while node is running

## Design Decision: Pure Python Implementation

### Why We Chose Pure Python
1. **No build dependencies**: Users only need Python, not Rust toolchain
2. **Cross-platform**: Works on any system with Python
3. **Simpler deployment**: No compilation step required
4. **Easier debugging**: All code is interpretable Python

### Alternative Considered: Rust Extractor
We initially built a Rust binary to extract data, but rejected this approach because:
- Required users to install Rust toolchain
- Added compilation step to setup process
- Increased complexity without significant benefits
- Pure Python solution proved sufficient

## The attrs2bin Investigation

### What attrs2bin Is
A Python library that claims to deserialize Rust bincode format data into Python objects.

### Why It Failed for Our Use Case
The fundamental issue was array type mismatch:

1. **Rust Fixed-Size Arrays**: `[u8; 32]` serializes as exactly 32 bytes
2. **Rust Vectors**: `Vec<u8>` serializes as 8-byte length + data
3. **attrs2bin Assumption**: Expected all byte arrays to be length-prefixed like `Vec<u8>`

Example of the mismatch:
```
Rust Hash type: [u8; 32]
Bincode output: [32 bytes of hash data]
attrs2bin expects: [8-byte length][32 bytes of data]
```

When attrs2bin read a Hash field, it interpreted the first 8 bytes of the actual hash as a length value, leading to nonsensical results.

### Our Testing Process
1. Created minimal struct definitions
2. Gradually added fields to isolate the issue
3. Tested various type representations
4. Confirmed the issue was fundamental to how attrs2bin works

## Manual Bincode Implementation

### Why Manual Deserialization
Since no Python library correctly handled Rust's bincode format for our structures, we implemented manual parsing based on the exact Rust struct layout.

### Key Implementation Details

The Rust Header struct (from consensus/core/src/header.rs):
```rust
pub struct Header {
    pub hash: Hash,                     // [u8; 32]
    pub version: u16,                   
    pub parents_by_level: Vec<Vec<Hash>>,
    pub hash_merkle_root: Hash,         // [u8; 32]
    pub accepted_id_merkle_root: Hash,  // [u8; 32]
    pub utxo_commitment: Hash,          // [u8; 32]
    pub timestamp: u64,                 
    pub bits: u32,                      
    pub nonce: u64,                     
    pub daa_score: u64,                 
    pub blue_work: BlueWorkType,        // Uint192 = 24 bytes
    pub blue_score: u64,                
    pub pruning_point: Hash,            // [u8; 32]
}
```

Our parsing approach:
1. Read each field in exact struct order
2. Handle fixed-size arrays without length prefix
3. Handle vectors with 8-byte length prefix
4. Use little-endian for all integers
5. Handle nested structures (Vec<Vec<Hash>>) recursively

## Database Key Format Discovery

### Initial Confusion
The Rust implementation uses different key formatting than we expected. Through debugging, we discovered:

1. **Direct consensus databases** (consensus-003) use simple prefix format
2. **No consensus prefix needed** for direct database access
3. **Key format**: `[prefix_byte] + [hash_bytes]` (no separators)

### Key Prefixes
Found in database/src/registry.rs:
- HEADERS = 8
- HEADERS_SELECTED_TIP = 7  
- TIPS = 24
- PRUNING_POINT = 13

## rocksdict Library

### Why rocksdict
- Modern Python library for RocksDB access
- Supports Python 3.8+ (unlike python-rocksdb which only supports Python 2.7/3.4)
- Provides read-only access mode
- Simple API similar to Python dict

### Key Features Used
1. **Read-only mode**: `rocksdict.AccessType.read_only()`
2. **Direct key access**: `db.get(key)`
3. **Works with running node**: Unlike LevelDB, RocksDB allows concurrent read access

## Lessons Learned

1. **Always verify serialization formats**: Don't assume libraries handle all cases
2. **Read source code**: The Rust source was essential for understanding data structures
3. **Test incrementally**: Building up from minimal cases helped isolate issues
4. **Prefer simple solutions**: Pure Python was simpler and more reliable than external tools
5. **Understand the database structure**: Key format discoveries were crucial

## Future Considerations

1. **Bincode changes**: If Rust changes serialization, manual parsing needs updates
2. **Database format**: Database structure changes would require code updates
3. **Performance**: Pure Python is fast enough for verification, but Rust would be faster for bulk operations
4. **Maintenance**: Manual parsing requires maintenance if struct layouts change

## Testing Approach

Our verification testing covered:
1. **Database connectivity**: Can we open and read the database?
2. **Key format**: Are we building keys correctly?
3. **Deserialization**: Does our manual parsing produce correct data?
4. **Hash verification**: Do calculated hashes match expected values?
5. **Complete workflow**: Does the entire notebook execute successfully?

## Header Hash Verification Discovery

### The Issue
During final testing, we discovered that the standard header hash verification `assert(header_hash(genesis_header) == genesis_hash)` fails for Rust nodes but works for Go nodes.

### Root Cause
The issue was with how BlueWork is serialized differently between Go and Rust:

**Go BlueWork Format:**
- Stored as "trimmed big endian" - leading zeros are removed
- Variable length encoding in hash calculation

**Rust BlueWork Format:**
- Stored as fixed 24-byte little-endian Uint192 in the database
- Must be converted to trimmed big-endian for hash calculation

Additionally, there's a storage format difference:

**Go Storage Format:**
- Headers are stored WITHOUT the hash as part of the data
- Database key: `[prefix][hash]`
- Database value: `[version][parents][merkle_root]...[pruning_point]`
- Hash calculation: `blake2b(version + parents + ... + pruning_point) = expected_hash` ✓

**Rust Storage Format:**
- Headers are stored WITH the hash as the first field
- Database key: `[prefix][hash]`
- Database value: `[hash][version][parents][merkle_root]...[pruning_point]`
- Hash calculation: `blake2b(version + parents + ... + pruning_point) ≠ stored_hash` ✗

The Rust `Header` struct literally includes the hash as its first field:
```rust
pub struct Header {
    pub hash: Hash,  // This is included in storage!
    pub version: u16,
    // ... other fields
}
```

### Why This Happens
- In Go, the hash is computed and used as a key, but not stored in the value
- In Rust, the entire struct (including the hash field) is serialized to storage
- When we deserialize, we get all fields including the stored hash
- But the hash algorithm only hashes the "content" fields, not the hash itself

### Solution Implemented
We fixed the BlueWork deserialization in store_rust.py:

```python
# Convert little-endian storage format to big-endian
blue_work_be = blue_work_bytes[::-1]
# Trim leading zeros (matching Go's behavior)
start = 0
for i, byte in enumerate(blue_work_be):
    if byte != 0:
        start = i
        break
else:
    start = len(blue_work_be)  # All zeros
blue_work = blue_work_be[start:] if start < len(blue_work_be) else b''
```

With this fix, the standard hash verification now works for both Go and Rust nodes:
```python
assert(header_hash(genesis_header) == genesis_hash)  # Works for both!
```

### Why This Solution is Correct

The fix ensures that:
1. BlueWork is handled identically in both implementations for hash calculation
2. The same cryptographic verification works for both node types
3. No special cases or workarounds needed
4. Full algorithmic verification is maintained

### Lessons Learned
1. **Serialization details matter**: BlueWork serialization was the key difference
2. **Big-endian vs little-endian**: Rust stores as little-endian but hashes as big-endian
3. **Trimming is important**: Leading zeros must be removed for hash compatibility
4. **Ask the experts**: Special thanks to Michael Sutton for quickly identifying the BlueWork serialization issue
5. **Test end-to-end early**: We tested components individually but missed the full verification flow

### Acknowledgments
Special shoutout to Michael for providing the crucial insight about BlueWork serialization differences between Go and Rust implementations. His guidance pointing to the specific code in `consensus/core/src/hashing/mod.rs` immediately solved what seemed like an intractable hash mismatch issue.

## Conclusion

The pure Python implementation with manual bincode deserialization provides a robust, maintainable solution that works reliably with Rust nodes while maintaining the simplicity of the original verification process. The header verification difference between Go and Rust nodes is handled transparently, ensuring users can verify the blockchain integrity regardless of which node implementation they use.