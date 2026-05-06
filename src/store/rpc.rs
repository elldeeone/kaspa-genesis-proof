use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
use tokio::sync::mpsc;
use tokio_stream::{iter, wrappers::ReceiverStream};
use tonic::codec::CompressionEncoding;
use tonic::transport::{Channel, Endpoint};

use crate::constants::HARDWIRED_GENESIS_HASH_HEX;
use crate::hashing::{hash32_from_hex, header_hash};
use crate::model::{Hash32, HeaderSource, HeaderStore, ParsedHeader};
use crate::p2pwire::{
    BlockHeader, KaspadMessage, ReadyMessage, RequestPruningPointAndItsAnticoneMessage,
    RequestPruningPointProofMessage, VerackMessage, VersionMessage, kaspad_message as p2p_message,
    p2p_client::P2pClient,
};
use crate::rpcwire::{
    GetBlockDagInfoRequestMessage, GetBlockRequestMessage, KaspadRequest, RpcBlockHeader,
    kaspad_request, kaspad_response, rpc_client::RpcClient,
};

#[derive(Debug)]
pub(crate) struct RpcStore {
    runtime: Runtime,
    client: RpcClient<Channel>,
    rpc_url_path: PathBuf,
    notes: Vec<String>,
    next_request_id: u64,
    proof_headers: Arc<HashMap<Hash32, ParsedHeader>>,
}

#[derive(Debug)]
struct CachedPruningProof {
    headers: Arc<HashMap<Hash32, ParsedHeader>>,
    source: String,
}

static PRUNING_PROOF_CACHE: OnceLock<Mutex<Option<Arc<CachedPruningProof>>>> = OnceLock::new();

impl RpcStore {
    pub(crate) fn connect(rpc_url: &str, p2p_addr: Option<&str>) -> Result<Self> {
        let runtime = RuntimeBuilder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed creating RPC runtime")?;

        let endpoint_url = tonic_endpoint_url(rpc_url)?;
        let channel = runtime
            .block_on(Endpoint::from_shared(endpoint_url)?.connect())
            .with_context(|| format!("failed connecting to RPC endpoint {rpc_url}"))?;

        let (proof_headers, proof_cache_source, proof_fetch_ms) = if let Some(p2p_addr) = p2p_addr {
            load_cached_or_fetch_p2p_pruning_proof(&runtime, p2p_addr)?
        } else {
            (Arc::new(HashMap::new()), None, 0)
        };

        let mut notes = vec![
            "RPC mode: node is used only as a remote header source".to_string(),
            "RPC mode: fetched headers are still hashed and verified locally".to_string(),
        ];
        if let Some(p2p_addr) = p2p_addr {
            if let Some(source) = proof_cache_source {
                notes.push(format!(
                    "P2P mode: reused {} cached pruning-proof headers from verifier cache warmed by {source}",
                    proof_headers.len()
                ));
            } else {
                notes.push(format!(
                    "P2P mode: loaded {} pruning-proof headers from {p2p_addr} in {proof_fetch_ms} ms",
                    proof_headers.len()
                ));
            }
        }

        Ok(Self {
            runtime,
            client: RpcClient::new(channel),
            rpc_url_path: PathBuf::from(rpc_url),
            notes,
            next_request_id: 1,
            proof_headers,
        })
    }

    fn request(&mut self, payload: kaspad_request::Payload) -> Result<kaspad_response::Payload> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let request = KaspadRequest {
            id,
            payload: Some(payload),
        };

        let response =
            self.runtime
                .block_on(async {
                    let mut stream = self
                        .client
                        .message_stream(iter([request]))
                        .await?
                        .into_inner();
                    stream.message().await?.ok_or_else(|| {
                        tonic::Status::unknown("RPC stream closed without a response")
                    })
                })
                .context("RPC request failed")?;

        if response.id != id {
            bail!(
                "RPC response id mismatch: expected {id}, got {}",
                response.id
            );
        }

        response
            .payload
            .ok_or_else(|| anyhow!("RPC response did not contain a payload"))
    }
}

