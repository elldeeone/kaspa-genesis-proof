use anyhow::{Context, Result};
use std::path::Path;

use crate::checkpoint_utxo::{
    CHECKPOINT_UTXO_DUMP_SOURCE_LABEL, CHECKPOINT_UTXO_DUMP_SOURCE_URL,
    format_kas_amount_from_sompi, format_sompi_amount, reference_baseline_sompi,
    verify_checkpoint_utxo_dump,
};
use crate::hashing::{hash32_from_hex, header_hash, hex_of, transaction_hash};
use crate::model::{
    Hash32, HeaderSource, HeaderStore, ParsedHeader, Transaction, VerificationReport,
};
use crate::output::{
    capture_output_line, format_duration_ms, now_millis, print_error, print_header, print_info,
    print_success, print_warning, prompt_continue_on_sync_warning,
};
use crate::store::{CheckpointStore, GoStore, OpenStoreResult, open_store_with_resolved_input};
use crate::{
    cli::Cli,
    constants::{
        BOLD, CHECKPOINT_HASH_HEX, EMPTY_MUHASH_HEX, END, HARDWIRED_GENESIS_BITCOIN_BLOCK_HASH_HEX,
        HARDWIRED_GENESIS_HASH_HEX, HARDWIRED_GENESIS_TX_PAYLOAD_HEX,
        MAINNET_SUBNETWORK_ID_COINBASE_HEX, ORIGINAL_GENESIS_BITCOIN_BLOCK_HASH_HEX,
        ORIGINAL_GENESIS_HASH_HEX, ORIGINAL_GENESIS_TX_PAYLOAD_HEX, TIP_SYNC_WARNING_THRESHOLD_MS,
    },
};

enum PreCheckpointSource {
    Embedded(CheckpointStore),
    External(Box<GoStore>),
}

#[derive(Clone, Copy)]
pub(crate) struct VerificationInputs<'a> {
    input_path: &'a Path,
    pre_checkpoint_datadir: Option<&'a Path>,
    checkpoint_utxos_gz: Option<&'a Path>,
    probe_notes: &'a [String],
    verbose: bool,
    no_input: bool,
}

