use anyhow::{Context as _, Result, bail, ensure};
use patch_manager::{Compatibility, Patch, PatchService};
use std::path::Path;

const USAGE: &str = "Usage:
  zed-patch <checkout> list|validate
  zed-patch <checkout> enable|disable <id>
  zed-patch <checkout> apply|revert|remove <id> --confirm-source-change
  zed-patch <checkout> register <file.patch> <metadata.json>
  zed-patch <checkout> update <id> <replacement.patch>";

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let root = arguments.next().context(USAGE)?;
    if root == "--help" || root == "-h" {
        println!("{USAGE}");
        return Ok(());
    }
    let operation = arguments.next().context(USAGE)?;
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
        "update" => service.update(
            &id,
            Path::new(
                &arguments
                    .next()
                    .context("Missing replacement .patch file")?,
            ),
        )?,
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
        _ => bail!("Unknown operation: {operation}\n{USAGE}"),
    }
    println!("{}", serde_json::to_string_pretty(&service.list()?)?);
    Ok(())
}
