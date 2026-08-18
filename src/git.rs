use std::ffi::OsStr;
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use sha1::Digest;
use tempfile::NamedTempFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFormat {
    Sha1,
    Sha256,
}

impl ObjectFormat {
    pub(crate) fn oid_hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }
}

pub(crate) struct Repository {
    root: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    index_path: PathBuf,
    objects_dir: PathBuf,
    object_format: ObjectFormat,
}

pub(crate) struct Head {
    pub(crate) reference: String,
    pub(crate) oid: Option<String>,
}

pub(crate) struct Identities {
    pub(crate) author: String,
    pub(crate) committer: String,
}

pub(crate) struct IndexLock {
    file: Option<File>,
    path: PathBuf,
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

impl Repository {
    pub(crate) fn discover(start_dir: &Path) -> Result<Self> {
        if let Some(repository) = Self::discover_standard(start_dir)? {
            return Ok(repository);
        }
        Self::discover_with_git(start_dir)
    }

    fn discover_standard(start_dir: &Path) -> Result<Option<Self>> {
        for variable in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_CEILING_DIRECTORIES",
        ] {
            if std::env::var_os(variable).is_some() {
                return Ok(None);
            }
        }

        let start = start_dir
            .canonicalize()
            .with_context(|| format!("could not resolve {}", start_dir.display()))?;
        for root in start.ancestors() {
            let dot_git = root.join(".git");
            let git_dir = if dot_git.is_dir() {
                dot_git.canonicalize().with_context(|| {
                    format!("could not resolve Git directory {}", dot_git.display())
                })?
            } else if dot_git.is_file() {
                let contents = fs::read_to_string(&dot_git)
                    .with_context(|| format!("could not read Git file {}", dot_git.display()))?;
                let target = contents
                    .trim()
                    .strip_prefix("gitdir: ")
                    .context("the .git file does not contain a gitdir target")?;
                let target = PathBuf::from(target);
                let target = if target.is_absolute() {
                    target
                } else {
                    root.join(target)
                };
                target.canonicalize().with_context(|| {
                    format!("could not resolve Git directory {}", target.display())
                })?
            } else {
                continue;
            };

            let common_dir = match fs::read_to_string(git_dir.join("commondir")) {
                Ok(contents) => {
                    let path = PathBuf::from(contents.trim());
                    let path = if path.is_absolute() {
                        path
                    } else {
                        git_dir.join(path)
                    };
                    path.canonicalize().with_context(|| {
                        format!("could not resolve Git common directory {}", path.display())
                    })?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => git_dir.clone(),
                Err(error) => return Err(error).context("could not read Git commondir"),
            };
            let object_format = read_object_format(&common_dir.join("config"))?;
            return Ok(Some(Self {
                root: root.to_path_buf(),
                index_path: git_dir.join("index"),
                objects_dir: common_dir.join("objects"),
                git_dir,
                common_dir,
                object_format,
            }));
        }
        Ok(None)
    }

