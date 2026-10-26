use super::*;

const PATCH: &str = "diff --git a/example.txt b/example.txt\n--- a/example.txt\n+++ b/example.txt\n@@ -1 +1 @@\n-before\n+after\n";

fn checkout() -> Result<(tempfile::TempDir, PatchService)> {
    let directory = tempfile::tempdir()?;
    let output = run_command(
        Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(directory.path()),
    )?;
    ensure!(output.status.success(), "Git init failed");
    fs::create_dir(directory.path().join("patches"))?;
    fs::write(
        directory.path().join("patches/manifest.json"),
        "{\"patches\":[]}",
    )?;
    fs::write(directory.path().join("example.txt"), "before\n")?;
    let service = PatchService::open(directory.path())?;
    Ok((directory, service))
}

fn metadata() -> Patch {
    Patch {
        id: "example".into(),
        name: "Example".into(),
        description: "A test patch".into(),
        category: "Other".into(),
        source: String::new(),
        sha256: String::new(),
        enabled: false,
        rebuild_required: false,
    }
}

fn register(service: &PatchService, content: &str) -> Result<()> {
    let source = service.root.join("input.patch");
    fs::write(&source, content)?;
    service.register(&source, metadata())
}

fn first(service: &PatchService) -> Result<PatchState> {
    service
        .list()?
        .into_iter()
        .next()
        .context("Missing test patch")
}

#[test]
fn registration_activation_application_and_revert_are_separate() -> Result<()> {
    let (_directory, service) = checkout()?;
    register(&service, PATCH)?;
    assert_eq!(
        fs::read_to_string(service.root.join("example.txt"))?,
        "before\n"
    );
    assert!(!first(&service)?.patch.enabled);
    assert!(service.apply("example", false).is_err());
    service.set_enabled("example", true)?;
    assert_eq!(first(&service)?.compatibility, Compatibility::Compatible);
    service.apply("example", false)?;
    let state = first(&service)?;
    assert_eq!(state.compatibility, Compatibility::AlreadyApplied);
    assert!(state.rebuild_required);
    assert!(service.apply("example", false).is_err());
    service.set_enabled("example", false)?;
    assert_eq!(
        first(&service)?.compatibility,
        Compatibility::AlreadyApplied
    );
    service.apply("example", true)?;
    assert_eq!(
        fs::read_to_string(service.root.join("example.txt"))?,
        "before\n"
    );
    assert!(first(&service)?.rebuild_required);
    service.remove("example")?;
    assert!(service.list()?.is_empty());
    Ok(())
}

#[test]
fn conflicting_edits_are_preserved() -> Result<()> {
    let (_directory, service) = checkout()?;
    register(&service, PATCH)?;
    service.set_enabled("example", true)?;
    fs::write(service.root.join("example.txt"), "user changes\n")?;
    assert_eq!(first(&service)?.compatibility, Compatibility::Conflict);
    assert!(service.apply("example", false).is_err());
    assert!(service.apply("example", true).is_err());
    assert_eq!(
        fs::read_to_string(service.root.join("example.txt"))?,
        "user changes\n"
    );
    Ok(())
}

#[test]
fn preview_uses_snapshots_before_and_after_application() -> Result<()> {
    let (_directory, service) = checkout()?;
    register(&service, PATCH)?;
    for applied in [false, true] {
        if applied {
            service.set_enabled("example", true)?;
            service.apply("example", false)?;
        }
        let preview = service.preview("example")?;
        let pair = preview.pairs.first().context("Missing diff pair")?;
        let [before, after] = pair;
        assert_eq!(fs::read_to_string(before)?, "before\n");
        assert_eq!(fs::read_to_string(after)?, "after\n");
        assert_eq!(
            fs::read_to_string(service.root.join("example.txt"))?,
            if applied { "after\n" } else { "before\n" }
        );
    }
    Ok(())
}

#[test]
fn malformed_duplicate_and_incompatible_registrations_fail() -> Result<()> {
    let (_directory, service) = checkout()?;
    assert!(register(&service, "not a patch").is_err());
    assert!(service.list()?.is_empty());
    fs::write(service.root.join("example.txt"), "conflict\n")?;
    assert!(register(&service, PATCH).is_err());
    fs::write(service.root.join("example.txt"), "before\n")?;
    register(&service, PATCH)?;
    assert!(register(&service, PATCH).is_err());
    assert_eq!(service.list()?.len(), 1);
    Ok(())
}

