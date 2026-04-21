use anyhow::{Context, Result};
use std::fs;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    BLUE, BOLD, Cli, CliNodeType, END, GREEN, OUTPUT_CAPTURE, RED, VerificationReport, YELLOW,
};

pub(crate) fn print_header(text: &str) {
    let sep = "=".repeat(60);
    println!("\n{BOLD}{BLUE}{sep}{END}");
    println!("{BOLD}{BLUE}{text}{END}");
    println!("{BOLD}{BLUE}{sep}{END}");
    capture_output_line("");
    capture_output_line(&sep);
    capture_output_line(text);
    capture_output_line(&sep);
}

pub(crate) fn print_success(text: &str) {
    println!("{GREEN}✓ {text}{END}");
    capture_output_line(&format!("✓ {text}"));
}

pub(crate) fn print_error(text: &str) {
    println!("{RED}✗ {text}{END}");
    capture_output_line(&format!("✗ {text}"));
}

pub(crate) fn print_info(text: &str) {
    println!("{GREEN}→ {text}{END}");
    capture_output_line(&format!("→ {text}"));
}

pub(crate) fn print_warning(text: &str) {
    println!("{YELLOW}! {text}{END}");
    capture_output_line(&format!("! {text}"));
}

pub(crate) fn print_plain(text: &str) {
    println!("{text}");
    capture_output_line(text);
}

pub(crate) fn print_prompt(text: &str) {
    println!("{YELLOW}? {text}{END}");
    capture_output_line(&format!("? {text}"));
}

fn output_capture() -> &'static Mutex<Vec<String>> {
    OUTPUT_CAPTURE.get_or_init(|| Mutex::new(Vec::new()))
}

pub(crate) fn clear_output_capture() {
    if let Ok(mut lines) = output_capture().lock() {
        lines.clear();
    }
}

pub(crate) fn capture_output_line(line: &str) {
    if let Ok(mut lines) = output_capture().lock() {
        lines.push(line.to_string());
    }
}

pub(crate) fn output_capture_snapshot() -> Vec<String> {
    output_capture()
        .lock()
        .map(|lines| lines.clone())
        .unwrap_or_default()
}

pub(crate) fn now_millis() -> Result<u64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock appears to be before Unix epoch")?;
    u64::try_from(now.as_millis()).context("current time millis does not fit in u64")
}

pub(crate) fn format_duration_ms(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let days = total_seconds / 86_400;
    let hours = (total_seconds % 86_400) / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;

    if days > 0 {
        format!("{days}d {hours}h {minutes}m {seconds}s")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn cli_node_type_label(node_type: CliNodeType) -> &'static str {
    match node_type {
        CliNodeType::Auto => "auto",
        CliNodeType::Rust => "rust",
        CliNodeType::Go => "go",
    }
}

pub(crate) fn build_initial_report(cli: &Cli) -> VerificationReport {
    VerificationReport {
        generated_at_unix_ms: now_millis().unwrap_or(0),
        requested_node_type: cli_node_type_label(cli.node_type).to_string(),
        provided_datadir: cli.datadir.as_ref().map(|p| p.display().to_string()),
        ..VerificationReport::default()
    }
}

pub(crate) fn write_json_report(path: &Path, report: &VerificationReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed creating report parent dir {}", parent.display())
            })?;
        }
    }

    let json = serde_json::to_string_pretty(report).context("failed serializing JSON report")?;
    fs::write(path, json)
        .with_context(|| format!("failed writing JSON report file {}", path.display()))
}

pub(crate) fn prompt_export_json_decision(no_input: bool) -> Result<bool> {
    if no_input || !io::stdin().is_terminal() {
        return Ok(false);
    }

    println!("{YELLOW}? Do you want to export this verification to JSON? [y/N]{END}");
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed reading export prompt response from stdin")?;

    let response = input.trim().to_ascii_lowercase();
    Ok(matches!(response.as_str(), "y" | "yes"))
}

pub(crate) fn prompt_continue_on_sync_warning(no_input: bool) -> Result<bool> {
    if no_input {
        print_warning("Sync advisory prompt skipped due to --no-input; continuing automatically.");
        return Ok(true);
    }

    if !io::stdin().is_terminal() {
        print_warning(
            "Sync advisory prompt skipped because stdin is non-interactive; continuing automatically.",
        );
        return Ok(true);
    }

    print_prompt("Continue verification anyway against your latest local synced tip? [y/N]");
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed reading prompt response from stdin")?;

    let response = input.trim().to_ascii_lowercase();
    Ok(matches!(response.as_str(), "y" | "yes"))
}