    fn discover_with_git(start_dir: &Path) -> Result<Self> {
        let discovery = git_stdout(
            start_dir,
            [
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--absolute-git-dir",
                "--show-object-format",
                "--git-common-dir",
                "--git-path",
                "index",
                "--git-path",
                "objects",
            ],
        )
        .context("not inside a non-bare Git working tree")?;
        let mut lines = discovery.lines();
        let root = PathBuf::from(
            lines
                .next()
                .context("Git did not return the working-tree root")?,
        );
        let git_dir = PathBuf::from(
            lines
                .next()
                .context("Git did not return its administrative directory")?,
        );
        let format = lines
            .next()
            .context("Git did not return its object format")?;
        let common_dir = PathBuf::from(
            lines
                .next()
                .context("Git did not return its common directory")?,
        );
        let index_path = PathBuf::from(lines.next().context("Git did not return its index path")?);
        let objects_dir = PathBuf::from(
            lines
                .next()
                .context("Git did not return its object directory")?,
        );
        if lines.next().is_some() {
            bail!("Git returned unexpected repository discovery output");
        }
        let object_format = match format {
            "sha1" => ObjectFormat::Sha1,
            "sha256" => ObjectFormat::Sha256,
            other => bail!("unsupported Git object format: {other}"),
        };
        Ok(Self {
            root,
            git_dir,
            common_dir,
            index_path,
            objects_dir,
            object_format,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn object_format(&self) -> ObjectFormat {
        self.object_format
    }

    pub(crate) fn ensure_supported_state(&self) -> Result<()> {
        for state in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge",
            "rebase-apply",
        ] {
            let path = self.git_dir.join(state);
            if path.exists() {
                bail!(
                    "a Git operation is in progress ({state}); finish or abort it before running shasha"
                );
            }
        }
        Ok(())
    }

    pub(crate) fn head(&self) -> Result<Head> {
        let contents =
            fs::read_to_string(self.git_dir.join("HEAD")).context("could not read Git HEAD")?;
        let value = contents.trim();
        if let Some(reference) = value.strip_prefix("ref: ") {
            let oid = if std::env::var_os("GIT_NAMESPACE").is_none()
                && !self.common_dir.join("reftable").exists()
            {
                self.resolve_file_reference(reference, 0)?
            } else {
                self.git_head_oid()?
            };
            Ok(Head {
                reference: reference.to_owned(),
                oid,
            })
        } else if value.len() == self.object_format.oid_hex_len()
            && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            Ok(Head {
                reference: "HEAD".to_owned(),
                oid: Some(value.to_ascii_lowercase()),
            })
        } else {
            bail!("Git HEAD contains an invalid target")
        }
    }

    pub(crate) fn head_reference(&self) -> Result<String> {
        let contents =
            fs::read_to_string(self.git_dir.join("HEAD")).context("could not re-read Git HEAD")?;
        Ok(contents
            .trim()
            .strip_prefix("ref: ")
            .unwrap_or("HEAD")
            .to_owned())
    }

    pub(crate) fn identities(&self) -> Result<Identities> {
        let variables = self
            .git_stdout(["var", "-l"])
            .context("could not determine commit identities")?;
        let mut author = None;
        let mut committer = None;
        for line in variables.lines() {
            if let Some(value) = line.strip_prefix("GIT_AUTHOR_IDENT=") {
                author = Some(value.to_owned());
            } else if let Some(value) = line.strip_prefix("GIT_COMMITTER_IDENT=") {
                committer = Some(value.to_owned());
            }
        }
        Ok(Identities {
            author: author.context("Git did not return GIT_AUTHOR_IDENT")?,
            committer: committer.context("Git did not return GIT_COMMITTER_IDENT")?,
        })
    }

    pub(crate) fn stage_file(&self, file: &Path) -> Result<()> {
        let output = Command::new("git")
            .current_dir(&self.root)
            .args([OsStr::new("add"), OsStr::new("-f"), OsStr::new("--")])
            .arg(file)
            .output()
            .context("could not execute git add")?;
        ensure_success(output, "git add")?;
        Ok(())
    }

    pub(crate) fn lock_index(&self) -> Result<IndexLock> {
        let mut lock_name = self.index_path.as_os_str().to_owned();
        lock_name.push(".lock");
        let path = PathBuf::from(lock_name);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| {
                format!(
                    "could not lock the Git index at {}; another Git process may be running",
                    path.display()
                )
            })?;
        Ok(IndexLock {
            file: Some(file),
            path,
        })
    }

    pub(crate) fn write_tree(&self, _lock: &IndexLock) -> Result<String> {
        let parent = self
            .index_path
            .parent()
            .context("the Git index path has no parent directory")?;
        let temporary =
            NamedTempFile::new_in(parent).context("could not create a temporary Git index")?;
        fs::copy(&self.index_path, temporary.path()).with_context(|| {
            format!("could not copy the Git index {}", self.index_path.display())
        })?;
        let temporary_path = temporary.into_temp_path();
        let output = Command::new("git")
            .current_dir(&self.root)
            .env("GIT_INDEX_FILE", &temporary_path)
            .arg("write-tree")
            .output()
            .context("could not execute git write-tree")?;
        ensure_success(output, "git write-tree").context("could not write the staged Git tree")
    }

    pub(crate) fn object_prefix_exists(&self, prefix: &str) -> Result<bool> {
        if let Some(exists) = self.loose_object_prefix_exists(prefix)? {
            return Ok(exists);
        }
        let argument = format!("--disambiguate={prefix}");
        let output = self.git_output(["rev-parse", argument.as_str()])?;
        if !output.status.success() {
            return Ok(false);
        }
        Ok(!trim_stdout(&output)?.is_empty())
    }

