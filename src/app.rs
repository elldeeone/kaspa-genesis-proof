use std::io;
use std::path::PathBuf;

use crate::cli::Cli;
use crate::constants::{BOLD, END};
use crate::output::{
    build_initial_report, clear_output_capture, now_millis, output_capture_snapshot, print_error,
    print_info, print_plain, prompt_export_json_decision, write_json_report,
};
use crate::verify;

pub fn run_cli(cli: Cli) -> i32 {
    clear_output_capture();
    let mut report = build_initial_report(&cli);

    println!("{BOLD}Kaspa Genesis Proof Verification (Rust-Native){END}");
    print_plain(&format!("Requested node type: {:?}", cli.node_type));

    if let Some(datadir) = cli.datadir.as_deref() {
        print_plain(&format!("Input data directory: {}", datadir.display()));
    } else {
        print_plain("Input data directory: auto-detect (OS default Kaspa locations)");
    }

    let mut exit_code = match verify::run(&cli, &mut report) {
        Ok(success) => {
            report.success = success;
            if success { 0 } else { 1 }
        }
        Err(err) => {
            let error_chain = format!("{err:#}");
            print_error(&format!("Verification failed with error: {error_chain}"));
            report.success = false;
            report.error = Some(error_chain);
            1
        }
    };

    if let Some(json_out) = cli.json_out.as_ref() {
        report.screen_output_lines = output_capture_snapshot();
        match write_json_report(json_out, &report) {
            Ok(_) => print_info(&format!("JSON report written to {}", json_out.display())),
            Err(err) => {
                print_error(&format!("Failed writing JSON report: {err}"));
                exit_code = 1;
            }
        }
    } else {
        match prompt_export_json_decision(cli.no_input) {
            Ok(true) => {
                let json_out = PathBuf::from(format!(
                    "kaspa-proof-report-{}.json",
                    now_millis().unwrap_or(0)
                ));
                report.screen_output_lines = output_capture_snapshot();
                match write_json_report(&json_out, &report) {
                    Ok(_) => print_info(&format!("JSON report written to {}", json_out.display())),
                    Err(err) => {
                        print_error(&format!("Failed writing JSON report: {err}"));
                        exit_code = 1;
                    }
                }
            }
            Ok(false) => {}
            Err(err) => {
                print_error(&format!("Failed during export prompt: {err}"));
                exit_code = 1;
            }
        }
    }

    if cli.pause_on_exit {
        print_plain("");
        print_plain("Press Enter to exit...");
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
    }

    exit_code
}