pub(crate) fn warm_p2p_pruning_proof_cache(p2p_addr: &str) -> Result<usize> {
    let runtime = RuntimeBuilder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed creating P2P warmup runtime")?;
    let (headers, _source, _elapsed_ms) =
        load_cached_or_fetch_p2p_pruning_proof(&runtime, p2p_addr)?;
    Ok(headers.len())
}

pub(crate) fn refresh_p2p_pruning_proof_cache(p2p_addr: &str) -> Result<usize> {
    let runtime = RuntimeBuilder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed creating P2P refresh runtime")?;
    let started = Instant::now();
    let headers = runtime
        .block_on(fetch_p2p_pruning_proof_headers(p2p_addr))
        .with_context(|| format!("failed refreshing P2P pruning proof from {p2p_addr}"))?;
    let headers = Arc::new(headers);
    let header_count = headers.len();
    let proof = Arc::new(CachedPruningProof {
        headers,
        source: p2p_addr.to_string(),
    });

    let cache = PRUNING_PROOF_CACHE.get_or_init(|| Mutex::new(None));
    *cache
        .lock()
        .map_err(|_| anyhow!("P2P pruning proof cache lock poisoned"))? = Some(proof);
    eprintln!(
        "Refreshed P2P pruning-proof cache from {p2p_addr}: {header_count} headers in {} ms",
        started.elapsed().as_millis()
    );
    Ok(header_count)
}

pub(crate) fn seed_p2p_pruning_proof_cache(p2p_addr: &str) -> Result<usize> {
    let runtime = RuntimeBuilder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed creating P2P seed runtime")?;
    let started = Instant::now();
    let headers = runtime
        .block_on(fetch_p2p_pruning_proof_headers(p2p_addr))
        .with_context(|| format!("failed seeding P2P pruning proof from {p2p_addr}"))?;
    let headers = Arc::new(headers);
    let header_count = headers.len();
    let proof = Arc::new(CachedPruningProof {
        headers,
        source: p2p_addr.to_string(),
    });

    let cache = PRUNING_PROOF_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache
        .lock()
        .map_err(|_| anyhow!("P2P pruning proof cache lock poisoned"))?;
    if guard.is_none() {
        *guard = Some(proof);
        eprintln!(
            "Seeded P2P pruning-proof cache from {p2p_addr}: {header_count} headers in {} ms",
            started.elapsed().as_millis()
        );
    }
    Ok(header_count)
}

fn load_cached_or_fetch_p2p_pruning_proof(
    runtime: &Runtime,
    p2p_addr: &str,
) -> Result<(Arc<HashMap<Hash32, ParsedHeader>>, Option<String>, u128)> {
    let cache = PRUNING_PROOF_CACHE.get_or_init(|| Mutex::new(None));
    if let Some(proof) = cache
        .lock()
        .map_err(|_| anyhow!("P2P pruning proof cache lock poisoned"))?
        .as_ref()
        .cloned()
    {
        return Ok((Arc::clone(&proof.headers), Some(proof.source.clone()), 0));
    }

    let started = Instant::now();
    let headers = runtime
        .block_on(fetch_p2p_pruning_proof_headers(p2p_addr))
        .with_context(|| format!("failed fetching P2P pruning proof from {p2p_addr}"))?;
    let elapsed_ms = started.elapsed().as_millis();
    let headers = Arc::new(headers);
    let headers_len = headers.len();
    let proof = Arc::new(CachedPruningProof {
        headers: Arc::clone(&headers),
        source: p2p_addr.to_string(),
    });

    let mut guard = cache
        .lock()
        .map_err(|_| anyhow!("P2P pruning proof cache lock poisoned"))?;
    let cached = guard.get_or_insert_with(|| Arc::clone(&proof));
    if cached.source == p2p_addr && cached.headers.len() == headers_len {
        Ok((Arc::clone(&cached.headers), None, elapsed_ms))
    } else {
        Ok((Arc::clone(&cached.headers), Some(cached.source.clone()), 0))
    }
}

