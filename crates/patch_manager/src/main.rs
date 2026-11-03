use anyhow::{Context as _, Result, bail, ensure};
use patch_manager::{Compatibility, Patch, PatchService};
use std::path::Path;

const USAGE: &str = "Usage:
  zed-patch <checkout> list|validate
  zed-patch <checkout> enable|disable <id>
  zed-patch <checkout> apply|revert|remove <id> --confirm-source-change
  zed-patch <checkout> register <file.patch> <metadata.json>
  zed-patch <checkout> update <id> <replacement.patch>
  zed-patch <checkout> regenerate <id> [--base <revision>] [--add <path>]...";

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
            let failed: Vec<_> = states
                .iter()
                .filter(|state| state.compatibility == Compatibility::Conflict || state.outdated)
                .map(|state| {
                    if state.outdated {
                        format!("{} (outdated; run regenerate)", state.patch.id)
                    } else {
                        format!("{} (conflict)", state.patch.id)
                    }
                })
                .collect();
            ensure!(
                failed.is_empty(),
                "Patch validation failed: {}",
                failed.join(", ")
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
        "regenerate" => {
            let mut base = None;
            let mut extra_files = Vec::new();
            while let Some(argument) = arguments.next() {
                match argument.as_str() {
                    "--base" => base = Some(arguments.next().context("Missing --base revision")?),
                    "--add" => extra_files.push(arguments.next().context("Missing --add path")?),
                    _ => bail!("Unknown regenerate option: {argument}\n{USAGE}"),
                }
            }
            if !service.regenerate(&id, base.as_deref(), &extra_files)? {
                println!("Patch already matches the checkout.");
            }
        }
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