#[test]
fn changed_patch_content_is_reported_and_cannot_apply() -> Result<()> {
    let (_directory, service) = checkout()?;
    register(&service, PATCH)?;
    fs::write(service.root.join("patches/example.patch"), "tampered")?;
    let state = first(&service)?;
    assert_eq!(state.compatibility, Compatibility::Conflict);
    assert!(state.validation.contains("SHA-256 mismatch"));
    assert!(service.apply("example", false).is_err());
    Ok(())
}

#[test]
fn rejects_unsafe_paths_and_metadata() -> Result<()> {
    let (_directory, service) = checkout()?;
    assert!(register(&service, &PATCH.replace("example.txt", "../example.txt")).is_err());
    assert!(register(&service, &PATCH.replace("example.txt", ".git/config")).is_err());
    assert!(
        register(
            &service,
            &PATCH.replace("example.txt", "patches/manifest.json")
        )
        .is_err()
    );
    let mut unicode_metadata = metadata();
    unicode_metadata.name = "補".repeat(200);
    assert!(validate_metadata(&unicode_metadata).is_ok());
    unicode_metadata.name.push('丁');
    assert!(validate_metadata(&unicode_metadata).is_err());
    for id in ["", "../escape", "Example", "with spaces"] {
        let mut patch = metadata();
        patch.id = id.into();
        assert!(validate_metadata(&patch).is_err());
    }
    Ok(())
}

#[test]
fn concurrent_services_do_not_lose_manifest_changes() -> Result<()> {
    let (_directory, service) = checkout()?;
    register(&service, PATCH)?;
    let other = service.clone();
    let thread = std::thread::spawn(move || other.set_enabled("example", true));
    for _ in 0..10 {
        assert_eq!(service.list()?.len(), 1);
    }
    thread
        .join()
        .map_err(|_| anyhow::anyhow!("Service thread panicked"))??;
    assert!(first(&service)?.patch.enabled);
    Ok(())
}

#[test]
fn built_in_wayland_patch_validates_previews_reverts_and_reapplies() -> Result<()> {
    let (_directory, service) = checkout()?;
    let client_path = "crates/gpui_linux/src/linux/wayland/client.rs";
    let window_path = "crates/gpui_linux/src/linux/wayland/window.rs";
    let client = include_str!("../../gpui_linux/src/linux/wayland/client.rs");
    let window = include_str!("../../gpui_linux/src/linux/wayland/window.rs");
    for (path, content) in [(client_path, client), (window_path, window)] {
        let destination = service.root.join(path);
        fs::create_dir_all(destination.parent().context("Missing parent")?)?;
        fs::write(destination, content)?;
    }
    fs::write(
        service.root.join("patches/manifest.json"),
        include_str!("../../../patches/manifest.json"),
    )?;
    fs::write(
        service
            .root
            .join("patches/wayland-ime-preedit-cursor.patch"),
        include_str!("../../../patches/wayland-ime-preedit-cursor.patch"),
    )?;
    let id = "wayland-ime-preedit-cursor";
    assert_eq!(
        first(&service)?.compatibility,
        Compatibility::AlreadyApplied
    );
    let preview = service.preview(id)?;
    assert_eq!(preview.pairs.len(), 2);
    service.apply(id, true)?;
    assert_eq!(first(&service)?.compatibility, Compatibility::Compatible);
    service.apply(id, false)?;
    assert_eq!(
        first(&service)?.compatibility,
        Compatibility::AlreadyApplied
    );
    assert_eq!(fs::read_to_string(service.root.join(client_path))?, client);
    assert_eq!(fs::read_to_string(service.root.join(window_path))?, window);
    Ok(())
}

#[test]
fn removing_registration_preserves_source_and_allows_reregistration() -> Result<()> {
    let (_directory, service) = checkout()?;
    register(&service, PATCH)?;
    service.set_enabled("example", true)?;
    service.apply("example", false)?;
    service.remove("example")?;
    assert!(service.list()?.is_empty());
    assert_eq!(
        fs::read_to_string(service.root.join("example.txt"))?,
        "after\n"
    );
    register(&service, PATCH)?;
    assert_eq!(
        first(&service)?.compatibility,
        Compatibility::AlreadyApplied
    );
    fs::write(service.root.join("patches/example.patch"), "invalid")?;
    assert_eq!(first(&service)?.compatibility, Compatibility::Conflict);
    service.remove("example")?;
    assert!(service.list()?.is_empty());
    assert_eq!(
        fs::read_to_string(service.root.join("example.txt"))?,
        "after\n"
    );
    Ok(())
}