impl HeaderSource for RpcStore {
    fn get_raw_header(&mut self, block_hash: &Hash32) -> Result<Option<ParsedHeader>> {
        let hash = hex::encode(block_hash);

        if hash == HARDWIRED_GENESIS_HASH_HEX {
            return Ok(Some(hardwired_genesis_header()?));
        }

        if let Some(header) = self.proof_headers.get(block_hash) {
            return Ok(Some(header.clone()));
        }

        let response = self.request(kaspad_request::Payload::GetBlockRequest(
            GetBlockRequestMessage {
                hash: hash.clone(),
                include_transactions: false,
            },
        ))?;

        let kaspad_response::Payload::GetBlockResponse(response) = response else {
            bail!("RPC getBlock returned an unexpected response payload");
        };

        if let Some(error) = response.error {
            let message = error.message;
            if message.contains("not found")
                || message.contains("NotFound")
                || message.contains("BlockNotFound")
                || message.contains("cannot find header")
            {
                return Ok(None);
            }
            bail!("RPC getBlock failed for {hash}: {message}");
        }

        let Some(block) = response.block else {
            bail!("RPC getBlock response for {hash} did not include a block");
        };
        let Some(header) = block.header else {
            bail!("RPC getBlock response for {hash} did not include a header");
        };

        Ok(Some(parsed_header_from_rpc(header)?))
    }
}

impl HeaderStore for RpcStore {
    fn store_name(&self) -> &'static str {
        "Rust node RPC (gRPC)"
    }

    fn resolved_db_path(&self) -> &Path {
        &self.rpc_url_path
    }

    fn resolution_notes(&self) -> &[String] {
        &self.notes
    }

    fn tips(&mut self) -> Result<(Vec<Hash32>, Hash32)> {
        let response = self.request(kaspad_request::Payload::GetBlockDagInfoRequest(
            GetBlockDagInfoRequestMessage {},
        ))?;

        let kaspad_response::Payload::GetBlockDagInfoResponse(response) = response else {
            bail!("RPC getBlockDagInfo returned an unexpected response payload");
        };

        if let Some(error) = response.error {
            bail!("RPC getBlockDagInfo failed: {}", error.message);
        }

        let tips = response
            .tip_hashes
            .iter()
            .map(|hash| hash32_from_hex(hash))
            .collect::<Result<Vec<_>>>()?;
        let sink = hash32_from_hex(&response.sink)?;

        Ok((tips, sink))
    }
}

fn tonic_endpoint_url(rpc_url: &str) -> Result<String> {
    let Some(rest) = rpc_url.strip_prefix("grpc://") else {
        bail!("RPC URL must start with grpc://");
    };
    Ok(format!("http://{rest}"))
}

