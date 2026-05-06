use std::collections::BTreeSet;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use genesis_proof::{
    RemoteProofOptions, VerificationReport, refresh_remote_pruning_proof_cache_from_p2p,
    run_remote_proof, seed_remote_pruning_proof_cache_from_p2p, warm_up_remote_proof_caches,
};
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

const DEFAULT_P2P_PORT: u16 = 16111;
const MAINNET_DNS_SEEDERS: &[&str] = &[
    "mainnet-dnsseed-1.kaspanet.org",
    "mainnet-dnsseed-2.kaspanet.org",
    "seeder1.kaspad.net",
    "seeder2.kaspad.net",
    "seeder3.kaspad.net",
    "seeder4.kaspad.net",
    "kaspadns.kaspacalc.net",
    "n-mainnet.kaspa.ws",
    "dnsseeder-kaspa-mainnet.x-con.at",
];

#[derive(Clone)]
struct AppState {
    default_rpc_port: u16,
    proof_source_addr: String,
    proof_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Deserialize)]
struct VerifyRequest {
    host: String,
    rpc_port: Option<u16>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let proof_source_addr = warm_startup_caches().await?;
    spawn_pruning_proof_refresh_loop(proof_source_addr.clone());

    let state = AppState {
        default_rpc_port: 16110,
        proof_source_addr,
        proof_lock: Arc::new(Mutex::new(())),
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/api/verify", post(verify))
        .with_state(state);

    let addr: SocketAddr = std::env::var("KASPA_PROOF_WEB_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_string())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("Kaspa genesis proof web prototype listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn warm_startup_caches() -> anyhow::Result<String> {
    let require_source_warmup = std::env::var("KASPA_PROOF_REQUIRE_SOURCE_WARMUP")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false);

    let started = Instant::now();
    let proof_source_candidates = proof_source_candidates().await?;
    let fallback_proof_source = proof_source_candidates
        .first()
        .cloned()
        .unwrap_or_else(|| format!("127.0.0.1:{DEFAULT_P2P_PORT}"));
    println!(
        "Warming checkpoint proof and pruning-proof cache from {} candidate proof source(s)...",
        proof_source_candidates.len()
    );
    tokio::task::spawn_blocking(warm_up_remote_proof_caches).await??;
    println!("Embedded checkpoint proof cache is ready.");

    let parallelism = proof_source_parallelism();
    match warm_pruning_proof_from_candidates(proof_source_candidates, parallelism).await {
        Ok((proof_source_addr, header_count)) => {
            println!(
                "Startup proof caches are ready from {proof_source_addr}: {header_count} pruning-proof headers in {} ms.",
                started.elapsed().as_millis()
            );
            Ok(proof_source_addr)
        }
        Err(err) if require_source_warmup => Err(err),
        Err(err) => {
            eprintln!(
                "Backend proof-source warmup failed for all candidates; serving with checkpoint cache only and retrying first candidate during verification/refresh: {err:#}"
            );
            Ok(fallback_proof_source)
        }
    }
}

fn proof_source_parallelism() -> usize {
    std::env::var("KASPA_PROOF_SOURCE_PARALLELISM")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(4)
}

async fn warm_pruning_proof_from_candidates(
    candidates: Vec<String>,
    parallelism: usize,
) -> anyhow::Result<(String, usize)> {
    let mut candidates = candidates.into_iter();
    let mut in_flight = JoinSet::new();
    let mut errors = Vec::new();

    for _ in 0..parallelism {
        if !spawn_next_proof_source(&mut candidates, &mut in_flight) {
            break;
        }
    }

    while let Some(result) = in_flight.join_next().await {
        match result? {
            (proof_source_addr, Ok(header_count)) => {
                in_flight.abort_all();
                return Ok((proof_source_addr, header_count));
            }
            (proof_source_addr, Err(err)) => {
                eprintln!("Proof source {proof_source_addr} failed: {err:#}");
                errors.push(format!("{proof_source_addr}: {err:#}"));
                spawn_next_proof_source(&mut candidates, &mut in_flight);
            }
        }
    }

    anyhow::bail!(
        "all backend proof-source candidates failed: {}",
        errors.join("; ")
    )
}

fn spawn_next_proof_source(
    candidates: &mut impl Iterator<Item = String>,
    in_flight: &mut JoinSet<(String, anyhow::Result<usize>)>,
) -> bool {
    let Some(proof_source_addr) = candidates.next() else {
        return false;
    };

    println!("Trying backend proof source node: {proof_source_addr}");
    in_flight.spawn_blocking(move || {
        let result = seed_remote_pruning_proof_cache_from_p2p(&proof_source_addr);
        (proof_source_addr, result)
    });
    true
}

fn configured_proof_source_addr() -> Option<String> {
    std::env::var("KASPA_PROOF_SOURCE_ADDR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn proof_source_candidates() -> anyhow::Result<Vec<String>> {
    if let Some(addr) = configured_proof_source_addr() {
        println!("Using configured proof source node: {addr}");
        return Ok(vec![addr]);
    }

    println!("Resolving backend proof source candidates from Kaspa mainnet DNS seeders...");
    let candidates = tokio::task::spawn_blocking(resolve_dns_seeders).await??;
    if candidates.is_empty() {
        anyhow::bail!("DNS seeders returned no proof-source candidates");
    }
    Ok(candidates)
}

fn resolve_dns_seeders() -> anyhow::Result<Vec<String>> {
    let mut candidates = BTreeSet::new();
    let mut errors = Vec::new();

    for seeder in MAINNET_DNS_SEEDERS {
        match (*seeder, DEFAULT_P2P_PORT).to_socket_addrs() {
            Ok(addrs) => {
                for addr in addrs {
                    candidates.insert(addr.to_string());
                }
            }
            Err(err) => errors.push(format!("{seeder}: {err}")),
        }
    }

    if candidates.is_empty() {
        anyhow::bail!("all DNS seeders failed: {}", errors.join("; "));
    }

    Ok(candidates.into_iter().take(32).collect())
}

fn spawn_pruning_proof_refresh_loop(proof_source_addr: String) {
    let refresh_interval_seconds = std::env::var("KASPA_PROOF_SOURCE_REFRESH_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(30 * 60);

    tokio::spawn(async move {
        let interval = Duration::from_secs(refresh_interval_seconds);
        loop {
            tokio::time::sleep(interval).await;
            let refresh_addr = proof_source_addr.clone();
            let started = Instant::now();
            match tokio::task::spawn_blocking(move || {
                refresh_remote_pruning_proof_cache_from_p2p(&refresh_addr)
            })
            .await
            {
                Ok(Ok(header_count)) => {
                    println!(
                        "Proof-source pruning-proof cache refresh completed: {header_count} headers in {} ms.",
                        started.elapsed().as_millis()
                    );
                }
                Ok(Err(err)) => {
                    eprintln!(
                        "Proof-source pruning-proof cache refresh failed; keeping existing cache: {err:#}"
                    );
                }
                Err(err) => {
                    eprintln!(
                        "Proof-source pruning-proof cache refresh worker failed; keeping existing cache: {err}"
                    );
                }
            }
        }
    });
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn verify(
    State(state): State<AppState>,
    Json(request): Json<VerifyRequest>,
) -> impl IntoResponse {
    let host = request.host.trim();
    if host.is_empty() || host.len() > 255 || host.contains("://") || host.contains('/') {
        return (
            StatusCode::BAD_REQUEST,
            Json(VerifyResponse::error("enter a bare host or IP address")),
        );
    }

    let rpc_port = request.rpc_port.unwrap_or(state.default_rpc_port);
    let rpc_url = format!("grpc://{host}:{rpc_port}");
    let p2p_addr = state.proof_source_addr.clone();
    let proof_lock = Arc::clone(&state.proof_lock);

    let _proof_guard = proof_lock.lock().await;
    let report = tokio::task::spawn_blocking(move || {
        run_remote_proof(RemoteProofOptions {
            rpc_url,
            p2p_addr: Some(p2p_addr),
            pre_checkpoint_datadir: None,
            checkpoint_utxos_gz: None,
        })
    })
    .await
    .unwrap_or_else(|err| VerificationReport {
        success: false,
        error: Some(format!("proof worker failed: {err}")),
        ..VerificationReport::default()
    });

    (StatusCode::OK, Json(VerifyResponse { report }))
}

#[derive(serde::Serialize)]
struct VerifyResponse {
    report: VerificationReport,
}

impl VerifyResponse {
    fn error(message: &str) -> Self {
        Self {
            report: VerificationReport {
                success: false,
                error: Some(message.to_string()),
                ..VerificationReport::default()
            },
        }
    }
}

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Kaspa Genesis Proof</title>
  <style>
    :root {
      color-scheme: light;
      --ink: #161411;
      --muted: #625d54;
      --paper: #f7f3ea;
      --line: #282018;
      --field: #fffaf0;
      --ok: #12795a;
      --bad: #aa3030;
      --accent: #1e8f86;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      color: var(--ink);
      background:
        linear-gradient(90deg, rgba(22,20,17,.05) 1px, transparent 1px),
        linear-gradient(rgba(22,20,17,.04) 1px, transparent 1px),
        var(--paper);
      background-size: 28px 28px;
      font-family: ui-serif, Georgia, Cambria, "Times New Roman", serif;
    }
    main {
      width: min(1120px, calc(100vw - 32px));
      margin: 0 auto;
      padding: 40px 0;
    }
    header {
      display: grid;
      grid-template-columns: minmax(0, 1.15fr) minmax(280px, .85fr);
      gap: 28px;
      align-items: end;
      min-height: 34vh;
      border-bottom: 2px solid var(--line);
      padding-bottom: 28px;
    }
    h1 {
      margin: 0;
      max-width: 820px;
      font-size: clamp(48px, 9vw, 116px);
      line-height: .86;
      letter-spacing: 0;
      font-weight: 900;
    }
    .lede {
      margin: 18px 0 0;
      max-width: 680px;
      color: var(--muted);
      font: 18px/1.45 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    }
    .status-strip {
      border: 2px solid var(--line);
      background: var(--field);
      padding: 18px;
      box-shadow: 8px 8px 0 var(--line);
      font: 14px/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    }
    .status-strip b { display: block; margin-bottom: 8px; }
    form {
      display: grid;
      grid-template-columns: minmax(240px, 1fr) 120px auto;
      gap: 12px;
      align-items: end;
      margin: 32px 0 24px;
    }
    label {
      display: grid;
      gap: 7px;
      color: var(--muted);
      font: 13px/1 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
      text-transform: uppercase;
    }
    input, button {
      min-height: 48px;
      border: 2px solid var(--line);
      border-radius: 0;
      color: var(--ink);
      background: var(--field);
      font: 18px/1 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
      padding: 0 14px;
    }
    button {
      cursor: pointer;
      background: var(--accent);
      color: white;
      font-weight: 800;
      box-shadow: 5px 5px 0 var(--line);
    }
    button:disabled {
      cursor: wait;
      filter: grayscale(.8);
      opacity: .72;
    }
    .actions {
      display: flex;
      flex-wrap: wrap;
      gap: 10px;
      margin: -8px 0 24px;
      justify-content: flex-end;
    }
    .actions button {
      min-height: 38px;
      padding: 0 12px;
      font-size: 13px;
      box-shadow: 3px 3px 0 var(--line);
      background: var(--field);
      color: var(--ink);
    }
    .result {
      display: grid;
      grid-template-columns: 280px minmax(0, 1fr);
      gap: 18px;
      align-items: start;
    }
    .summary, pre {
      border: 2px solid var(--line);
      background: var(--field);
    }
    .summary {
      min-height: 160px;
      padding: 18px;
      font: 15px/1.45 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    }
    .badge {
      display: inline-block;
      margin-bottom: 14px;
      padding: 6px 9px;
      border: 2px solid currentColor;
      font-weight: 900;
    }
    .badge.ok { color: var(--ok); }
    .badge.bad { color: var(--bad); }
    pre {
      min-height: 360px;
      max-height: 60vh;
      overflow: auto;
      margin: 0;
      padding: 18px;
      white-space: pre-wrap;
      word-break: break-word;
      font: 13px/1.55 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    }
    @media (max-width: 800px) {
      header, form, .result { grid-template-columns: 1fr; }
      h1 { font-size: clamp(44px, 18vw, 78px); }
    }
  </style>
</head>
<body>
  <main>
    <header>
      <div>
        <h1>Kaspa Genesis Proof</h1>
        <p class="lede">Connect to your node briefly for live chain state, then verify against this backend's warmed genesis proof cache.</p>
      </div>
      <aside class="status-strip">
        <b>Required forward</b>
        RPC: 16110<br>
        The backend supplies the pruning-proof cache.
      </aside>
    </header>

    <form id="verify-form">
      <label>WAN IP or host
        <input id="host" name="host" value="10.0.4.30" autocomplete="off" required>
      </label>
      <label>RPC
        <input id="rpc-port" name="rpc_port" type="number" min="1" max="65535" value="16110">
      </label>
      <button id="submit" type="submit">Verify</button>
    </form>

    <div class="actions">
      <button id="copy-report" type="button" disabled>Copy JSON</button>
      <button id="download-report" type="button" disabled>Save JSON</button>
    </div>

    <section class="result">
      <div class="summary" id="summary">Waiting for a node.</div>
      <pre id="report">{}</pre>
    </section>
  </main>

  <script>
    const form = document.querySelector("#verify-form");
    const submit = document.querySelector("#submit");
    const summary = document.querySelector("#summary");
    const reportBox = document.querySelector("#report");
    const copyReport = document.querySelector("#copy-report");
    const downloadReport = document.querySelector("#download-report");
    let latestReport = null;

    function setSummary(report) {
      const ok = report && report.success;
      const lines = [
        `<span class="badge ${ok ? "ok" : "bad"}">${ok ? "PASSED" : "FAILED"}</span>`,
        `tips: ${report.tips_count ?? "n/a"}`,
        `tip: ${report.chain_tip_used ?? "n/a"}`,
        `checkpoint total: ${report.checkpoint_total_kas ?? "n/a"}`,
        `error: ${report.error ?? "none"}`
      ];
      summary.innerHTML = lines.join("<br>");
    }

    function setReport(report) {
      latestReport = report;
      reportBox.textContent = JSON.stringify(report, null, 2);
      copyReport.disabled = !report;
      downloadReport.disabled = !report;
    }

    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      submit.disabled = true;
      copyReport.disabled = true;
      downloadReport.disabled = true;
      summary.textContent = "Connecting and verifying. This can take a few minutes.";
      reportBox.textContent = "{}";
      const fields = new FormData(form);
      const body = {
        host: fields.get("host"),
        rpc_port: Number(fields.get("rpc_port") || 16110)
      };
      try {
        const response = await fetch("/api/verify", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body)
        });
        const payload = await response.json();
        setSummary(payload.report);
        setReport(payload.report);
      } catch (error) {
        const payload = { success: false, error: String(error) };
        setSummary(payload);
        setReport(payload);
      } finally {
        submit.disabled = false;
      }
    });

    copyReport.addEventListener("click", async () => {
      if (!latestReport) return;
      await navigator.clipboard.writeText(JSON.stringify(latestReport, null, 2));
      copyReport.textContent = "Copied";
      setTimeout(() => { copyReport.textContent = "Copy JSON"; }, 1200);
    });

    downloadReport.addEventListener("click", () => {
      if (!latestReport) return;
      const blob = new Blob([JSON.stringify(latestReport, null, 2)], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = `kaspa-genesis-proof-${latestReport.chain_tip_used || "report"}.json`;
      link.click();
      URL.revokeObjectURL(url);
    });
  </script>
</body>
</html>
"##;
