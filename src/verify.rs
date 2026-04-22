use anyhow::{Context, Result};
use std::path::Path;

use crate::hashing::{hash32_from_hex, header_hash, hex_of, transaction_hash};
use crate::output::{
    capture_output_line, format_duration_ms, now_millis, print_error, print_header, print_info,
    print_success, print_warning, prompt_continue_on_sync_warning,
};
use crate::store::open_store_with_resolved_input;
use crate::{
    BOLD, CHECKPOINT_HASH_HEX, CheckpointStore, Cli, EMPTY_MUHASH_HEX, END, GoStore,
    HARDWIRED_GENESIS_BITCOIN_BLOCK_HASH_HEX, HARDWIRED_GENESIS_HASH_HEX,
    HARDWIRED_GENESIS_TX_PAYLOAD_HEX, Hash32, HeaderSource, HeaderStore,
    MAINNET_SUBNETWORK_ID_COINBASE_HEX, ORIGINAL_GENESIS_BITCOIN_BLOCK_HASH_HEX,
    ORIGINAL_GENESIS_HASH_HEX, ORIGINAL_GENESIS_TX_PAYLOAD_HEX, OpenStoreResult,
    TIP_SYNC_WARNING_THRESHOLD_MS, Transaction, VerificationReport,
};

enum PreCheckpointSource {
    Embedded(CheckpointStore),
    External(GoStore),
}

impl HeaderSource for PreCheckpointSource {
    fn get_raw_header(&mut self, block_hash: &Hash32) -> Result<Option<crate::ParsedHeader>> {
        match self {
            Self::Embedded(store) => store.get_raw_header(block_hash),
            Self::External(store) => store.get_raw_header(block_hash),
        }
    }
}

fn choose_chain_tip_for_verification(tips: &[Hash32], _headers_selected_tip: Hash32) -> Hash32 {
    if let Some(first_tip) = tips.first().copied() {
        return first_tip;
    }

    [0u8; 32]
}

fn assert_cryptographic_hash_chain_to_genesis(
    source: &mut dyn HeaderSource,
    mut block_hash: Hash32,
    genesis_hash: Hash32,
    verbose: bool,
) -> Result<bool> {
    let mut steps: usize = 0;

    loop {
        if block_hash == genesis_hash {
            if verbose {
                print_info(&format!(
                    "✓ Reached genesis block via {steps} pruning points"
                ));
            }
            return Ok(true);
        }

        let Some(header) = source.get_raw_header(&block_hash)? else {
            print_error(&format!(
                "Header not found for hash: {}",
                hex_of(&block_hash)
            ));
            return Ok(false);
        };

        let calculated_hash = header_hash(&header);
        if calculated_hash != block_hash {
            print_error(&format!("Hash mismatch at block {}", hex_of(&block_hash)));
            print_error(&format!("  Expected: {}", hex_of(&block_hash)));
            print_error(&format!("  Got:      {}", hex_of(&calculated_hash)));
            return Ok(false);
        }

        if verbose {
            print_info(&format!(
                "  Step {}: {} -> {}",
                steps + 1,
                hex_of(&block_hash),
                hex_of(&header.pruning_point)
            ));
        }

        block_hash = header.pruning_point;
        steps += 1;

        if steps > 100_000 {
            print_error("Too many iterations in hash chain verification (safety stop)");
            return Ok(false);
        }
    }
}

pub(crate) fn hardwired_genesis_coinbase_tx() -> Result<Transaction> {
    genesis_coinbase_tx_from_payload_hex(HARDWIRED_GENESIS_TX_PAYLOAD_HEX)
}

pub(crate) fn original_genesis_coinbase_tx() -> Result<Transaction> {
    genesis_coinbase_tx_from_payload_hex(ORIGINAL_GENESIS_TX_PAYLOAD_HEX)
}

fn genesis_coinbase_tx_from_payload_hex(payload_hex: &str) -> Result<Transaction> {
    let subnetwork_id_bytes = hex::decode(MAINNET_SUBNETWORK_ID_COINBASE_HEX)
        .context("invalid coinbase subnetwork id constant")?;
    let mut subnetwork_id = [0u8; 20];
    subnetwork_id.copy_from_slice(&subnetwork_id_bytes);

    let payload = hex::decode(payload_hex).context("invalid genesis coinbase payload hex")?;

    Ok(Transaction {
        version: 0,
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: 0,
        subnetwork_id,
        gas: 0,
        payload,
        mass: 0,
    })
}

pub(crate) fn verify_genesis(
    store: &mut dyn HeaderStore,
    input_path: &Path,
    pre_checkpoint_datadir: Option<&Path>,
    probe_notes: &[String],
    verbose: bool,
    no_input: bool,
    report: &mut VerificationReport,
) -> Result<bool> {
    verify_genesis_with_prompt(
        store,
        input_path,
        pre_checkpoint_datadir,
        probe_notes,
        verbose,
        no_input,
        report,
        prompt_continue_on_sync_warning,
    )
}

