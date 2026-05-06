use anyhow::{Context, Result};
use std::cell::RefCell;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli::{Cli, CliNodeType};
use crate::constants::{BLUE, BOLD, END, GREEN, RED, YELLOW};
use crate::model::VerificationReport;

static OUTPUT_CAPTURE: std::sync::OnceLock<Mutex<Vec<String>>> = std::sync::OnceLock::new();

thread_local! {
    static SCOPED_OUTPUT_CAPTURE: RefCell<Option<Arc<Mutex<Vec<String>>>>> = const { RefCell::new(None) };
}

pub(crate) struct ScopedOutputCaptureGuard;

impl Drop for ScopedOutputCaptureGuard {
    fn drop(&mut self) {
        SCOPED_OUTPUT_CAPTURE.with(|capture| {
            *capture.borrow_mut() = None;
        });
    }
}

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
    let captured = SCOPED_OUTPUT_CAPTURE.with(|capture| {
        if let Some(lines) = capture.borrow().as_ref() {
            if let Ok(mut lines) = lines.lock() {
                lines.push(line.to_string());
            }
            true
        } else {
            false
        }
    });
    if captured {
        return;
    }

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

pub(crate) fn new_scoped_output_capture() -> Arc<Mutex<Vec<String>>> {
    Arc::new(Mutex::new(Vec::new()))
}

pub(crate) fn begin_scoped_output_capture(
    lines: Arc<Mutex<Vec<String>>>,
) -> ScopedOutputCaptureGuard {
    if let Ok(mut lines) = lines.lock() {
        lines.clear();
    }
    SCOPED_OUTPUT_CAPTURE.with(|capture| {
        *capture.borrow_mut() = Some(lines);
    });
    ScopedOutputCaptureGuard
}

pub(crate) fn scoped_output_capture_snapshot(lines: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    lines.lock().map(|lines| lines.clone()).unwrap_or_default()
}

pub(crate) fn now_millis() -> Result<u64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock appears to be before Unix epoch")?;
    u64::try_from(now.as_millis()).context("current time millis does not fit in u64")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn scoped_output_capture_is_thread_local() {
        let left = new_scoped_output_capture();
        let right = new_scoped_output_capture();

        let left_thread = {
            let left = Arc::clone(&left);
            thread::spawn(move || {
                let _guard = begin_scoped_output_capture(Arc::clone(&left));
                capture_output_line("left one");
                capture_output_line("left two");
                scoped_output_capture_snapshot(&left)
            })
        };
        let right_thread = {
            let right = Arc::clone(&right);
            thread::spawn(move || {
                let _guard = begin_scoped_output_capture(Arc::clone(&right));
                capture_output_line("right one");
                scoped_output_capture_snapshot(&right)
            })
        };

        assert_eq!(left_thread.join().unwrap(), vec!["left one", "left two"]);
        assert_eq!(right_thread.join().unwrap(), vec!["right one"]);
    }
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
        pre_checkpoint_datadir: cli
            .pre_checkpoint_datadir
            .as_ref()
            .map(|p| p.display().to_string()),
        checkpoint_utxos_gz: cli
            .checkpoint_utxos_gz
            .as_ref()
            .map(|p| p.display().to_string()),
        ..VerificationReport::default()
    }
}

pub(crate) fn write_json_report(path: &Path, report: &VerificationReport) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed creating report parent dir {}", parent.display()))?;
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
