//! Move 1 acceptance: `Runtime::new()` runs and writes zero files anywhere.

use std::path::PathBuf;

use execra::{Command, Outcome, Runtime};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    let before = snapshot_dir(&cwd);

    let rt = Runtime::new();
    let outcome = rt.spawn(Command::new("cmd").args(["/C", "echo", "hi"]))?.await;
    println!("outcome: {outcome:?}");
    assert!(matches!(outcome, Outcome::Succeeded { .. }), "expected success");

    let after = snapshot_dir(&cwd);
    let new_paths: Vec<_> = after.iter().filter(|p| !before.contains(p)).collect();
    if !new_paths.is_empty() {
        panic!("Runtime::new() created files: {new_paths:?}");
    }
    println!("OK — Runtime::new() wrote zero files in {}", cwd.display());

    // Sanity: did NOT create the legacy execra.db / execra-logs/ either.
    assert!(!PathBuf::from("execra.db").exists(), "execra.db must not exist");
    assert!(
        !PathBuf::from("execra-logs").exists(),
        "execra-logs/ must not exist"
    );
    println!("OK — no execra.db / execra-logs created");
    Ok(())
}

fn snapshot_dir(p: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(p) {
        for entry in rd.flatten() {
            out.push(entry.path());
        }
    }
    out.sort();
    out
}
