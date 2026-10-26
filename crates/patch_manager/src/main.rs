use anyhow::{Context as _, Result, bail, ensure};
use patch_manager::{Compatibility, Patch, PatchService};
use std::path::Path;

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let root = arguments.next().context("Usage: zed-patch <checkout> <list|validate|enable|disable|apply|revert|remove|register> [id] [--confirm-source-change]")?;
    let operation = arguments.next().context("Missing operation")?;
    let service = PatchService::open(Path::new(&root))?;
    if operation == "list" || operation == "validate" {
        let states = service.list()?;
        println!("{}", serde_json::to_string_pretty(&states)?);
        if operation == "validate" {
            ensure!(
                states
                    .iter()
                    .all(|state| state.compatibility != Compatibility::Conflict),
                "Patch validation failed"
            );
        }
        return Ok(());
    }
    if operation == "register" {
        let source = arguments.next().context("Missing .patch file")?;
        let metadata = arguments.next().context(
            "Missing metadata JSON file (id, name, description, category, source, sha256, enabled)",
        )?;
        let patch: Patch = serde_json::from_slice(&std::fs::read(metadata)?)?;
        service.register(Path::new(&source), patch)?;
        println!(
            "Registered. Source unchanged; activation and application are separate operations."
        );
        return Ok(());
    }
    let id = arguments.next().context("Missing patch ID")?;
    match operation.as_str() {
        "enable" => service.set_enabled(&id, true)?,
        "disable" => service.set_enabled(&id, false)?,
        "apply" | "revert" | "remove" => {
            ensure!(
                arguments.next().as_deref() == Some("--confirm-source-change"),
                "Review the patch first, then pass --confirm-source-change. Apply/revert changes source and requires a build/restart; remove deletes registration only."
            );
            match operation.as_str() {
                "apply" => service.apply(&id, false)?,
                "revert" => service.apply(&id, true)?,
                _ => service.remove(&id)?,
            }
        }
        _ => bail!("Unknown operation: {operation}"),
    }
    println!("{}", serde_json::to_string_pretty(&service.list()?)?);
    Ok(())
}
