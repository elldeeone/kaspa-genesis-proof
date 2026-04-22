use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use kaspa_muhash::MuHash;
use std::fs::{self, File};
use std::io::{BufReader, Cursor, Read};
use std::path::Path;
use std::sync::OnceLock;

use crate::Hash32;

pub(crate) const CHECKPOINT_UTXO_DUMP_SOURCE_LABEL: &str =
    "embedded kaspad v0.11.5-2 resources/utxos.gz";
pub(crate) const CHECKPOINT_UTXO_DUMP_SOURCE_URL: &str = "https://raw.githubusercontent.com/kaspanet/kaspad/v0.11.5-2/domain/consensus/processes/blockprocessor/resources/utxos.gz";
pub(crate) const CHECKPOINT_TOTAL_SOMPI_EXPECTED: u64 = 98_422_254_404_487_171;
pub(crate) const SOMPI_PER_KAS: u64 = 100_000_000;
pub(crate) const REFERENCE_SCHEDULE_KAS_PER_DAA: u64 = 500;

const EMBEDDED_CHECKPOINT_UTXO_DUMP_GZ: &[u8] =
    include_bytes!("../resources/kaspad-v0.11.5-2-utxos.gz");
static EMBEDDED_CHECKPOINT_SCAN: OnceLock<Result<CheckpointUtxoScan, String>> = OnceLock::new();

