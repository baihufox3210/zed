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
    lock_path: PathBuf,
}

pub struct DiffPreview {
    pub pairs: Vec<[String; 2]>,
    _directory: tempfile::TempDir,
}

impl Manifest {
    fn find(&self, id: &str) -> Result<&Patch> {
        self.patches
            .iter()
            .find(|patch| patch.id == id)
            .with_context(|| format!("Unknown patch ID: {id}"))
    }

    fn find_mut(&mut self, id: &str) -> Result<&mut Patch> {
        self.patches
            .iter_mut()
            .find(|patch| patch.id == id)
            .with_context(|| format!("Unknown patch ID: {id}"))
    }
}

impl PatchService {
    pub fn open(root: &Path) -> Result<Self> {
        let root = fs::canonicalize(root)?;
        let output = git_output(&root, &["rev-parse", "--show-toplevel"])
            .context("Open a local Git source checkout first")?;
        let root = fs::canonicalize(output.trim())?;
        ensure!(
            root.join("patches/manifest.json").is_file(),
            "This checkout has no patches/manifest.json. Open the Zed source checkout containing Patch Manager."
        );
        ensure!(
            fs::canonicalize(root.join("patches"))? == root.join("patches"),
            "Registry directory cannot be a symlink"
        );
        let lock_path = git_output(
            &root,
            &["rev-parse", "--git-path", "zed-patch-manager.lock"],
        )
        .context("Cannot locate Git directory")?;
        let lock_path = root.join(lock_path.trim());
        Ok(Self { root, lock_path })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn registry(&self) -> PathBuf {
        self.root.join("patches")
    }

    fn lock(&self) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock_path)?;
        file.lock()?;
        Ok(file)
    }

    fn read_manifest(&self) -> Result<Manifest> {
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(self.registry().join("manifest.json"))?)?;
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
        let mut temporary = tempfile::NamedTempFile::new_in(self.registry())?;
        temporary.write_all(&serde_json::to_vec_pretty(manifest)?)?;
        temporary.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        temporary.persist(self.registry().join("manifest.json"))?;
        Ok(())
    }

    fn patch_path(&self, patch: &Patch) -> Result<PathBuf> {
        ensure!(
            safe_relative_path(&patch.source) && patch.source.starts_with("patches/"),
            "Invalid patch source"
        );
        let path = fs::canonicalize(self.root.join(&patch.source))?;
        ensure!(
            path.starts_with(self.registry()),
            "Patch source escapes registry"
        );
        ensure!(
            fs::metadata(&path)?.len() <= MAX_PATCH_SIZE,
            "Patch exceeds 10 MiB"
        );
        let bytes = fs::read(&path)?;
        ensure!(
            sha256(&bytes) == patch.sha256,
            "Patch content changed: SHA-256 mismatch"
        );
        Ok(path)
    }

    /// Copies a user-selected patch into the registry directory and validates
    /// it, so that the checked content is exactly the content that gets persisted.
    fn stage(&self, source: &Path) -> Result<(Vec<u8>, tempfile::NamedTempFile)> {
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
        let mut temporary = tempfile::NamedTempFile::new_in(self.registry())?;
        temporary.write_all(&content)?;
        temporary.as_file().sync_all()?;
        self.affected_files(temporary.path())?;
        Ok((content, temporary))
    }

    fn affected_files(&self, path: &Path) -> Result<Vec<String>> {
        let content = fs::read_to_string(path).context("Only UTF-8 text patches are supported")?;
        ensure!(
            !content.lines().any(is_unsupported_header),
            "Symlink, submodule and binary patches are not supported"
        );
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&self.root)
            .args(["apply", "--numstat", "-z", "--"])
            .arg(path);
        let output = run_command(&mut command)?;
        ensure!(
            output.status.success(),
            "Invalid patch: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut entries = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty());
        // `--numstat` reports only the destination of a pure rename or copy,
        // so the source paths are taken from the extended headers.
        let mut paths: Vec<&str> = content
            .lines()
            .filter_map(|line| {
                ["rename from ", "copy from "]
                    .iter()
                    .find_map(|prefix| line.strip_prefix(prefix))
            })
            .collect();
        while let Some(entry) = entries.next() {
            let mut fields = std::str::from_utf8(entry)?.splitn(3, '\t');
            let added = fields.next();
            let deleted = fields.next();
            let path = fields.next().context("Unsupported patch path format")?;
            ensure!(
                added != Some("-") && deleted != Some("-"),
                "Binary patches are not supported"
            );
            // With `-z`, a rename that also edits content leaves the path field
            // empty and lists the old and new paths as the next two entries.
            if path.is_empty() {
                for _ in 0..2 {
                    paths.push(std::str::from_utf8(
                        entries.next().context("Unsupported rename format")?,
                    )?);
                }
            } else {
                paths.push(path);
            }
        }
        let mut files: Vec<String> = Vec::new();
        for file in paths {
            ensure!(safe_relative_path(file), "Unsafe affected path: {file}");
            ensure!(
                !file.starts_with("patches/")
                    && !file
                        .split('/')
                        .any(|part| part.eq_ignore_ascii_case(".git")),
                "Patches cannot modify Git metadata or the patch registry"
            );
            if !files.iter().any(|existing| existing == file) {
                files.push(file.to_owned());
            }
        }
        ensure!(!files.is_empty(), "Patch contains no text changes");
        Ok(files)
    }

    fn compatibility(&self, path: &Path) -> Result<(Compatibility, String)> {
        let check = |reverse: bool| {
            let mut command = Command::new("git");
            command.arg("-C").arg(&self.root).args(["apply", "--check"]);
            if reverse {
                command.arg("--reverse");
            }
            run_command(command.arg("--").arg(path))
        };
        let forward = check(false)?;
        let reverse = check(true)?;
        Ok(
            match (forward.status.success(), reverse.status.success()) {
                (false, true) => (
                    Compatibility::AlreadyApplied,
                    "Patch already applied to source. Build and restart required; runtime state is unverified.".to_owned(),
                ),
                (true, false) => (
                    Compatibility::Compatible,
                    "Patch compatible with the current source checkout.".to_owned(),
                ),
                _ => (
                    Compatibility::Conflict,
                    format!(
                        "Patch conflict or ambiguous application state.\n{}\n{}",
                        String::from_utf8_lossy(&forward.stderr).trim(),
                        String::from_utf8_lossy(&reverse.stderr).trim()
                    ),
                ),
            },
        )
    }

    /// Returns the verified registry path together with the state so callers
    /// that go on to run Git do not hash the patch a second time.
    fn inspect(&self, patch: Patch) -> Result<(PatchState, PathBuf)> {
        let path = self.patch_path(&patch)?;
        let affected_files = self.affected_files(&path)?;
        let (compatibility, validation) = self.compatibility(&path)?;
        let rebuild_required =
            patch.rebuild_required || compatibility == Compatibility::AlreadyApplied;
        Ok((
            PatchState {
                patch,
                compatibility,
                affected_files,
                validation,
                rebuild_required,
            },
            path,
        ))
    }

    pub fn list(&self) -> Result<Vec<PatchState>> {
        let _lock = self.lock()?;
        Ok(self
            .read_manifest()?
            .patches
            .into_iter()
            .map(|patch| match self.inspect(patch.clone()) {
                Ok((state, _)) => state,
                Err(error) => PatchState {
                    rebuild_required: patch.rebuild_required,
                    patch,
                    compatibility: Compatibility::Conflict,
                    affected_files: Vec::new(),
                    validation: format!("Validation failed: {error:#}"),
                },
            })
            .collect())
    }

    pub fn register(&self, source: &Path, mut patch: Patch) -> Result<()> {
        let _lock = self.lock()?;
        validate_metadata(&patch)?;
        let mut manifest = self.read_manifest()?;
        ensure!(
            manifest.find(&patch.id).is_err(),
            "Patch ID already registered"
        );
        let (content, temporary) = self.stage(source)?;
        let (compatibility, validation) = self.compatibility(temporary.path())?;
        ensure!(
            compatibility != Compatibility::Conflict,
            "Patch is incompatible: {validation}"
        );
        patch.rebuild_required = false;
        patch.source = format!("patches/{}.patch", patch.id);
        patch.sha256 = sha256(&content);
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

    pub fn update(&self, id: &str, source: &Path) -> Result<()> {
        let _lock = self.lock()?;
        let mut manifest = self.read_manifest()?;
        let patch = manifest.find_mut(id)?;
        let (state, destination) = self.inspect(patch.clone())?;
        ensure!(
            state.compatibility == Compatibility::Compatible,
            "Revert the applied patch and resolve conflicts before updating its source"
        );
        let (content, temporary) = self.stage(source)?;
        let previous = fs::read(&destination)?;
        ensure!(
            content != previous,
            "This patch version is already registered"
        );
        let (compatibility, validation) = self.compatibility(temporary.path())?;
        ensure!(
            compatibility == Compatibility::Compatible,
            "Updated patch is incompatible: {validation}"
        );
        temporary.persist(&destination)?;
        patch.sha256 = sha256(&content);
        if let Err(error) = self.write_manifest(&manifest) {
            let mut rollback = tempfile::NamedTempFile::new_in(self.registry())?;
            rollback.write_all(&previous)?;
            rollback.as_file().sync_all()?;
            rollback
                .persist(&destination)
                .context("Manifest update failed and patch rollback failed")?;
            return Err(error);
        }
        Ok(())
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let _lock = self.lock()?;
        let mut manifest = self.read_manifest()?;
        let patch = manifest.find_mut(id)?;
        if patch.enabled == enabled {
            return Ok(());
        }
        patch.enabled = enabled;
        self.write_manifest(&manifest)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let _lock = self.lock()?;
        let mut manifest = self.read_manifest()?;
        manifest.find(id)?;
        manifest.patches.retain(|patch| patch.id != id);
        self.write_manifest(&manifest)
    }

    pub fn apply(&self, id: &str, reverse: bool) -> Result<()> {
        let _lock = self.lock()?;
        let mut manifest = self.read_manifest()?;
        let (state, path) = self.inspect(manifest.find(id)?.clone())?;
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
        manifest.find_mut(id)?.rebuild_required = true;
        self.write_manifest(&manifest)
            .context("Source changed, but could not persist the rebuild requirement")?;
        Ok(())
    }

    pub fn preview(&self, id: &str) -> Result<DiffPreview> {
        let _lock = self.lock()?;
        let (state, patch_path) = self.inspect(self.read_manifest()?.find(id)?.clone())?;
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
        let output = run_command(command.arg("--").arg(&patch_path))?;
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

fn git_output(root: &Path, arguments: &[&str]) -> Result<String> {
    let output = run_command(Command::new("git").arg("-C").arg(root).args(arguments))?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn sha256(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

/// Only extended header lines can declare symlink, submodule or binary
/// content; hunk lines that merely contain the same text are ordinary changes.
fn is_unsupported_header(line: &str) -> bool {
    if line.starts_with(['+', '-', ' ']) {
        return false;
    }
    let special_mode = |mode: &str| mode == "120000" || mode == "160000";
    line == "GIT binary patch"
        || line.starts_with("Binary files ")
        || [
            "new file mode ",
            "deleted file mode ",
            "old mode ",
            "new mode ",
        ]
        .iter()
        .any(|prefix| line.strip_prefix(prefix).is_some_and(special_mode))
        || line
            .strip_prefix("index ")
            .and_then(|index| index.rsplit_once(' '))
            .is_some_and(|(_, mode)| special_mode(mode))
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
