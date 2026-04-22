use std::collections::HashMap;
use std::path::Path;

use crate::constants::{
    CHECKPOINT_HASH_HEX, HARDWIRED_GENESIS_BITCOIN_BLOCK_HASH_HEX, HARDWIRED_GENESIS_HASH_HEX,
    ORIGINAL_GENESIS_BITCOIN_BLOCK_HASH_HEX, ORIGINAL_GENESIS_HASH_HEX,
    TIP_SYNC_WARNING_THRESHOLD_MS,
};
use crate::hashing::{hash32_from_hex, header_hash, hex_of, transaction_hash};
use crate::output::{clear_output_capture, now_millis};
use crate::test_support::{
    FakeStore, base_report, create_go_leveldb, embedded_checkpoint_headers_for_external_store,
    fake_store_with_tip, hardwired_genesis_header, make_tip_header, original_genesis_header,
    test_hash,
};

use super::{
    VerificationInputs, choose_chain_tip_for_verification, hardwired_genesis_coinbase_tx,
    original_genesis_coinbase_tx, verify_genesis_with_prompt,
};

fn test_inputs<'a>(
    input_path: &'a Path,
    pre_checkpoint_datadir: Option<&'a Path>,
    checkpoint_utxos_gz: Option<&'a Path>,
    probe_notes: &'a [String],
    verbose: bool,
    no_input: bool,
) -> VerificationInputs<'a> {
    VerificationInputs {
        input_path,
        pre_checkpoint_datadir,
        checkpoint_utxos_gz,
        probe_notes,
        verbose,
        no_input,
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
fn hardwired_genesis_coinbase_tx_hash_matches_live_node_merkle_root() {
    let tx = hardwired_genesis_coinbase_tx().expect("hardwired tx");
    let tx_hash = transaction_hash(&tx, true);

    assert_eq!(
        hex_of(&tx_hash),
        "8ec898568c6801d13df4ee6e2a1b54b7e6236f671f20954f05306410518eeb32"
    );
}

#[test]
fn hardwired_genesis_payload_embeds_expected_bitcoin_and_checkpoint_hashes() {
    let tx = hardwired_genesis_coinbase_tx().expect("hardwired tx");

    assert_eq!(
        hex::encode(&tx.payload[140..172]),
        HARDWIRED_GENESIS_BITCOIN_BLOCK_HASH_HEX
    );
    assert_eq!(hex::encode(&tx.payload[172..204]), CHECKPOINT_HASH_HEX);
}

#[test]
fn original_genesis_coinbase_tx_hash_matches_original_genesis_merkle_root() {
    let tx = original_genesis_coinbase_tx().expect("original genesis tx");
    let tx_hash = transaction_hash(&tx, true);

    assert_eq!(
        hex_of(&tx_hash),
        "caedaf7d4a08bbe89011640c4841b66d5bba67d7288ce6d67228db000966e974"
    );
}

#[test]
fn original_genesis_payload_embeds_expected_bitcoin_hash() {
    let tx = original_genesis_coinbase_tx().expect("original genesis tx");

    assert_eq!(
        hex::encode(&tx.payload[140..172]),
        ORIGINAL_GENESIS_BITCOIN_BLOCK_HASH_HEX
    );
}

#[test]
fn hardwired_genesis_header_hash_matches_live_node_hash() {
    assert_eq!(
        hex_of(&header_hash(&hardwired_genesis_header())),
        HARDWIRED_GENESIS_HASH_HEX
    );
}

#[test]
fn verify_genesis_succeeds_for_hardwired_mode() {
    clear_output_capture();
    let tip_time = now_millis().expect("now");
    let hardwired_genesis =
        hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX).expect("hardwired genesis hash");
    let mut store = fake_store_with_tip(hardwired_genesis, hardwired_genesis_header(), tip_time);
    let mut report = base_report();
    let probe_notes = vec!["probe note".to_string()];

    let result = verify_genesis_with_prompt(
        &mut store,
        test_inputs(
            Path::new("/tmp/fake-input"),
            None,
            None,
            &probe_notes,
            false,
            false,
        ),
        &mut report,
        |_| -> anyhow::Result<bool> { panic!("sync warning prompt should not run") },
    )
    .expect("verify genesis");

    assert!(result);
    assert_eq!(report.genesis_mode.as_deref(), Some("hardwired"));
    assert_eq!(report.tips_count, Some(1));
    assert_eq!(report.store_type.as_deref(), Some("Fake test store"));
    assert!(report.checkpoint_utxo_dump_verified);
    assert_eq!(
        report.checkpoint_total_sompi.as_deref(),
        Some("98422254404487171")
    );
    assert_eq!(
        report.checkpoint_total_kas.as_deref(),
        Some("984,222,544.04487171")
    );
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
    let probe_notes = Vec::new();

    let result = verify_genesis_with_prompt(
        &mut store,
        test_inputs(
            Path::new("/tmp/fake-input"),
            None,
            None,
            &probe_notes,
            false,
            false,
        ),
        &mut report,
        |_| -> anyhow::Result<bool> { panic!("sync warning prompt should not run") },
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
    let tempdir = tempfile::TempDir::new().expect("tempdir");
    let db_path = tempdir.path().join("datadir2");
    create_go_leveldb(&db_path, &embedded_checkpoint_headers_for_external_store());

    let tip_time = now_millis().expect("now");
    let hardwired_genesis =
        hash32_from_hex(HARDWIRED_GENESIS_HASH_HEX).expect("hardwired genesis hash");
    let mut current_store =
        fake_store_with_tip(hardwired_genesis, hardwired_genesis_header(), tip_time);
    let mut report = base_report();
    let probe_notes = Vec::new();

    let result = verify_genesis_with_prompt(
        &mut current_store,
        test_inputs(
            Path::new("/tmp/fake-input"),
            Some(db_path.as_path()),
            None,
            &probe_notes,
            false,
            true,
        ),
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
        db_path: "/tmp/fake-db".into(),
        notes: Vec::new(),
    };
    let mut report = base_report();
    let probe_notes = Vec::new();

    let result = verify_genesis_with_prompt(
        &mut store,
        test_inputs(
            Path::new("/tmp/fake-input"),
            None,
            None,
            &probe_notes,
            false,
            false,
        ),
        &mut report,
        |_| -> anyhow::Result<bool> { panic!("sync warning prompt should not run") },
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
        db_path: "/tmp/fake-db".into(),
        notes: Vec::new(),
    };
    let mut report = base_report();
    let probe_notes = Vec::new();

    let result = verify_genesis_with_prompt(
        &mut store,
        test_inputs(
            Path::new("/tmp/fake-input"),
            None,
            None,
            &probe_notes,
            false,
            false,
        ),
        &mut report,
        |_| -> anyhow::Result<bool> { panic!("sync warning prompt should not run") },
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
        db_path: "/tmp/fake-db".into(),
        notes: Vec::new(),
    };
    let mut report = base_report();
    let mut prompt_calls = 0usize;
    let probe_notes = Vec::new();

    let result = verify_genesis_with_prompt(
        &mut store,
        test_inputs(
            Path::new("/tmp/fake-input"),
            None,
            None,
            &probe_notes,
            false,
            false,
        ),
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
    let probe_notes = Vec::new();

    let result = verify_genesis_with_prompt(
        &mut store,
        test_inputs(
            Path::new("/tmp/fake-input"),
            None,
            None,
            &probe_notes,
            false,
            false,
        ),
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
    let probe_notes = Vec::new();

    let result = verify_genesis_with_prompt(
        &mut store,
        test_inputs(
            Path::new("/tmp/fake-input"),
            None,
            None,
            &probe_notes,
            false,
            false,
        ),
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
        db_path: "/tmp/fake-db".into(),
        notes: Vec::new(),
    };
    let mut report = base_report();
    let probe_notes = Vec::new();

    let result = verify_genesis_with_prompt(
        &mut store,
        test_inputs(
            Path::new("/tmp/fake-input"),
            None,
            None,
            &probe_notes,
            false,
            false,
        ),
        &mut report,
        |_| -> anyhow::Result<bool> { panic!("sync warning prompt should not run") },
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
        db_path: "/tmp/fake-db".into(),
        notes: Vec::new(),
    };
    let mut report = base_report();
    let probe_notes = Vec::new();

    let result = verify_genesis_with_prompt(
        &mut store,
        test_inputs(
            Path::new("/tmp/fake-input"),
            None,
            None,
            &probe_notes,
            false,
            false,
        ),
        &mut report,
        |_| -> anyhow::Result<bool> { panic!("sync warning prompt should not run") },
    )
    .expect("verify genesis");

    assert!(!result);
    assert_eq!(
        report.error.as_deref(),
        Some("hash chain verification failed")
    );
}