#[derive(Clone, Debug)]
pub(crate) struct CheckpointUtxoScan {
    pub(crate) commitment: Hash32,
    pub(crate) compressed_size_bytes: u64,
    pub(crate) record_count: u64,
    pub(crate) total_sompi: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct VerifiedCheckpointUtxoDump {
    pub(crate) scan: CheckpointUtxoScan,
    pub(crate) source_label: String,
    pub(crate) source_url: &'static str,
    pub(crate) used_operator_supplied_file: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct DecodedCheckpointUtxo {
    transaction_id: Hash32,
    outpoint_index: u32,
    block_daa_score: u64,
    amount_sompi: u64,
    is_coinbase: bool,
    script_version: u16,
    script_pub_key: Vec<u8>,
}

pub(crate) fn scan_embedded_checkpoint_utxo_dump() -> Result<CheckpointUtxoScan> {
    let cached = EMBEDDED_CHECKPOINT_SCAN.get_or_init(|| {
        scan_checkpoint_utxo_dump_bytes(EMBEDDED_CHECKPOINT_UTXO_DUMP_GZ)
            .and_then(|scan| {
                if scan.total_sompi != CHECKPOINT_TOTAL_SOMPI_EXPECTED {
                    bail!(
                        "embedded checkpoint total mismatch: expected {CHECKPOINT_TOTAL_SOMPI_EXPECTED}, got {}",
                        scan.total_sompi
                    );
                }
                Ok(scan)
            })
            .map_err(|err| format!("{err:#}"))
    });

    match cached {
        Ok(scan) => Ok(scan.clone()),
        Err(err) => Err(anyhow!(err.clone())),
    }
}

pub(crate) fn verify_checkpoint_utxo_dump(
    expected_commitment: Hash32,
    operator_supplied_path: Option<&Path>,
) -> Result<VerifiedCheckpointUtxoDump> {
    if let Some(path) = operator_supplied_path {
        let scan = scan_checkpoint_utxo_dump_file(path)?;
        verify_checkpoint_scan(&scan, expected_commitment)?;
        return Ok(VerifiedCheckpointUtxoDump {
            scan,
            source_label: format!("operator-supplied checkpoint utxos.gz: {}", path.display()),
            source_url: CHECKPOINT_UTXO_DUMP_SOURCE_URL,
            used_operator_supplied_file: true,
        });
    }

    let scan = scan_embedded_checkpoint_utxo_dump()?;
    verify_checkpoint_scan(&scan, expected_commitment)?;
    Ok(VerifiedCheckpointUtxoDump {
        scan,
        source_label: CHECKPOINT_UTXO_DUMP_SOURCE_LABEL.to_string(),
        source_url: CHECKPOINT_UTXO_DUMP_SOURCE_URL,
        used_operator_supplied_file: false,
    })
}

pub(crate) fn format_sompi_amount(value: u64) -> String {
    format_u64_grouped(value)
}

pub(crate) fn format_kas_amount_from_sompi(value: u64) -> String {
    let whole = value / SOMPI_PER_KAS;
    let fractional = value % SOMPI_PER_KAS;
    format!("{}.{fractional:08}", format_u64_grouped(whole))
}

pub(crate) fn reference_baseline_sompi(daa_score: u64) -> Result<u64> {
    daa_score
        .checked_mul(REFERENCE_SCHEDULE_KAS_PER_DAA)
        .and_then(|value| value.checked_mul(SOMPI_PER_KAS))
        .context("reference schedule baseline overflowed u64")
}

fn scan_checkpoint_utxo_dump_bytes(gzip_bytes: &[u8]) -> Result<CheckpointUtxoScan> {
    let decoder = GzDecoder::new(Cursor::new(gzip_bytes));
    let mut reader = BufReader::with_capacity(256 * 1024, decoder);
    scan_checkpoint_utxo_dump_reader(&mut reader, gzip_bytes.len() as u64)
}

fn scan_checkpoint_utxo_dump_file(path: &Path) -> Result<CheckpointUtxoScan> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed reading checkpoint dump metadata {}", path.display()))?;
    let file = File::open(path)
        .with_context(|| format!("failed opening checkpoint dump {}", path.display()))?;
    let decoder = GzDecoder::new(file);
    let mut reader = BufReader::with_capacity(256 * 1024, decoder);
    scan_checkpoint_utxo_dump_reader(&mut reader, metadata.len())
}

fn scan_checkpoint_utxo_dump_reader<R: Read>(
    reader: &mut BufReader<R>,
    compressed_size_bytes: u64,
) -> Result<CheckpointUtxoScan> {
    let mut muhash = MuHash::new();
    let mut total_sompi = 0u64;
    let mut record_count = 0u64;

    loop {
        let mut framed_size = [0u8; 1];
        let bytes_read = reader
            .read(&mut framed_size)
            .context("failed reading checkpoint record length")?;
        if bytes_read == 0 {
            break;
        }

        let record_len = usize::from(framed_size[0]);
        let mut record = vec![0u8; record_len];
        reader.read_exact(&mut record).with_context(|| {
            format!(
                "failed reading checkpoint record #{} payload ({record_len} bytes)",
                record_count + 1
            )
        })?;

        muhash.add_element(&record);
        let decoded = deserialize_checkpoint_utxo(&record).with_context(|| {
            format!(
                "failed deserializing checkpoint record #{}",
                record_count + 1
            )
        })?;
        total_sompi = total_sompi
            .checked_add(decoded.amount_sompi)
            .context("checkpoint total overflowed u64")?;
        record_count += 1;
    }

    let commitment = muhash.finalize().as_bytes();
    Ok(CheckpointUtxoScan {
        commitment,
        compressed_size_bytes,
        record_count,
        total_sompi,
    })
}

fn verify_checkpoint_scan(scan: &CheckpointUtxoScan, expected_commitment: Hash32) -> Result<()> {
    if scan.commitment != expected_commitment {
        bail!(
            "checkpoint dump MuHash mismatch: expected {}, got {}",
            hex::encode(expected_commitment),
            hex::encode(scan.commitment)
        );
    }
    if scan.total_sompi != CHECKPOINT_TOTAL_SOMPI_EXPECTED {
        bail!(
            "checkpoint total mismatch: expected {CHECKPOINT_TOTAL_SOMPI_EXPECTED}, got {}",
            scan.total_sompi
        );
    }
    Ok(())
}

fn deserialize_checkpoint_utxo(record: &[u8]) -> Result<DecodedCheckpointUtxo> {
    let mut cursor = Cursor::new(record);
    let mut transaction_id = [0u8; 32];
    cursor
        .read_exact(&mut transaction_id)
        .context("failed reading outpoint.transactionID")?;

    let outpoint_index = read_u32_le(&mut cursor, "outpoint.index")?;
    let block_daa_score = read_u64_le(&mut cursor, "entry.blockDAAScore")?;
    let amount_sompi = read_u64_le(&mut cursor, "entry.amount")?;
    let is_coinbase = read_bool(&mut cursor, "entry.isCoinbase")?;
    let script_version = read_u16_le(&mut cursor, "script.version")?;
    let script_pub_key_len = read_u64_le(&mut cursor, "scriptPubKeyLen")?;
    let script_len =
        usize::try_from(script_pub_key_len).context("scriptPubKeyLen does not fit in usize")?;
    let mut script_pub_key = vec![0u8; script_len];
    cursor
        .read_exact(&mut script_pub_key)
        .context("failed reading script bytes")?;

    if cursor.position() != u64::try_from(record.len()).expect("record len fits in u64") {
        bail!(
            "record contains {} trailing bytes",
            record.len()
                - usize::try_from(cursor.position()).expect("cursor position fits in usize")
        );
    }

    Ok(DecodedCheckpointUtxo {
        transaction_id,
        outpoint_index,
        block_daa_score,
        amount_sompi,
        is_coinbase,
        script_version,
        script_pub_key,
    })
}

fn read_u16_le<R: Read>(reader: &mut R, field: &str) -> Result<u16> {
    let mut bytes = [0u8; 2];
    reader
        .read_exact(&mut bytes)
        .with_context(|| format!("failed reading {field}"))?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32_le<R: Read>(reader: &mut R, field: &str) -> Result<u32> {
    let mut bytes = [0u8; 4];
    reader
        .read_exact(&mut bytes)
        .with_context(|| format!("failed reading {field}"))?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64_le<R: Read>(reader: &mut R, field: &str) -> Result<u64> {
    let mut bytes = [0u8; 8];
    reader
        .read_exact(&mut bytes)
        .with_context(|| format!("failed reading {field}"))?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_bool<R: Read>(reader: &mut R, field: &str) -> Result<bool> {
    let mut byte = [0u8; 1];
    reader
        .read_exact(&mut byte)
        .with_context(|| format!("failed reading {field}"))?;
    match byte[0] {
        0x00 => Ok(false),
        0x01 => Ok(true),
        value => {
            bail!("malformed {field}: expected canonical bool byte 0x00 or 0x01, got 0x{value:02x}")
        }
    }
}

fn format_u64_grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    fn serialize_test_utxo(
        transaction_id_fill: u8,
        outpoint_index: u32,
        block_daa_score: u64,
        amount_sompi: u64,
        is_coinbase: bool,
        script_version: u16,
        script_pub_key: &[u8],
    ) -> Vec<u8> {
        let mut record = vec![transaction_id_fill; 32];
        record.extend_from_slice(&outpoint_index.to_le_bytes());
        record.extend_from_slice(&block_daa_score.to_le_bytes());
        record.extend_from_slice(&amount_sompi.to_le_bytes());
        record.push(if is_coinbase { 0x01 } else { 0x00 });
        record.extend_from_slice(&script_version.to_le_bytes());
        record.extend_from_slice(&(script_pub_key.len() as u64).to_le_bytes());
        record.extend_from_slice(script_pub_key);
        record
    }

    #[test]
    fn deserialize_checkpoint_utxo_matches_go_layout() {
        let record = serialize_test_utxo(0x11, 7, 123, 456, true, 9, &[0xaa, 0xbb, 0xcc]);

        let decoded = deserialize_checkpoint_utxo(&record).expect("decode checkpoint utxo");

        assert_eq!(decoded.transaction_id, [0x11; 32]);
        assert_eq!(decoded.outpoint_index, 7);
        assert_eq!(decoded.block_daa_score, 123);
        assert_eq!(decoded.amount_sompi, 456);
        assert!(decoded.is_coinbase);
        assert_eq!(decoded.script_version, 9);
        assert_eq!(decoded.script_pub_key, vec![0xaa, 0xbb, 0xcc]);
    }

    #[test]
    fn scan_checkpoint_utxo_dump_bytes_hashes_raw_records_and_sums_amounts() {
        let record_a = serialize_test_utxo(0x21, 1, 10, 111, false, 0, &[0x51]);
        let record_b = serialize_test_utxo(0x22, 2, 20, 222, true, 1, &[0x52, 0x53]);

        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&[u8::try_from(record_a.len()).expect("record a len fits in u8")])
            .expect("write record a len");
        gz.write_all(&record_a).expect("write record a");
        gz.write_all(&[u8::try_from(record_b.len()).expect("record b len fits in u8")])
            .expect("write record b len");
        gz.write_all(&record_b).expect("write record b");
        let dump = gz.finish().expect("finish gzip");

        let mut expected_muhash = MuHash::new();
        expected_muhash.add_element(&record_a);
        expected_muhash.add_element(&record_b);

        let scan = scan_checkpoint_utxo_dump_bytes(&dump).expect("scan dump");

        assert_eq!(scan.record_count, 2);
        assert_eq!(scan.total_sompi, 333);
        assert_eq!(scan.commitment, expected_muhash.finalize().as_bytes());
    }

    #[test]
    fn scan_checkpoint_utxo_dump_file_reads_gzip_from_disk() {
        let record = serialize_test_utxo(0x44, 9, 30, 777, false, 0, &[0x51, 0x52]);
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&[u8::try_from(record.len()).expect("record len fits in u8")])
            .expect("write record len");
        gz.write_all(&record).expect("write record");
        let dump = gz.finish().expect("finish gzip");

        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("utxos.gz");
        fs::write(&path, dump).expect("write dump file");

        let scan = scan_checkpoint_utxo_dump_file(&path).expect("scan file");

        assert_eq!(scan.record_count, 1);
        assert_eq!(scan.total_sompi, 777);
        assert!(scan.compressed_size_bytes > 0);
    }

    #[test]
    fn format_kas_amount_from_sompi_keeps_eight_decimals() {
        assert_eq!(
            format_kas_amount_from_sompi(CHECKPOINT_TOTAL_SOMPI_EXPECTED),
            "984,222,544.04487171"
        );
    }
}
