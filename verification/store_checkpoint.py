"""
Checkpoint store implementation that reads from extracted JSON data
instead of requiring the full pre-checkpoint database.
"""
import json
import os

class CheckpointStore:
    """Store implementation that reads from extracted checkpoint data"""
    
    def __init__(self, json_path='verification/checkpoint_data.json'):
        """Initialize from JSON file instead of database"""
        with open(json_path, 'r') as f:
            self.data = json.load(f)
        
        # Build header lookup
        self.headers = {}
        for header_data in self.data['headers_chain']:
            # Convert hex strings back to bytes where needed
            header = type('Header', (), {})()
            header.version = header_data['version']
            
            # Reconstruct parents structure
            header.parents = []
            for level_hashes in header_data['parents']:
                level = type('ParentLevel', (), {})()
                level.parentHashes = []
                for parent_hex in level_hashes:
                    parent = type('Parent', (), {'hash': bytes.fromhex(parent_hex)})()
                    level.parentHashes.append(parent)
                header.parents.append(level)
            
            # Reconstruct hash fields
            header.hashMerkleRoot = type('Hash', (), {'hash': bytes.fromhex(header_data['hashMerkleRoot'])})()
            header.acceptedIDMerkleRoot = type('Hash', (), {'hash': bytes.fromhex(header_data['acceptedIDMerkleRoot'])})()
            header.utxoCommitment = type('Hash', (), {'hash': bytes.fromhex(header_data['utxoCommitment'])})()
            header.pruningPoint = type('Hash', (), {'hash': bytes.fromhex(header_data['pruningPoint'])})()
            
            # Copy numeric fields
            header.timeInMilliseconds = header_data['timeInMilliseconds']
            header.bits = header_data['bits']
            header.nonce = header_data['nonce']
            header.daaScore = header_data['daaScore']
            header.blueScore = header_data['blueScore']
            header.blueWork = bytes.fromhex(header_data['blueWork'])
            
            # Store in lookup
            self.headers[bytes.fromhex(header_data['hash'])] = header
    
    def get_raw_header(self, block_hash):
        """Get header by hash"""
        return self.headers.get(block_hash)
    
    def close(self):
        """No-op for compatibility"""
        pass