    fn loose_object_prefix_exists(&self, prefix: &str) -> Result<Option<bool>> {
        if std::env::var_os("GIT_ALTERNATE_OBJECT_DIRECTORIES").is_some()
            || self.objects_dir.join("info/alternates").exists()
        {
            return Ok(None);
        }

        let pack_dir = self.objects_dir.join("pack");
        match fs::read_dir(&pack_dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.with_context(|| {
                        format!(
                            "could not inspect object pack directory {}",
                            pack_dir.display()
                        )
                    })?;
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.ends_with(".idx") || name == "multi-pack-index" {
                        return Ok(None);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "could not inspect object pack directory {}",
                        pack_dir.display()
                    )
                });
            }
        }

        let directory = self.objects_dir.join(&prefix[..2]);
        let suffix = &prefix[2..];
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Some(false)),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("could not inspect loose objects in {}", directory.display())
                });
            }
        };
        for entry in entries {
            let name = entry?.file_name();
            let name = name.to_string_lossy();
            if name.len() == self.object_format.oid_hex_len() - 2
                && name.as_bytes().iter().all(u8::is_ascii_hexdigit)
                && name.starts_with(suffix)
            {
                return Ok(Some(true));
            }
        }
        Ok(Some(false))
    }

    pub(crate) fn write_commit_object(&self, body: &[u8], expected_oid: &str) -> Result<String> {
        let mut object = format!("commit {}\0", body.len()).into_bytes();
        object.extend_from_slice(body);
        let computed_oid = match self.object_format {
            ObjectFormat::Sha1 => hex_encode(&sha1::Sha1::digest(&object)),
            ObjectFormat::Sha256 => hex_encode(&sha2::Sha256::digest(&object)),
        };
        if computed_oid != expected_oid {
            bail!(
                "the object writer computed {computed_oid}, but the miner computed {expected_oid}"
            );
        }

        if let Err(error) = self.write_loose_object(&object, expected_oid) {
            // Nonstandard filesystems or permission setups still get Git's
            // mature object writer as a compatibility fallback.
            return self
                .write_commit_object_with_git(body)
                .with_context(|| format!("direct object write also failed: {error:#}"));
        }
        Ok(expected_oid.to_owned())
    }

    fn write_commit_object_with_git(&self, body: &[u8]) -> Result<String> {
        let mut child = Command::new("git")
            .current_dir(&self.root)
            .args(["hash-object", "-t", "commit", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("could not execute git hash-object")?;
        child
            .stdin
            .take()
            .context("could not open git hash-object stdin")?
            .write_all(body)
            .context("could not send the mined commit to Git")?;
        let output = child
            .wait_with_output()
            .context("could not wait for git hash-object")?;
        ensure_success(output, "git hash-object")
    }

    pub(crate) fn update_reference(
        &self,
        reference: &str,
        new_oid: &str,
        old_oid: Option<&str>,
        message: &str,
    ) -> Result<()> {
        let zero_oid = "0".repeat(self.object_format.oid_hex_len());
        let expected = old_oid.unwrap_or(&zero_oid);
        let subject = message.lines().next().unwrap_or("shasha commit");
        let reflog = format!("commit: {subject}");
        let output = Command::new("git")
            .current_dir(&self.root)
            .args(["update-ref", "-m", &reflog, reference, new_oid, expected])
            .output()
            .context("could not execute git update-ref")?;
        ensure_success(output, "git update-ref")?;
        Ok(())
    }

    fn git_stdout<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        git_stdout(&self.root, args)
    }

    fn git_head_oid(&self) -> Result<Option<String>> {
        let output = self.git_output(["rev-parse", "--verify", "HEAD"])?;
        if output.status.success() {
            Ok(Some(trim_stdout(&output)?))
        } else {
            Ok(None)
        }
    }

    fn resolve_file_reference(&self, reference: &str, depth: u8) -> Result<Option<String>> {
        if depth >= 8 {
            bail!("Git reference chain is too deep");
        }

        for base in [&self.git_dir, &self.common_dir] {
            let path = base.join(reference);
            match fs::read_to_string(&path) {
                Ok(contents) => {
                    let value = contents.trim();
                    if let Some(next) = value.strip_prefix("ref: ") {
                        return self.resolve_file_reference(next, depth + 1);
                    }
                    return Ok(Some(self.validate_oid(value, &path)?));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("could not read Git reference {}", path.display())
                    });
                }
            }
        }

        let packed_refs = self.common_dir.join("packed-refs");
        match fs::read_to_string(&packed_refs) {
            Ok(contents) => {
                for line in contents.lines() {
                    if line.starts_with(['#', '^']) {
                        continue;
                    }
                    if let Some((oid, name)) = line.split_once(' ')
                        && name == reference
                    {
                        return Ok(Some(self.validate_oid(oid, &packed_refs)?));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "could not read packed references at {}",
                        packed_refs.display()
                    )
                });
            }
        }
        Ok(None)
    }

    fn validate_oid(&self, oid: &str, source: &Path) -> Result<String> {
        if oid.len() != self.object_format.oid_hex_len()
            || !oid.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("invalid object ID in {}", source.display());
        }
        Ok(oid.to_ascii_lowercase())
    }

    fn write_loose_object(&self, object: &[u8], oid: &str) -> Result<()> {
        let directory = self.objects_dir.join(&oid[..2]);
        fs::create_dir_all(&directory).with_context(|| {
            format!("could not create object directory {}", directory.display())
        })?;
        copy_directory_permissions(&self.objects_dir, &directory)?;

        let destination = directory.join(&oid[2..]);
        if destination.exists() {
            return Ok(());
        }

        let temporary = NamedTempFile::new_in(&directory).with_context(|| {
            format!(
                "could not create a temporary object in {}",
                directory.display()
            )
        })?;
        let mut encoder = ZlibEncoder::new(temporary, Compression::fast());
        encoder
            .write_all(object)
            .context("could not compress the commit object")?;
        let temporary = encoder
            .finish()
            .context("could not finish the commit object")?;
        make_object_read_only(temporary.path())?;

        match temporary.persist_noclobber(&destination) {
            Ok(_) => Ok(()),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error.error)
                .with_context(|| format!("could not install Git object {}", destination.display())),
        }
    }

    fn git_output<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .output()
            .context("could not execute Git")
    }
}