fn verify_genesis_with_prompt<F>(
    store: &mut dyn HeaderStore,
    input_path: &Path,
    pre_checkpoint_datadir: Option<&Path>,
    probe_notes: &[String],
    verbose: bool,
    no_input: bool,
    report: &mut VerificationReport,
    _prompt_continue: F,
) -> Result<bool>
where
    F: FnMut(bool) -> Result<bool>,
{
    let hardwired_genesis = hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX)?;
    let original_genesis = hash32_from_hex(ORIGINAL_GENESIS_HASH_HEX)?;
    let checkpoint_hash = hash32_from_hex(CHECKPOINT_HASH_HEX)?;
    let empty_muhash = hash32_from_hex(EMPTY_MUHASH_HEX)?;

    print_header("Step 1: Database Connectivity Test");
    print_success("Database opened successfully");
    print_info(&format!("Using {}", store.store_name()));
    print_info(&format!("Input path: {}", input_path.display()));
    print_info(&format!(
        "Resolved DB path: {}",
        store.resolved_db_path().display()
    ));
    for note in probe_notes {
        print_info(note);
    }
    for note in store.resolution_notes() {
        print_info(note);
    }
    report.store_type = Some(store.store_name().to_string());
    report.resolved_input_path = Some(input_path.display().to_string());
    report.resolved_db_path = Some(store.resolved_db_path().display().to_string());

    print_header("Step 2: Current Chain State");
    let (tips, hst) = store.tips()?;
    let chain_tip = choose_chain_tip_for_verification(&tips, hst);
    print_info(&format!("Number of DAG tips: {}", tips.len()));
    print_info(&format!("Headers selected tip: {}", hex_of(&hst)));
    report.tips_count = Some(tips.len());
    report.headers_selected_tip = Some(hex_of(&hst));
    report.chain_tip_used = Some(hex_of(&chain_tip));
    report.tips = tips.iter().map(hex_of).collect();

    let hst_header = store.get_raw_header(&hst)?;
    if let Some(header) = hst_header.as_ref() {
        report.headers_selected_tip_timestamp_ms = Some(header.time_in_milliseconds);
        if chain_tip != hst {
            print_info(&format!(
                "Headers selected tip timestamp: {} ms",
                header.time_in_milliseconds
            ));
        }
    }

    let chain_tip_header = if chain_tip == [0u8; 32] {
        None
    } else if chain_tip == hst {
        hst_header
    } else {
        print_info(&format!(
            "Proof chain tip selected from DAG tips: {}",
            hex_of(&chain_tip)
        ));
        store.get_raw_header(&chain_tip)?
    };

    if let Some(chain_tip_header) = chain_tip_header {
        let tip_ts = chain_tip_header.time_in_milliseconds;
        print_info(&format!("Proof chain tip timestamp: {tip_ts} ms"));
        report.chain_tip_timestamp_ms = Some(tip_ts);
        let now = now_millis()?;
        if now >= tip_ts {
            let lag = now - tip_ts;
            print_info(&format!(
                "Tip age vs local clock: {}",
                format_duration_ms(lag)
            ));
            report.tip_age_ms = Some(lag);

            if lag > TIP_SYNC_WARNING_THRESHOLD_MS {
                report.sync_warning_triggered = true;
                print_warning(
                    "Node appears to still be syncing or is behind the network tip. This proof is valid for your current local tip; rerun after sync completes for latest-state verification.",
                );
                report.continued_after_sync_warning = Some(true);
                report.aborted_due_to_sync_warning = false;
                if no_input {
                    print_warning(
                        "Sync advisory prompt skipped due to --no-input; continuing automatically.",
                    );
                } else {
                    print_info("Continuing verification; sync advisory is warning-only.");
                }
            } else {
                print_success("Tip time is close to local clock (likely near latest network tip)");
            }
        } else {
            let lead = tip_ts - now;
            print_warning(&format!(
                "Tip timestamp is {} ahead of local clock. Check system time.",
                format_duration_ms(lead)
            ));
        }
    } else {
        print_warning(
            "Could not read proof-tip header timestamp, so sync status advisory is unavailable.",
        );
    }

    print_header("Step 3: Genesis Header Verification");

    let (active_genesis_hash, genesis_header, genesis_kind) =
        if let Some(header) = store.get_raw_header(&hardwired_genesis)? {
            (hardwired_genesis, header, "hardwired")
        } else if let Some(header) = store.get_raw_header(&original_genesis)? {
            (original_genesis, header, "original")
        } else {
            print_error("Neither hardwired nor original genesis headers were found");
            report.error =
                Some("neither hardwired nor original genesis headers were found".to_string());
            return Ok(false);
        };
    report.genesis_mode = Some(genesis_kind.to_string());
    report.active_genesis_hash = Some(hex_of(&active_genesis_hash));

    print_info(&format!("Detected genesis mode: {genesis_kind}"));
    print_info(&format!(
        "Expected genesis hash: {}",
        hex_of(&active_genesis_hash)
    ));

    let calculated_genesis_hash = header_hash(&genesis_header);
    print_info(&format!(
        "Calculated hash:      {}",
        hex_of(&calculated_genesis_hash)
    ));

    if calculated_genesis_hash != active_genesis_hash {
        print_error("Genesis header hash mismatch");
        report.error = Some("genesis header hash mismatch".to_string());
        return Ok(false);
    }

    print_success("Genesis header hash verified");
    print_info(&format!(
        "Genesis timestamp: {}",
        genesis_header.time_in_milliseconds
    ));
    print_info(&format!("Genesis DAA score: {}", genesis_header.daa_score));
    print_info(&format!(
        "Genesis blue score: {}",
        genesis_header.blue_score
    ));
    print_info(&format!(
        "Genesis bits (difficulty): {}",
        genesis_header.bits
    ));

    print_header("Step 4: Genesis Coinbase Transaction");
    if active_genesis_hash == hardwired_genesis {
        let genesis_coinbase_tx = hardwired_genesis_coinbase_tx()?;

        print_info("Genesis transaction properties:");
        print_info(&format!("  Version: {}", genesis_coinbase_tx.version));
        print_info(&format!(
            "  Inputs: {} (coinbase has no inputs)",
            genesis_coinbase_tx.inputs.len()
        ));
        print_info(&format!(
            "  Outputs: {} (coinbase has no outputs)",
            genesis_coinbase_tx.outputs.len()
        ));
        print_info(&format!(
            "  Payload size: {} bytes",
            genesis_coinbase_tx.payload.len()
        ));

        let calc_tx_hash = transaction_hash(&genesis_coinbase_tx, true);
        print_info(&format!("Calculated tx hash:    {}", hex_of(&calc_tx_hash)));
        print_info(&format!(
            "Expected merkle root:  {}",
            hex_of(&genesis_header.hash_merkle_root)
        ));

        if calc_tx_hash != genesis_header.hash_merkle_root {
            print_error("Genesis coinbase transaction hash mismatch");
            report.error = Some("genesis coinbase transaction hash mismatch".to_string());
            return Ok(false);
        }

        print_success("Genesis coinbase transaction verified");

        let hebrew_text = &genesis_coinbase_tx.payload[20..140];
        let bitcoin_hash = &genesis_coinbase_tx.payload[140..172];
        let checkpoint_ref = &genesis_coinbase_tx.payload[172..204];
        let expected_bitcoin_hash = hex::decode(HARDWIRED_GENESIS_BITCOIN_BLOCK_HASH_HEX)
            .context("invalid hardwired bitcoin block hash constant")?;

        print_info("Embedded data in genesis transaction:");
        print_info(&format!(
            "  Hebrew text: '{}'",
            String::from_utf8_lossy(hebrew_text)
        ));
        print_info(&format!(
            "  Bitcoin block reference: {}",
            hex::encode(bitcoin_hash)
        ));
        print_info("    (Bitcoin block #808080, provides timestamp anchor)");
        if bitcoin_hash != expected_bitcoin_hash.as_slice() {
            print_error("Hardwired genesis bitcoin block reference mismatch");
            report.error = Some("hardwired genesis bitcoin block reference mismatch".to_string());
            return Ok(false);
        }
        print_success("Bitcoin block reference verified");
        print_info(&format!(
            "  Checkpoint block reference: {}",
            hex::encode(checkpoint_ref)
        ));
        print_info("    (Kaspa checkpoint block for UTXO state)");
        if checkpoint_ref != checkpoint_hash {
            print_error("Hardwired genesis checkpoint block reference mismatch");
            report.error =
                Some("hardwired genesis checkpoint block reference mismatch".to_string());
            return Ok(false);
        }
        print_success("Checkpoint block reference verified");
    } else {
        print_info("Legacy/original genesis detected.");
        print_info(
            "Original genesis coinbase verification is performed in pre-checkpoint verification.",
        );
    }

    print_header("Step 5: Hash Chain Verification");
    if chain_tip == [0u8; 32] {
        print_error("No valid chain tip found to verify");
        report.error = Some("no valid chain tip found to verify".to_string());
        return Ok(false);
    }

    print_info(&format!(
        "Starting hash chain verification from tip: {}",
        hex_of(&chain_tip)
    ));
    print_info(&format!(
        "Target genesis hash: {}",
        hex_of(&active_genesis_hash)
    ));
    print_info("Verifying hash chain from current tip to genesis...");

    if !assert_cryptographic_hash_chain_to_genesis(store, chain_tip, active_genesis_hash, verbose)?
    {
        print_error("Hash chain verification failed");
        report.error = Some("hash chain verification failed".to_string());
        return Ok(false);
    }
    print_success("Hash chain from current state to genesis verified");

    print_header("Step 6: UTXO Commitment Analysis");
    let utxo_commitment = genesis_header.utxo_commitment;
    print_info(&format!(
        "Genesis UTXO commitment: {}",
        hex_of(&utxo_commitment)
    ));
    print_info(&format!(
        "Empty MuHash value:      {}",
        hex_of(&empty_muhash)
    ));

    if utxo_commitment.iter().all(|b| *b == 0) {
        print_info("Status: All-zero UTXO commitment (should not occur)");
    } else if utxo_commitment == empty_muhash {
        print_info("Status: Empty UTXO commitment (original genesis)");
    } else {
        print_info(
            "Status: Non-empty UTXO commitment (hardwired genesis with checkpoint UTXO set)",
        );
        print_info("This means the genesis contains a pre-calculated UTXO set from a checkpoint");
    }

    print_header("Step 7: Pre-Checkpoint Verification");
    let mut checkpoint_store = if let Some(pre_checkpoint_datadir) = pre_checkpoint_datadir {
        let store = GoStore::open(pre_checkpoint_datadir)?;
        print_success("Loaded external pre-checkpoint store");
        print_info(&format!(
            "Pre-checkpoint store path: {}",
            store.resolved_db_path().display()
        ));
        PreCheckpointSource::External(store)
    } else {
        let store = CheckpointStore::from_embedded_json()?;
        print_success("Loaded embedded checkpoint_data.json");
        print_info("(No 1GB pre-checkpoint database download required)");
        PreCheckpointSource::Embedded(store)
    };

    print_info(&format!(
        "Checkpoint hash:       {}",
        hex_of(&checkpoint_hash)
    ));
    print_info(&format!(
        "Original genesis hash: {}",
        hex_of(&original_genesis)
    ));

    if let Some(checkpoint_header) = checkpoint_store.get_raw_header(&checkpoint_hash)? {
        print_success("Checkpoint header found");
        let calculated_checkpoint_hash = header_hash(&checkpoint_header);
        if calculated_checkpoint_hash != checkpoint_hash {
            print_error("Checkpoint header hash mismatch");
            report.error = Some("checkpoint header hash mismatch".to_string());
            return Ok(false);
        }
        print_success("Checkpoint header hash verified");
        print_info(&format!(
            "Checkpoint UTXO commitment: {}",
            hex_of(&checkpoint_header.utxo_commitment)
        ));

        if active_genesis_hash == hardwired_genesis {
            if genesis_header.utxo_commitment == checkpoint_header.utxo_commitment {
                print_success("UTXO commitments match between hardwired genesis and checkpoint");
            } else {
                print_error("UTXO commitment mismatch between hardwired genesis and checkpoint");
                print_error(&format!(
                    "Genesis:    {}",
                    hex_of(&genesis_header.utxo_commitment)
                ));
                print_error(&format!(
                    "Checkpoint: {}",
                    hex_of(&checkpoint_header.utxo_commitment)
                ));
                report.error = Some(
                    "utxo commitment mismatch between hardwired genesis and checkpoint".to_string(),
                );
                return Ok(false);
            }
        }

        print_info("Verifying chain from checkpoint to original genesis...");
        if !assert_cryptographic_hash_chain_to_genesis(
            &mut checkpoint_store,
            checkpoint_hash,
            original_genesis,
            verbose,
        )? {
            print_error("Checkpoint chain verification failed");
            report.error = Some("checkpoint chain verification failed".to_string());
            return Ok(false);
        }

        print_success("Checkpoint chain to original genesis verified");

        if let Some(original_genesis_header) = checkpoint_store.get_raw_header(&original_genesis)? {
            let calculated_original_genesis_hash = header_hash(&original_genesis_header);
            if calculated_original_genesis_hash != original_genesis {
                print_error("Original genesis header hash mismatch");
                report.error = Some("original genesis header hash mismatch".to_string());
                return Ok(false);
            }

            let original_genesis_coinbase_tx = original_genesis_coinbase_tx()?;
            let original_calc_tx_hash = transaction_hash(&original_genesis_coinbase_tx, true);
            let original_bitcoin_hash = &original_genesis_coinbase_tx.payload[140..172];
            let expected_original_bitcoin_hash =
                hex::decode(ORIGINAL_GENESIS_BITCOIN_BLOCK_HASH_HEX)
                    .context("invalid original genesis bitcoin block hash constant")?;

            print_info(&format!(
                "Original genesis coinbase tx hash: {}",
                hex_of(&original_calc_tx_hash)
            ));
            print_info(&format!(
                "Original genesis merkle root:     {}",
                hex_of(&original_genesis_header.hash_merkle_root)
            ));
            print_info(&format!(
                "Original genesis bitcoin reference: {}",
                hex::encode(original_bitcoin_hash)
            ));

            if original_calc_tx_hash != original_genesis_header.hash_merkle_root {
                print_error("Original genesis coinbase transaction hash mismatch");
                report.error =
                    Some("original genesis coinbase transaction hash mismatch".to_string());
                return Ok(false);
            }

            if original_bitcoin_hash != expected_original_bitcoin_hash.as_slice() {
                print_error("Original genesis bitcoin block reference mismatch");
                report.error =
                    Some("original genesis bitcoin block reference mismatch".to_string());
                return Ok(false);
            }

            print_success("Original genesis coinbase transaction verified");
            print_success("Original genesis bitcoin block reference verified");
            print_info(&format!(
                "Original genesis UTXO commitment: {}",
                hex_of(&original_genesis_header.utxo_commitment)
            ));
            print_info(&format!(
                "Expected empty MuHash:            {}",
                hex_of(&empty_muhash)
            ));

            if original_genesis_header.utxo_commitment == empty_muhash {
                print_success("Original genesis has empty UTXO set verified!");
            } else {
                print_error("Original genesis UTXO commitment is not empty");
                report.error = Some("original genesis UTXO commitment is not empty".to_string());
                return Ok(false);
            }
        } else {
            print_error("Original genesis header not found in checkpoint dataset");
            report.error =
                Some("original genesis header not found in checkpoint dataset".to_string());
            return Ok(false);
        }
    } else {
        print_error("Checkpoint header not found in checkpoint dataset");
        report.error = Some("checkpoint header not found in checkpoint dataset".to_string());
        return Ok(false);
    }

    print_header("Verification Summary");
    print_success("All cryptographic verifications passed!");
    print_info("Verification details:");
    print_info(&format!(
        "  ✓ Active genesis hash: {}",
        hex_of(&active_genesis_hash)
    ));
    if active_genesis_hash == hardwired_genesis {
        print_info("  ✓ Hardwired genesis coinbase transaction verified");
    } else {
        print_info("  ✓ Legacy genesis mode detected");
    }
    print_info("  ✓ Hash chain from current tip to genesis verified");
    print_info("  ✓ UTXO commitment analysis completed");
    print_info("  ✓ Pre-checkpoint verification completed");
    print_info("  ✓ Original genesis coinbase transaction verified");
    print_info("  ✓ Original genesis empty UTXO set verified");

    print_success("The Kaspa blockchain integrity has been verified");
    print_success("No premine detected - UTXO set evolved from empty state");
    println!("\n{BOLD}Thank you for verifying the integrity of Kaspa!{END}");
    capture_output_line("");
    capture_output_line("Thank you for verifying the integrity of Kaspa!");

    Ok(true)
}