impl HeaderSource for PreCheckpointSource {
    fn get_raw_header(&mut self, block_hash: &Hash32) -> Result<Option<ParsedHeader>> {
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
    inputs: VerificationInputs<'_>,
    report: &mut VerificationReport,
) -> Result<bool> {
    verify_genesis_with_prompt(store, inputs, report, prompt_continue_on_sync_warning)
}

fn verify_genesis_with_prompt<F>(
    store: &mut dyn HeaderStore,
    inputs: VerificationInputs<'_>,
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
    print_info(&format!("Input path: {}", inputs.input_path.display()));
    print_info(&format!(
        "Resolved DB path: {}",
        store.resolved_db_path().display()
    ));
    for note in inputs.probe_notes {
        print_info(note);
    }
    for note in store.resolution_notes() {
        print_info(note);
    }
    report.store_type = Some(store.store_name().to_string());
    report.resolved_input_path = Some(inputs.input_path.display().to_string());
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
                if inputs.no_input {
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

    if !assert_cryptographic_hash_chain_to_genesis(
        store,
        chain_tip,
        active_genesis_hash,
        inputs.verbose,
    )? {
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
    let mut checkpoint_store = if let Some(pre_checkpoint_datadir) = inputs.pre_checkpoint_datadir {
        let store = GoStore::open(pre_checkpoint_datadir)?;
        print_info("Trust model: operator-supplied pre-checkpoint store");
        print_success("Loaded external pre-checkpoint store");
        print_info(&format!(
            "Pre-checkpoint store path: {}",
            store.resolved_db_path().display()
        ));
        PreCheckpointSource::External(Box::new(store))
    } else {
        let store = CheckpointStore::from_embedded_json()?;
        print_info(
            "Trust model: embedded pre-checkpoint data by default (override with --pre-checkpoint-datadir PATH)",
        );
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
            inputs.verbose,
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

        print_header("Step 8: Checkpoint UTXO Dump Verification");
        if let Some(path) = inputs.checkpoint_utxos_gz {
            print_info("Trust model: operator-supplied checkpoint UTXO dump");
            print_info(&format!(
                "Using operator-supplied checkpoint utxos.gz: {}",
                path.display()
            ));
            print_info(&format!(
                "Canonical upstream reference: {CHECKPOINT_UTXO_DUMP_SOURCE_URL}"
            ));
        } else {
            print_info(
                "Trust model: embedded checkpoint dump by default (override with --checkpoint-utxos-gz PATH)",
            );
            print_info(&format!(
                "Using embedded canonical checkpoint dump: {CHECKPOINT_UTXO_DUMP_SOURCE_LABEL}"
            ));
            print_info(&format!(
                "Canonical upstream reference: {CHECKPOINT_UTXO_DUMP_SOURCE_URL}"
            ));
        }
        print_info(
            "Streaming the selected checkpoint dump through MuHash and the Go-format UTXO parser...",
        );

        let checkpoint_dump = match verify_checkpoint_utxo_dump(
            checkpoint_header.utxo_commitment,
            inputs.checkpoint_utxos_gz,
        ) {
            Ok(scan) => scan,
            Err(err) => {
                print_error(&format!(
                    "Checkpoint UTXO dump verification failed: {err:#}"
                ));
                report.error = Some("checkpoint UTXO dump verification failed".to_string());
                return Ok(false);
            }
        };
        let checkpoint_scan = checkpoint_dump.scan;
        let source_label = checkpoint_dump.source_label;
        let source_url = checkpoint_dump.source_url;
        let used_operator_supplied_file = checkpoint_dump.used_operator_supplied_file;

        let reference_baseline = match reference_baseline_sompi(checkpoint_header.daa_score) {
            Ok(value) => value,
            Err(err) => {
                print_error(&format!(
                    "Failed computing checkpoint reference baseline: {err:#}"
                ));
                report.error = Some("checkpoint reference baseline calculation failed".to_string());
                return Ok(false);
            }
        };
        let checkpoint_excess = match checkpoint_scan.total_sompi.checked_sub(reference_baseline) {
            Some(value) => value,
            None => {
                print_error(
                    "Checkpoint total is unexpectedly below the reference emission schedule",
                );
                report.error =
                    Some("checkpoint total below reference emission schedule".to_string());
                return Ok(false);
            }
        };

        print_info(&format!(
            "Compressed dump size: {} bytes",
            format_sompi_amount(checkpoint_scan.compressed_size_bytes)
        ));
        print_info(&format!(
            "Framed UTXO records: {}",
            format_sompi_amount(checkpoint_scan.record_count)
        ));
        print_info(&format!(
            "Computed MuHash:      {}",
            hex_of(&checkpoint_scan.commitment)
        ));
        print_info(&format!(
            "Expected commitment:  {}",
            hex_of(&checkpoint_header.utxo_commitment)
        ));
        print_success("Checkpoint dump MuHash matches the checkpoint header UTXO commitment");
        if active_genesis_hash == hardwired_genesis {
            print_success(
                "Checkpoint dump MuHash is the same commitment carried by the hardwired genesis",
            );
        }
        if used_operator_supplied_file {
            print_success(
                "Operator-supplied checkpoint dump verified against the hardwired commitment",
            );
        }
        print_success(&format!(
            "Verified checkpoint total: {} sompi",
            format_sompi_amount(checkpoint_scan.total_sompi)
        ));
        print_success(&format!(
            "Verified checkpoint total: {} KAS",
            format_kas_amount_from_sompi(checkpoint_scan.total_sompi)
        ));
        print_info(&format!(
            "Reference schedule at checkpoint DAA {}: {} KAS",
            checkpoint_header.daa_score,
            format_kas_amount_from_sompi(reference_baseline)
        ));
        print_info(&format!(
            "Excess over 500 KAS reference schedule: {} KAS",
            format_kas_amount_from_sompi(checkpoint_excess)
        ));

        report.checkpoint_utxo_dump_verified = true;
        report.checkpoint_utxo_dump_source = Some(source_label);
        report.checkpoint_utxo_dump_source_url = Some(source_url.to_string());
        report.checkpoint_utxo_dump_records = Some(checkpoint_scan.record_count);
        report.checkpoint_utxo_commitment = Some(hex_of(&checkpoint_scan.commitment));
        report.checkpoint_daa_score = Some(checkpoint_header.daa_score);
        report.checkpoint_total_sompi = Some(checkpoint_scan.total_sompi.to_string());
        report.checkpoint_total_kas =
            Some(format_kas_amount_from_sompi(checkpoint_scan.total_sompi));
        report.checkpoint_reference_baseline_kas =
            Some(format_kas_amount_from_sompi(reference_baseline));
        report.checkpoint_excess_over_reference_kas =
            Some(format_kas_amount_from_sompi(checkpoint_excess));
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
    print_info("  ✓ Checkpoint UTXO dump MuHash verified");
    print_info("  ✓ Checkpoint total supply verified from canonical dump");
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
        VerificationInputs {
            input_path: &input_path,
            pre_checkpoint_datadir: cli.pre_checkpoint_datadir.as_deref(),
            checkpoint_utxos_gz: cli.checkpoint_utxos_gz.as_deref(),
            probe_notes: &probe_notes,
            verbose: cli.verbose,
            no_input: cli.no_input,
        },
        report,
    )
}

#[cfg(test)]
mod tests;
