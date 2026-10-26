use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

pub const CATEGORIES: &[&str] = &[
    "Input Method",
    "Display",
    "Compatibility",
    "Editor",
    "Other",
];
const MAX_PATCH_SIZE: u64 = 10 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub rebuild_required: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    patches: Vec<Patch>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Compatibility {
    Compatible,
    AlreadyApplied,
    Conflict,
}

#[derive(Clone, Debug, Serialize)]
pub struct PatchState {
    pub patch: Patch,
    pub compatibility: Compatibility,
    pub affected_files: Vec<String>,
    pub validation: String,
    pub rebuild_required: bool,
}

#[derive(Clone)]
pub struct PatchService {
    root: PathBuf,
}

pub struct DiffPreview {
    pub pairs: Vec<[String; 2]>,
    _directory: tempfile::TempDir,
}

impl PatchService {
    pub fn open(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root)?;
        let output = run_command(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["rev-parse", "--show-toplevel"]),
        )?;
        ensure!(
            output.status.success(),
            "Open a local Git source checkout first"
        );
        let git_root = PathBuf::from(String::from_utf8(output.stdout)?.trim());
        let root = fs::canonicalize(git_root)?;
        ensure!(
            root.join("patches/manifest.json").is_file(),
            "This checkout has no patches/manifest.json. Open the Zed source checkout containing Patch Manager."
        );
        ensure!(
            fs::canonicalize(root.join("patches"))? == root.join("patches"),
            "Registry directory cannot be a symlink"
        );
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn lock(&self) -> Result<File> {
        let output = self.git(&["rev-parse", "--git-path", "zed-patch-manager.lock"])?;
        ensure!(output.status.success(), "Cannot locate Git directory");
        let path = PathBuf::from(String::from_utf8(output.stdout)?.trim());
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join(path))?;
        file.lock()?;
        Ok(file)
    }

    fn git(&self, arguments: &[&str]) -> Result<Output> {
        run_command(
            Command::new("git")
                .arg("-C")
                .arg(&self.root)
                .args(arguments),
        )
    }

    fn read_manifest(&self) -> Result<Manifest> {
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(self.root.join("patches/manifest.json"))?)?;
        let mut identifiers = std::collections::HashSet::new();
        for patch in &manifest.patches {
            validate_metadata(patch)?;
            ensure!(
                identifiers.insert(&patch.id),
                "Duplicate patch ID: {}",
                patch.id
            );
        }
        Ok(manifest)
    }

    fn write_manifest(&self, manifest: &Manifest) -> Result<()> {
        let mut temporary = tempfile::NamedTempFile::new_in(self.root.join("patches"))?;
        temporary.write_all(&serde_json::to_vec_pretty(manifest)?)?;
        temporary.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        temporary.persist(self.root.join("patches/manifest.json"))?;
        Ok(())
    }

    fn patch_path(&self, patch: &Patch) -> Result<PathBuf> {
        ensure!(
            safe_relative_path(&patch.source) && patch.source.starts_with("patches/"),
            "Invalid patch source"
        );
        let path = fs::canonicalize(self.root.join(&patch.source))?;
        ensure!(
            path.starts_with(self.root.join("patches")),
            "Patch source escapes registry"
        );
        ensure!(
            fs::metadata(&path)?.len() <= MAX_PATCH_SIZE,
            "Patch exceeds 10 MiB"
        );
        let bytes = fs::read(&path)?;
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == patch.sha256,
            "Patch content changed: SHA-256 mismatch"
        );
        Ok(path)
    }

    fn affected_files(&self, path: &Path) -> Result<Vec<String>> {
        let content = fs::read_to_string(path).context("Only UTF-8 text patches are supported")?;
        ensure!(
            !content.lines().any(|line| line.contains("mode 120000")
                || line.contains("mode 160000")
                || line == "GIT binary patch"),
            "Symlink, submodule and binary patches are not supported"
        );
        let output = run_command(
            Command::new("git")
                .arg("-C")
                .arg(&self.root)
                .args(["apply", "--numstat", "-z", "--"])
                .arg(path),
        )?;
        ensure!(
            output.status.success(),
            "Invalid patch: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut files = Vec::new();
        for entry in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let entry = std::str::from_utf8(entry)?;
            let file = entry
                .splitn(3, '\t')
                .nth(2)
                .context("Unsupported patch path format")?;
            ensure!(safe_relative_path(file), "Unsafe affected path: {file}");
            ensure!(
                !file.starts_with("patches/")
                    && !file
                        .split('/')
                        .any(|part| part.eq_ignore_ascii_case(".git")),
                "Patches cannot modify Git metadata or the patch registry"
            );
            files.push(file.to_owned());
        }
        ensure!(!files.is_empty(), "Patch contains no text changes");
        Ok(files)
    }

    fn check(&self, path: &Path, reverse: bool) -> Result<Output> {
        let mut command = Command::new("git");
        command.arg("-C").arg(&self.root).args(["apply", "--check"]);
        if reverse {
            command.arg("--reverse");
        }
        run_command(command.arg("--").arg(path))
    }

    fn state(&self, patch: Patch) -> Result<PatchState> {
        let path = self.patch_path(&patch)?;
        let affected_files = self.affected_files(&path)?;
        let forward = self.check(&path, false)?;
        let reverse = self.check(&path, true)?;
        let (compatibility, validation) = if reverse.status.success() && !forward.status.success() {
            (Compatibility::AlreadyApplied, "Patch already applied to source. Build and restart required; runtime state is unverified.".to_owned())
        } else if forward.status.success() && !reverse.status.success() {
            (
                Compatibility::Compatible,
                "Patch compatible with the current source checkout.".to_owned(),
            )
        } else {
            (
                Compatibility::Conflict,
                format!(
                    "Patch conflict or ambiguous application state.\n{}\n{}",
                    String::from_utf8_lossy(&forward.stderr),
                    String::from_utf8_lossy(&reverse.stderr)
                ),
            )
        };
        let rebuild_required =
            patch.rebuild_required || compatibility == Compatibility::AlreadyApplied;
        Ok(PatchState {
            patch,
            compatibility,
            affected_files,
            validation,
            rebuild_required,
        })
    }

    pub fn list(&self) -> Result<Vec<PatchState>> {
        let _lock = self.lock()?;
        self.read_manifest()?
            .patches
            .into_iter()
            .map(|patch| match self.state(patch.clone()) {
                Ok(state) => Ok(state),
                Err(error) => Ok(PatchState {
                    rebuild_required: patch.rebuild_required,
                    patch,
                    compatibility: Compatibility::Conflict,
                    affected_files: Vec::new(),
                    validation: format!("Validation failed: {error:#}"),
                }),
            })
            .collect()
    }

    pub fn register(&self, source: &Path, mut patch: Patch) -> Result<()> {
        let _lock = self.lock()?;
        validate_metadata(&patch)?;
        ensure!(
            source
                .extension()
                .is_some_and(|extension| extension == "patch"),
            "Select a .patch file"
        );
        ensure!(
            fs::metadata(source)?.len() <= MAX_PATCH_SIZE,
            "Patch exceeds 10 MiB"
        );
        let content = fs::read(source)?;
        let mut manifest = self.read_manifest()?;
        ensure!(
            !manifest.patches.iter().any(|entry| entry.id == patch.id),
            "Patch ID already registered"
        );
        patch.rebuild_required = false;
        patch.source = format!("patches/{}.patch", patch.id);
        patch.sha256 = format!("{:x}", Sha256::digest(&content));
        let mut temporary = tempfile::NamedTempFile::new_in(self.root.join("patches"))?;
        temporary.write_all(&content)?;
        self.affected_files(temporary.path())?;
        let forward = self.check(temporary.path(), false)?;
        let reverse = self.check(temporary.path(), true)?;
        ensure!(
            forward.status.success() || reverse.status.success(),
            "Patch is incompatible:\n{}",
            String::from_utf8_lossy(&forward.stderr)
        );
        temporary.as_file().sync_all()?;
        let destination = self.root.join(&patch.source);
        let created = if destination.exists() {
            ensure!(
                fs::read(&destination)? == content,
                "A different patch file already exists at {}",
                destination.display()
            );
            false
        } else {
            temporary.persist_noclobber(&destination)?;
            true
        };
        manifest.patches.push(patch);
        if let Err(error) = self.write_manifest(&manifest) {
            if created {
                fs::remove_file(destination)
                    .context("Manifest write failed and patch cleanup failed")?;
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let _lock = self.lock()?;
        let mut manifest = self.read_manifest()?;
        let patch = manifest
            .patches
            .iter_mut()
            .find(|patch| patch.id == id)
            .context("Unknown patch ID")?;
        patch.enabled = enabled;
        self.write_manifest(&manifest)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let _lock = self.lock()?;
        let mut manifest = self.read_manifest()?;
        ensure!(
            manifest.patches.iter().any(|patch| patch.id == id),
            "Unknown patch ID"
        );
        manifest.patches.retain(|patch| patch.id != id);
        self.write_manifest(&manifest)
    }

    pub fn apply(&self, id: &str, reverse: bool) -> Result<()> {
        let _lock = self.lock()?;
        let mut manifest = self.read_manifest()?;
        let patch = manifest
            .patches
            .iter()
            .find(|patch| patch.id == id)
            .context("Unknown patch ID")?
            .clone();
        let state = self.state(patch)?;
        if reverse {
            ensure!(
                state.compatibility == Compatibility::AlreadyApplied,
                "Patch is not fully applied"
            );
        } else {
            ensure!(state.patch.enabled, "Enable the patch before applying it");
            ensure!(
                state.compatibility == Compatibility::Compatible,
                "Patch already applied or conflicting"
            );
        }
        let path = self.patch_path(&state.patch)?;
        // Never use --reject or --3way: a failed operation must not leave partial changes.
        let mut command = Command::new("git");
        command.arg("-C").arg(&self.root).arg("apply");
        if reverse {
            command.arg("--reverse");
        }
        let output = run_command(command.arg("--").arg(path))?;
        ensure!(
            output.status.success(),
            "Git apply failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let patch = manifest
            .patches
            .iter_mut()
            .find(|patch| patch.id == id)
            .context("Unknown patch ID")?;
        patch.rebuild_required = true;
        self.write_manifest(&manifest)
            .context("Source changed, but could not persist the rebuild requirement")?;
        Ok(())
    }

    pub fn preview(&self, id: &str) -> Result<DiffPreview> {
        let _lock = self.lock()?;
        let patch = self
            .read_manifest()?
            .patches
            .into_iter()
            .find(|patch| patch.id == id)
            .context("Unknown patch ID")?;
        let state = self.state(patch)?;
        ensure!(
            state.compatibility != Compatibility::Conflict,
            "Resolve compatibility errors before viewing diff"
        );
        let directory = tempfile::tempdir()?;
        let before = directory.path().join("before");
        let after = directory.path().join("after");
        fs::create_dir_all(&before)?;
        fs::create_dir_all(&after)?;
        for file in &state.affected_files {
            let source = self.root.join(file);
            if source.is_file() {
                let canonical = fs::canonicalize(&source)?;
                ensure!(
                    canonical.starts_with(&self.root),
                    "Affected file escapes checkout"
                );
                for destination in [&before, &after] {
                    let target = destination.join(file);
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::copy(&canonical, target)?;
                }
            }
        }
        let destination = if state.compatibility == Compatibility::AlreadyApplied {
            &before
        } else {
            &after
        };
        // An isolated Git directory prevents Git from discovering the user's checkout.
        let output = run_command(
            Command::new("git")
                .arg("init")
                .arg("--quiet")
                .arg(destination),
        )?;
        ensure!(
            output.status.success(),
            "Could not initialize diff snapshot"
        );
        let mut command = Command::new("git");
        command.arg("-C").arg(destination).arg("apply");
        if state.compatibility == Compatibility::AlreadyApplied {
            command.arg("--reverse");
        }
        let output = run_command(command.arg("--").arg(self.patch_path(&state.patch)?))?;
        ensure!(
            output.status.success(),
            "Could not create diff: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut pairs = Vec::new();
        for file in state.affected_files {
            let paths = [before.join(&file), after.join(&file)];
            for path in &paths {
                if !path.exists() {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    File::create(path)?;
                }
            }
            pairs.push(paths.map(|path| path.to_string_lossy().into_owned()));
        }
        Ok(DiffPreview {
            pairs,
            _directory: directory,
        })
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "This synchronous service runs on background executors or CLI threads"
)]
fn run_command(command: &mut Command) -> Result<Output> {
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .context("Could not run Git")
}

fn safe_relative_path(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('\\')
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn validate_metadata(patch: &Patch) -> Result<()> {
    ensure!(
        !patch.id.is_empty()
            && patch.id.len() <= 80
            && patch
                .id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
        "Patch ID must contain 1–80 lowercase letters, digits or hyphens"
    );
    ensure!(
        !patch.name.trim().is_empty() && patch.name.chars().count() <= 200,
        "Name must contain 1–200 characters"
    );
    ensure!(
        !patch.description.trim().is_empty() && patch.description.chars().count() <= 4000,
        "Description must contain 1–4000 characters"
    );
    if !CATEGORIES.contains(&patch.category.as_str()) {
        bail!("Choose a supported category");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