pub(crate) fn run(cli: &Cli, report: &mut VerificationReport) -> Result<bool> {
    let OpenStoreResult {
        mut store,
        input_path,
        probe_notes,
    } = open_store_with_resolved_input(cli)?;

    verify_genesis(
        &mut *store,
        &input_path,
        cli.pre_checkpoint_datadir.as_deref(),
        &probe_notes,
        cli.verbose,
        cli.no_input,
        report,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;
    use rusty_leveldb::{DB as LevelDb, Options as LevelOptions};
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    use crate::ParsedHeader;
    use crate::hashing::{hash32_from_hex, header_hash};
    use crate::output::clear_output_capture;
    use crate::proto;

    struct FakeStore {
        headers: HashMap<Hash32, ParsedHeader>,
        tips: Vec<Hash32>,
        headers_selected_tip: Hash32,
        db_path: PathBuf,
        notes: Vec<String>,
    }

    impl HeaderSource for FakeStore {
        fn get_raw_header(&mut self, block_hash: &Hash32) -> Result<Option<ParsedHeader>> {
            Ok(self.headers.get(block_hash).cloned())
        }
    }

    impl HeaderStore for FakeStore {
        fn store_name(&self) -> &'static str {
            "Fake test store"
        }

        fn resolved_db_path(&self) -> &Path {
            &self.db_path
        }

        fn resolution_notes(&self) -> &[String] {
            &self.notes
        }

        fn tips(&mut self) -> Result<(Vec<Hash32>, Hash32)> {
            Ok((self.tips.clone(), self.headers_selected_tip))
        }
    }

    fn test_hash(fill: u8) -> Hash32 {
        [fill; 32]
    }

    fn base_report() -> VerificationReport {
        VerificationReport {
            requested_node_type: "test".to_string(),
            ..VerificationReport::default()
        }
    }

    fn hardwired_genesis_header() -> ParsedHeader {
        ParsedHeader {
            version: 0,
            parents: Vec::new(),
            hash_merkle_root: hash32_from_hex(
                "8ec898568c6801d13df4ee6e2a1b54b7e6236f671f20954f05306410518eeb32",
            )
            .expect("hash merkle root"),
            accepted_id_merkle_root: [0u8; 32],
            utxo_commitment: hash32_from_hex(
                "710f27df423e63aa6cdb72b89ea5a06cffa399d66f167704455b5af59def8e20",
            )
            .expect("utxo commitment"),
            time_in_milliseconds: 1_637_609_671_037,
            bits: 486_722_099,
            nonce: 211_244,
            daa_score: 1_312_860,
            blue_score: 0,
            blue_work_trimmed_be: Vec::new(),
            pruning_point: [0u8; 32],
        }
    }

    fn original_genesis_header() -> ParsedHeader {
        let mut checkpoint_store = CheckpointStore::from_embedded_json().expect("checkpoint store");
        let original_genesis_hash =
            hash32_from_hex(ORIGINAL_GENESIS_HASH_HEX).expect("original genesis hash");
        checkpoint_store
            .get_raw_header(&original_genesis_hash)
            .expect("read original genesis")
            .expect("original genesis header")
    }

    fn make_tip_header_with_blue_work(
        pruning_point: Hash32,
        time_in_milliseconds: u64,
        blue_work_trimmed_be: Vec<u8>,
    ) -> (Hash32, ParsedHeader) {
        let header = ParsedHeader {
            version: 1,
            parents: vec![vec![test_hash(0x11)]],
            hash_merkle_root: test_hash(0x22),
            accepted_id_merkle_root: test_hash(0x33),
            utxo_commitment: test_hash(0x44),
            time_in_milliseconds,
            bits: 0x1d00ffff,
            nonce: 42,
            daa_score: 7,
            blue_score: 8,
            blue_work_trimmed_be,
            pruning_point,
        };
        let hash = header_hash(&header);
        (hash, header)
    }

    fn make_tip_header(pruning_point: Hash32, time_in_milliseconds: u64) -> (Hash32, ParsedHeader) {
        make_tip_header_with_blue_work(pruning_point, time_in_milliseconds, vec![0x01, 0x02, 0x03])
    }

    fn create_go_leveldb(db_path: &Path, entries: &[(Vec<u8>, Vec<u8>)]) {
        fs::create_dir_all(db_path).expect("create go db dir");

        let mut opts = LevelOptions::default();
        opts.create_if_missing = true;

        let mut db = LevelDb::open(db_path, opts).expect("open go db");
        for (key, value) in entries {
            db.put(key, value).expect("write go db key");
        }
        db.flush().expect("flush go db");
        db.close().expect("close go db");
    }

    fn encode_db_block_header(header: &ParsedHeader) -> Vec<u8> {
        proto::DbBlockHeader {
            version: u32::from(header.version),
            parents: header
                .parents
                .iter()
                .map(|level| proto::DbBlockLevelParents {
                    parent_hashes: level
                        .iter()
                        .map(|hash| proto::DbHash {
                            hash: hash.to_vec(),
                        })
                        .collect(),
                })
                .collect(),
            hash_merkle_root: Some(proto::DbHash {
                hash: header.hash_merkle_root.to_vec(),
            }),
            accepted_id_merkle_root: Some(proto::DbHash {
                hash: header.accepted_id_merkle_root.to_vec(),
            }),
            utxo_commitment: Some(proto::DbHash {
                hash: header.utxo_commitment.to_vec(),
            }),
            time_in_milliseconds: i64::try_from(header.time_in_milliseconds)
                .expect("header timestamp fits in i64"),
            bits: header.bits,
            nonce: header.nonce,
            daa_score: header.daa_score,
            blue_work: header.blue_work_trimmed_be.clone(),
            pruning_point: Some(proto::DbHash {
                hash: header.pruning_point.to_vec(),
            }),
            blue_score: header.blue_score,
        }
        .encode_to_vec()
    }

    fn go_bucketed_key(active_prefix: u8, bucket: &[u8], suffix: Option<&[u8]>) -> Vec<u8> {
        let mut key =
            Vec::with_capacity(2 + bucket.len() + suffix.map(|s| 1 + s.len()).unwrap_or(0));
        key.push(active_prefix);
        key.push(b'/');
        key.extend_from_slice(bucket);
        if let Some(suffix) = suffix {
            key.push(b'/');
            key.extend_from_slice(suffix);
        }
        key
    }

    fn fake_store_with_tip(
        genesis_hash: Hash32,
        genesis_header: ParsedHeader,
        tip_time_in_milliseconds: u64,
    ) -> FakeStore {
        let (tip_hash, tip_header) = make_tip_header(genesis_hash, tip_time_in_milliseconds);
        let mut headers = HashMap::new();
        headers.insert(genesis_hash, genesis_header);
        headers.insert(tip_hash, tip_header);

        FakeStore {
            headers,
            tips: vec![tip_hash],
            headers_selected_tip: tip_hash,
            db_path: PathBuf::from("/tmp/fake-db"),
            notes: vec!["fixture note".to_string()],
        }
    }

    #[test]
    fn choose_chain_tip_prefers_first_dag_tip_even_when_hst_differs() {
        let first_tip = test_hash(0x11);
        let tips = vec![first_tip, test_hash(0x22)];
        let headers_selected_tip = test_hash(0x77);

        assert_eq!(
            choose_chain_tip_for_verification(&tips, headers_selected_tip),
            first_tip
        );
    }

    #[test]
    fn choose_chain_tip_uses_first_dag_tip_even_when_hst_is_also_present() {
        let first_tip = test_hash(0x11);
        let headers_selected_tip = test_hash(0x77);
        let tips = vec![first_tip, headers_selected_tip];

        assert_eq!(
            choose_chain_tip_for_verification(&tips, headers_selected_tip),
            first_tip
        );
    }

    #[test]
    fn choose_chain_tip_returns_zero_when_no_dag_tips_exist_even_if_hst_exists() {
        let headers_selected_tip = test_hash(0x77);

        assert_eq!(
            choose_chain_tip_for_verification(&[], headers_selected_tip),
            [0u8; 32]
        );
    }

    #[test]
    fn choose_chain_tip_returns_zero_when_no_dag_tip_or_selected_tip_exists() {
        assert_eq!(choose_chain_tip_for_verification(&[], [0u8; 32]), [0u8; 32]);
    }

    #[test]
    fn verify_genesis_succeeds_for_hardwired_mode() {
        clear_output_capture();
        let tip_time = now_millis().expect("now");
        let hardwired_genesis =
            hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX).expect("hardwired genesis hash");
        let mut store =
            fake_store_with_tip(hardwired_genesis, hardwired_genesis_header(), tip_time);
        let mut report = base_report();

        let result = verify_genesis_with_prompt(
            &mut store,
            Path::new("/tmp/fake-input"),
            None,
            &["probe note".to_string()],
            false,
            false,
            &mut report,
            |_| -> Result<bool> { panic!("sync warning prompt should not run") },
        )
        .expect("verify genesis");

        assert!(result);
        assert_eq!(report.genesis_mode.as_deref(), Some("hardwired"));
        assert_eq!(report.tips_count, Some(1));
        assert_eq!(report.store_type.as_deref(), Some("Fake test store"));
        assert_eq!(report.error, None);
        assert_eq!(report.tips.len(), 1);
    }

    #[test]
    fn verify_genesis_succeeds_for_original_mode() {
        clear_output_capture();
        let tip_time = now_millis().expect("now");
        let original_genesis =
            hash32_from_hex(ORIGINAL_GENESIS_HASH_HEX).expect("original genesis hash");
        let mut store = fake_store_with_tip(original_genesis, original_genesis_header(), tip_time);
        let mut report = base_report();

        let result = verify_genesis_with_prompt(
            &mut store,
            Path::new("/tmp/fake-input"),
            None,
            &[],
            false,
            false,
            &mut report,
            |_| -> Result<bool> { panic!("sync warning prompt should not run") },
        )
        .expect("verify genesis");

        assert!(result);
        assert_eq!(report.genesis_mode.as_deref(), Some("original"));
        assert_eq!(
            report.active_genesis_hash.as_deref(),
            Some(ORIGINAL_GENESIS_HASH_HEX)
        );
        assert_eq!(report.error, None);
    }

    #[test]
    fn verify_genesis_accepts_external_pre_checkpoint_store() {
        clear_output_capture();
        let tempdir = TempDir::new().expect("tempdir");
        let db_path = tempdir.path().join("datadir2");
        let active_prefix = 0u8;
        let checkpoint_store = CheckpointStore::from_embedded_json().expect("checkpoint store");
        let mut entries = vec![(b"active-prefix".to_vec(), vec![active_prefix])];
        for (hash, header) in &checkpoint_store.headers {
            entries.push((
                go_bucketed_key(active_prefix, b"block-headers", Some(hash)),
                encode_db_block_header(header),
            ));
        }

        create_go_leveldb(&db_path, &entries);

        let tip_time = now_millis().expect("now");
        let hardwired_genesis =
            hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX).expect("hardwired genesis hash");
        let mut current_store =
            fake_store_with_tip(hardwired_genesis, hardwired_genesis_header(), tip_time);
        let mut report = base_report();

        let result = verify_genesis_with_prompt(
            &mut current_store,
            Path::new("/tmp/fake-input"),
            Some(db_path.as_path()),
            &[],
            false,
            true,
            &mut report,
            |_| Ok(false),
        )
        .expect("verify genesis");

        assert!(result);
        assert_eq!(report.error, None);
    }

    #[test]
    fn verify_genesis_prefers_real_dag_tip_over_headers_selected_tip() {
        clear_output_capture();
        let tip_time = now_millis().expect("now");
        let hardwired_genesis =
            hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX).expect("hardwired genesis hash");
        let (real_tip_hash, real_tip_header) = make_tip_header(hardwired_genesis, tip_time);
        let headers_selected_tip = test_hash(0xfe);
        let mut headers = HashMap::new();
        headers.insert(hardwired_genesis, hardwired_genesis_header());
        headers.insert(real_tip_hash, real_tip_header);

        let mut store = FakeStore {
            headers,
            tips: vec![real_tip_hash],
            headers_selected_tip,
            db_path: PathBuf::from("/tmp/fake-db"),
            notes: Vec::new(),
        };
        let mut report = base_report();

        let result = verify_genesis_with_prompt(
            &mut store,
            Path::new("/tmp/fake-input"),
            None,
            &[],
            false,
            false,
            &mut report,
            |_| -> Result<bool> { panic!("sync warning prompt should not run") },
        )
        .expect("verify genesis");

        let expected_hst = hex_of(&headers_selected_tip);
        let expected_chain_tip = hex_of(&real_tip_hash);
        assert!(result);
        assert_eq!(
            report.headers_selected_tip.as_deref(),
            Some(expected_hst.as_str())
        );
        assert_eq!(
            report.chain_tip_used.as_deref(),
            Some(expected_chain_tip.as_str())
        );
        assert_eq!(report.error, None);
    }

    #[test]
    fn verify_genesis_uses_real_chain_tip_for_sync_age_when_hst_is_stale() {
        clear_output_capture();
        let fresh_tip_time = now_millis().expect("now");
        let stale_hst_time = fresh_tip_time.saturating_sub(TIP_SYNC_WARNING_THRESHOLD_MS + 1);
        let hardwired_genesis =
            hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX).expect("hardwired genesis hash");
        let (real_tip_hash, real_tip_header) = make_tip_header(hardwired_genesis, fresh_tip_time);
        let headers_selected_tip = test_hash(0xfd);
        let (_unused_hash, stale_hst_header) = make_tip_header(hardwired_genesis, stale_hst_time);
        let mut headers = HashMap::new();
        headers.insert(hardwired_genesis, hardwired_genesis_header());
        headers.insert(real_tip_hash, real_tip_header);
        headers.insert(headers_selected_tip, stale_hst_header);

        let mut store = FakeStore {
            headers,
            tips: vec![real_tip_hash],
            headers_selected_tip,
            db_path: PathBuf::from("/tmp/fake-db"),
            notes: Vec::new(),
        };
        let mut report = base_report();

        let result = verify_genesis_with_prompt(
            &mut store,
            Path::new("/tmp/fake-input"),
            None,
            &[],
            false,
            false,
            &mut report,
            |_| -> Result<bool> { panic!("sync warning prompt should not run") },
        )
        .expect("verify genesis");

        assert!(result);
        assert_eq!(report.chain_tip_timestamp_ms, Some(fresh_tip_time));
        assert_eq!(
            report.headers_selected_tip_timestamp_ms,
            Some(stale_hst_time)
        );
        assert!(!report.sync_warning_triggered);
        assert!(report.tip_age_ms.unwrap_or(u64::MAX) < TIP_SYNC_WARNING_THRESHOLD_MS);
    }

    #[test]
    fn verify_genesis_uses_real_chain_tip_for_sync_warning_when_hst_is_fresh() {
        clear_output_capture();
        let fresh_hst_time = now_millis().expect("now");
        let stale_tip_time = fresh_hst_time.saturating_sub(TIP_SYNC_WARNING_THRESHOLD_MS + 1);
        let hardwired_genesis =
            hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX).expect("hardwired genesis hash");
        let (real_tip_hash, real_tip_header) = make_tip_header(hardwired_genesis, stale_tip_time);
        let headers_selected_tip = test_hash(0xfc);
        let (_unused_hash, fresh_hst_header) = make_tip_header(hardwired_genesis, fresh_hst_time);
        let mut headers = HashMap::new();
        headers.insert(hardwired_genesis, hardwired_genesis_header());
        headers.insert(real_tip_hash, real_tip_header);
        headers.insert(headers_selected_tip, fresh_hst_header);

        let mut store = FakeStore {
            headers,
            tips: vec![real_tip_hash],
            headers_selected_tip,
            db_path: PathBuf::from("/tmp/fake-db"),
            notes: Vec::new(),
        };
        let mut report = base_report();
        let mut prompt_calls = 0usize;

        let result = verify_genesis_with_prompt(
            &mut store,
            Path::new("/tmp/fake-input"),
            None,
            &[],
            false,
            false,
            &mut report,
            |_| {
                prompt_calls += 1;
                Ok(true)
            },
        )
        .expect("verify genesis");

        assert!(result);
        assert_eq!(prompt_calls, 0);
        assert_eq!(report.chain_tip_timestamp_ms, Some(stale_tip_time));
        assert_eq!(
            report.headers_selected_tip_timestamp_ms,
            Some(fresh_hst_time)
        );
        assert!(report.sync_warning_triggered);
        assert_eq!(report.continued_after_sync_warning, Some(true));
        assert!(report.tip_age_ms.unwrap_or(0) > TIP_SYNC_WARNING_THRESHOLD_MS);
    }

    #[test]
    fn verify_genesis_records_sync_warning_and_continues_without_prompt() {
        clear_output_capture();
        let stale_tip_time = now_millis()
            .expect("now")
            .saturating_sub(TIP_SYNC_WARNING_THRESHOLD_MS + 1);
        let hardwired_genesis =
            hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX).expect("hardwired genesis hash");
        let mut store = fake_store_with_tip(
            hardwired_genesis,
            hardwired_genesis_header(),
            stale_tip_time,
        );
        let mut report = base_report();
        let mut prompt_calls = 0usize;

        let result = verify_genesis_with_prompt(
            &mut store,
            Path::new("/tmp/fake-input"),
            None,
            &[],
            false,
            false,
            &mut report,
            |no_input| {
                prompt_calls += 1;
                assert!(!no_input);
                Ok(true)
            },
        )
        .expect("verify genesis");

        assert!(result);
        assert_eq!(prompt_calls, 0);
        assert!(report.sync_warning_triggered);
        assert_eq!(report.continued_after_sync_warning, Some(true));
        assert!(!report.aborted_due_to_sync_warning);
        assert_eq!(report.error, None);
    }

    #[test]
    fn verify_genesis_sync_warning_never_aborts_proof_flow() {
        clear_output_capture();
        let stale_tip_time = now_millis()
            .expect("now")
            .saturating_sub(TIP_SYNC_WARNING_THRESHOLD_MS + 1);
        let hardwired_genesis =
            hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX).expect("hardwired genesis hash");
        let mut store = fake_store_with_tip(
            hardwired_genesis,
            hardwired_genesis_header(),
            stale_tip_time,
        );
        let mut report = base_report();
        let mut prompt_calls = 0usize;

        let result = verify_genesis_with_prompt(
            &mut store,
            Path::new("/tmp/fake-input"),
            None,
            &[],
            false,
            false,
            &mut report,
            |no_input| {
                prompt_calls += 1;
                assert!(!no_input);
                Ok(false)
            },
        )
        .expect("verify genesis");

        assert!(result);
        assert_eq!(prompt_calls, 0);
        assert!(report.sync_warning_triggered);
        assert_eq!(report.continued_after_sync_warning, Some(true));
        assert!(!report.aborted_due_to_sync_warning);
        assert_eq!(report.error, None);
    }

    #[test]
    fn verify_genesis_fails_when_tip_header_is_missing() {
        clear_output_capture();
        let hardwired_genesis =
            hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX).expect("hardwired genesis hash");
        let missing_tip_hash = test_hash(0xaa);
        let mut headers = HashMap::new();
        headers.insert(hardwired_genesis, hardwired_genesis_header());

        let mut store = FakeStore {
            headers,
            tips: vec![missing_tip_hash],
            headers_selected_tip: missing_tip_hash,
            db_path: PathBuf::from("/tmp/fake-db"),
            notes: Vec::new(),
        };
        let mut report = base_report();

        let result = verify_genesis_with_prompt(
            &mut store,
            Path::new("/tmp/fake-input"),
            None,
            &[],
            false,
            false,
            &mut report,
            |_| -> Result<bool> { panic!("sync warning prompt should not run") },
        )
        .expect("verify genesis");

        assert!(!result);
        assert_eq!(
            report.error.as_deref(),
            Some("hash chain verification failed")
        );
    }

    #[test]
    fn verify_genesis_fails_when_tip_hash_does_not_match_header_contents() {
        clear_output_capture();
        let tip_time = now_millis().expect("now");
        let hardwired_genesis =
            hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX).expect("hardwired genesis hash");
        let wrong_tip_hash = test_hash(0xbb);
        let (_actual_tip_hash, tip_header) = make_tip_header(hardwired_genesis, tip_time);
        let mut headers = HashMap::new();
        headers.insert(hardwired_genesis, hardwired_genesis_header());
        headers.insert(wrong_tip_hash, tip_header);

        let mut store = FakeStore {
            headers,
            tips: vec![wrong_tip_hash],
            headers_selected_tip: wrong_tip_hash,
            db_path: PathBuf::from("/tmp/fake-db"),
            notes: Vec::new(),
        };
        let mut report = base_report();

        let result = verify_genesis_with_prompt(
            &mut store,
            Path::new("/tmp/fake-input"),
            None,
            &[],
            false,
            false,
            &mut report,
            |_| -> Result<bool> { panic!("sync warning prompt should not run") },
        )
        .expect("verify genesis");

        assert!(!result);
        assert_eq!(
            report.error.as_deref(),
            Some("hash chain verification failed")
        );
    }
}