fn parsed_header_from_rpc(header: RpcBlockHeader) -> Result<ParsedHeader> {
    let blue_work = hex::decode(header.blue_work.trim_start_matches('0'))
        .context("invalid RPC header blueWork hex")?;

    Ok(ParsedHeader {
        version: header.version as u16,
        parents: header
            .parents
            .into_iter()
            .map(|level| {
                level
                    .parent_hashes
                    .iter()
                    .map(|hash| hash32_from_hex(hash))
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?,
        hash_merkle_root: hash32_from_hex(&header.hash_merkle_root)?,
        accepted_id_merkle_root: hash32_from_hex(&header.accepted_id_merkle_root)?,
        utxo_commitment: hash32_from_hex(&header.utxo_commitment)?,
        time_in_milliseconds: header.timestamp as u64,
        bits: header.bits,
        nonce: header.nonce,
        daa_score: header.daa_score,
        blue_score: header.blue_score,
        blue_work_trimmed_be: blue_work,
        pruning_point: hash32_from_hex(&header.pruning_point)?,
    })
}

async fn fetch_p2p_pruning_proof_headers(p2p_addr: &str) -> Result<HashMap<Hash32, ParsedHeader>> {
    let endpoint = format!("http://{p2p_addr}");
    let channel = Endpoint::from_shared(endpoint)?
        .connect_timeout(Duration::from_secs(8))
        .connect()
        .await
        .with_context(|| format!("failed connecting to P2P endpoint {p2p_addr}"))?;

    let mut client = P2pClient::new(channel)
        .send_compressed(CompressionEncoding::Gzip)
        .accept_compressed(CompressionEncoding::Gzip)
        .max_decoding_message_size(512 * 1024 * 1024);

    let (sender, receiver) = mpsc::channel::<KaspadMessage>(32);
    let mut stream = client
        .message_stream(ReceiverStream::new(receiver))
        .await
        .context("failed opening P2P message stream")?
        .into_inner();

    sender
        .send(KaspadMessage {
            response_id: 0,
            request_id: 0,
            payload: Some(p2p_message::Payload::Version(VersionMessage {
                protocol_version: 9,
                services: 0,
                timestamp: current_unix_millis() as i64,
                address: None,
                id: vec![0; 16],
                user_agent: "genesis-proof-probe".to_string(),
                disable_relay_tx: true,
                subnetwork_id: None,
                network: "kaspa-mainnet".to_string(),
            })),
        })
        .await
        .context("failed sending P2P version")?;

    let mut saw_version = false;
    let mut saw_verack = false;

    while !(saw_version && saw_verack) {
        let Some(message) = stream
            .message()
            .await
            .context("failed reading P2P handshake")?
        else {
            bail!("P2P stream closed during handshake");
        };
        match message.payload {
            Some(p2p_message::Payload::Version(_)) => {
                saw_version = true;
                sender
                    .send(KaspadMessage {
                        response_id: 0,
                        request_id: 0,
                        payload: Some(p2p_message::Payload::Verack(VerackMessage {})),
                    })
                    .await
                    .context("failed sending P2P verack")?;
            }
            Some(p2p_message::Payload::Verack(_)) => saw_verack = true,
            Some(p2p_message::Payload::Reject(reject)) => {
                bail!("P2P handshake rejected: {}", reject.reason)
            }
            _ => {}
        }
    }

    sender
        .send(KaspadMessage {
            response_id: 0,
            request_id: 0,
            payload: Some(p2p_message::Payload::Ready(ReadyMessage {})),
        })
        .await
        .context("failed sending P2P ready")?;

    loop {
        let Some(message) = stream.message().await.context("failed reading P2P ready")? else {
            bail!("P2P stream closed before ready");
        };
        match message.payload {
            Some(p2p_message::Payload::Ready(_)) => break,
            Some(p2p_message::Payload::Reject(reject)) => {
                bail!("P2P ready rejected: {}", reject.reason)
            }
            _ => {}
        }
    }

    sender
        .send(KaspadMessage {
            response_id: 0,
            request_id: 42,
            payload: Some(p2p_message::Payload::RequestPruningPointProof(
                RequestPruningPointProofMessage {},
            )),
        })
        .await
        .context("failed sending P2P pruning proof request")?;

    loop {
        let Some(message) = stream
            .message()
            .await
            .context("failed reading P2P pruning proof response")?
        else {
            bail!("P2P stream closed before pruning proof response");
        };
        match message.payload {
            Some(p2p_message::Payload::PruningPointProof(proof)) => {
                let mut headers = HashMap::new();
                for level in proof.headers {
                    for header in level.headers {
                        let parsed = parsed_header_from_p2p(header)?;
                        headers.insert(header_hash(&parsed), parsed);
                    }
                }
                fetch_p2p_pruning_points(&sender, &mut stream, &mut headers).await?;
                return Ok(headers);
            }
            Some(p2p_message::Payload::Reject(reject)) => {
                bail!("P2P pruning proof rejected: {}", reject.reason)
            }
            _ => {}
        }
    }
}

async fn fetch_p2p_pruning_points(
    sender: &mpsc::Sender<KaspadMessage>,
    stream: &mut tonic::Streaming<KaspadMessage>,
    headers: &mut HashMap<Hash32, ParsedHeader>,
) -> Result<()> {
    sender
        .send(KaspadMessage {
            response_id: 0,
            request_id: 43,
            payload: Some(p2p_message::Payload::RequestPruningPointAndItsAnticone(
                RequestPruningPointAndItsAnticoneMessage {},
            )),
        })
        .await
        .context("failed sending P2P pruning points request")?;

    loop {
        let Some(message) = stream
            .message()
            .await
            .context("failed reading P2P pruning points response")?
        else {
            bail!("P2P stream closed before pruning points response");
        };
        match message.payload {
            Some(p2p_message::Payload::PruningPoints(pruning_points)) => {
                for header in pruning_points.headers {
                    let parsed = parsed_header_from_p2p(header)?;
                    headers.insert(header_hash(&parsed), parsed);
                }
                return Ok(());
            }
            Some(p2p_message::Payload::Reject(reject)) => {
                bail!("P2P pruning points rejected: {}", reject.reason)
            }
            _ => {}
        }
    }
}

fn parsed_header_from_p2p(header: BlockHeader) -> Result<ParsedHeader> {
    let parents = expand_p2p_parents(header.parents)?;
    Ok(ParsedHeader {
        version: header.version as u16,
        parents,
        hash_merkle_root: hash_from_p2p(header.hash_merkle_root, "hashMerkleRoot")?,
        accepted_id_merkle_root: hash_from_p2p(
            header.accepted_id_merkle_root,
            "acceptedIdMerkleRoot",
        )?,
        utxo_commitment: hash_from_p2p(header.utxo_commitment, "utxoCommitment")?,
        time_in_milliseconds: header.timestamp as u64,
        bits: header.bits,
        nonce: header.nonce,
        daa_score: header.daa_score,
        blue_score: header.blue_score,
        blue_work_trimmed_be: header.blue_work,
        pruning_point: hash_from_p2p(header.pruning_point, "pruningPoint")?,
    })
}

fn expand_p2p_parents(levels: Vec<crate::p2pwire::BlockLevelParents>) -> Result<Vec<Vec<Hash32>>> {
    let mut out = Vec::new();
    let mut previous = 0u32;

    for level in levels {
        if level.cumulative_level == 0 {
            out.push(
                level
                    .parent_hashes
                    .into_iter()
                    .map(|hash| hash_from_p2p(Some(hash), "parentHash"))
                    .collect::<Result<Vec<_>>>()?,
            );
            continue;
        }

        if level.cumulative_level <= previous {
            bail!(
                "invalid compressed P2P parents: non-increasing cumulative level {} <= {}",
                level.cumulative_level,
                previous
            );
        }
        let parents = level
            .parent_hashes
            .into_iter()
            .map(|hash| hash_from_p2p(Some(hash), "parentHash"))
            .collect::<Result<Vec<_>>>()?;
        for _ in previous..level.cumulative_level {
            out.push(parents.clone());
        }
        previous = level.cumulative_level;
    }

    Ok(out)
}

fn hash_from_p2p(hash: Option<crate::p2pwire::Hash>, field: &str) -> Result<Hash32> {
    let hash = hash.ok_or_else(|| anyhow!("missing P2P header field {field}"))?;
    hash.bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| anyhow!("invalid P2P hash length for {field}: {}", bytes.len()))
}

fn current_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before unix epoch")
        .as_millis() as u64
}

fn hardwired_genesis_header() -> Result<ParsedHeader> {
    Ok(ParsedHeader {
        version: 0,
        parents: Vec::new(),
        hash_merkle_root: hash32_from_hex(
            "8ec898568c6801d13df4ee6e2a1b54b7e6236f671f20954f05306410518eeb32",
        )?,
        accepted_id_merkle_root: [0u8; 32],
        utxo_commitment: hash32_from_hex(
            "710f27df423e63aa6cdb72b89ea5a06cffa399d66f167704455b5af59def8e20",
        )?,
        time_in_milliseconds: 1_637_609_671_037,
        bits: 486_722_099,
        nonce: 211_244,
        daa_score: 1_312_860,
        blue_score: 0,
        blue_work_trimmed_be: Vec::new(),
        pruning_point: [0u8; 32],
    })
}