fn read_object_format(config_path: &Path) -> Result<ObjectFormat> {
    let config = fs::read_to_string(config_path)
        .with_context(|| format!("could not read Git config {}", config_path.display()))?;
    let mut section = String::new();
    let mut format = None;
    for raw_line in config.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1]
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches('"')
                .to_ascii_lowercase();
            continue;
        }
        if section != "extensions" || line.starts_with(['#', ';']) || line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .or_else(|| line.split_once(char::is_whitespace))
            .unwrap_or((line, ""));
        if key.trim().eq_ignore_ascii_case("objectformat") {
            format = Some(value.trim().trim_matches('"').to_ascii_lowercase());
        }
    }
    match format.as_deref() {
        None | Some("sha1") => Ok(ObjectFormat::Sha1),
        Some("sha256") => Ok(ObjectFormat::Sha256),
        Some(other) => bail!("unsupported Git object format: {other}"),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(unix)]
fn copy_directory_permissions(source: &Path, destination: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(source)
        .with_context(|| format!("could not inspect {}", source.display()))?
        .permissions()
        .mode();
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))
        .with_context(|| format!("could not set permissions on {}", destination.display()))
}

#[cfg(not(unix))]
fn copy_directory_permissions(_source: &Path, _destination: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn make_object_read_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o444))
        .with_context(|| format!("could not set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn make_object_read_only(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("could not set permissions on {}", path.display()))
}

fn git_stdout<I, S>(directory: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()
        .context("could not execute Git")?;
    ensure_success(output, "git")
}

fn ensure_success(output: Output, operation: &str) -> Result<String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{operation} failed: {}", stderr.trim());
    }
    trim_stdout(&output)
}

fn trim_stdout(output: &Output) -> Result<String> {
    let stdout = std::str::from_utf8(&output.stdout).context("Git returned non-UTF-8 output")?;
    Ok(stdout.trim().to_owned())
}
