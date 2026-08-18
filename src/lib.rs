mod git;
mod miner;
#[cfg(target_arch = "aarch64")]
mod sha1_arm;

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::git::Repository;
use crate::miner::{MineRequest, mine};

pub use crate::git::ObjectFormat;

const NONCE_LABEL: &str = "Shasha-Nonce: ";
const NONCE_HEX_LEN: usize = 16;

#[derive(Debug, Clone)]
pub struct CommitOptions {
    pub message: String,
    pub version_file: PathBuf,
    pub prefix_len: u8,
    pub threads: usize,
}

impl Default for CommitOptions {
    fn default() -> Self {
        Self {
            message: String::new(),
            version_file: PathBuf::from(".shasha"),
            prefix_len: 5,
            threads: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommitOutcome {
    pub oid: String,
    pub prefix: String,
    pub nonce: u64,
    pub attempts: u64,
    pub elapsed: std::time::Duration,
    pub reference: String,
    pub version_file: PathBuf,
}

pub fn create_commit(start_dir: &Path, options: &CommitOptions) -> Result<CommitOutcome> {
    validate_options(options)?;

    let repo = Repository::discover(start_dir)?;
    repo.ensure_supported_state()?;

    let version_path = repo.root().join(&options.version_file);
    ensure_no_symlink_components(repo.root(), &options.version_file)?;
    if let Some(parent) = version_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "could not create the parent directory for {}",
                options.version_file.display()
            )
        })?;
    }

    let head = repo.head()?;
    let reference = head.reference;
    let old_oid = head.oid;
    let seed = target_seed(old_oid.as_deref(), &options.message)?;

    let (target, prefix) = select_target(&repo, options.prefix_len, &seed)?;
    fs::write(&version_path, format!("{prefix}\n")).with_context(|| {
        format!(
            "could not write version file {}",
            options.version_file.display()
        )
    })?;
    repo.stage_file(&options.version_file)?;
    let index_lock = repo.lock_index()?;
    let tree_oid = repo.write_tree(&index_lock)?;

    let identities = repo.identities()?;
    let body_prefix = commit_body_prefix(
        &tree_oid,
        old_oid.as_deref(),
        &identities.author,
        &identities.committer,
        &options.message,
    );

    let body_len = body_prefix.len() + NONCE_HEX_LEN + 1;
    let mut object_prefix = format!("commit {body_len}\0").into_bytes();
    object_prefix.extend_from_slice(&body_prefix);

    let mined = mine(MineRequest {
        format: repo.object_format(),
        object_prefix: &object_prefix,
        target,
        prefix_len: options.prefix_len,
        threads: options.threads,
    })?;

    let body = finish_commit_body(body_prefix, mined.nonce);
    let current_reference = repo.head_reference()?;
    if current_reference != reference {
        bail!("HEAD changed while mining; no commit was published");
    }
    let stored_oid = repo.write_commit_object(&body, &mined.oid)?;
    if stored_oid != mined.oid {
        bail!(
            "Git computed {stored_oid}, but the miner computed {}; refusing to move HEAD",
            mined.oid
        );
    }
    if !stored_oid.starts_with(&prefix) {
        bail!(
            "the stored commit {stored_oid} does not start with the value written to {}",
            options.version_file.display()
        );
    }

    repo.update_reference(
        &reference,
        &stored_oid,
        old_oid.as_deref(),
        &options.message,
    )?;

    Ok(CommitOutcome {
        oid: stored_oid,
        prefix,
        nonce: mined.nonce,
        attempts: mined.attempts,
        elapsed: mined.elapsed,
        reference,
        version_file: options.version_file.clone(),
    })
}

fn validate_options(options: &CommitOptions) -> Result<()> {
    if options.message.trim().is_empty() {
        bail!("the commit message cannot be empty");
    }
    if options.message.as_bytes().contains(&0) {
        bail!("the commit message cannot contain a NUL byte");
    }
    if !(1..=8).contains(&options.prefix_len) {
        bail!("the prefix length must be between 1 and 8 hexadecimal characters");
    }
    if options.threads == 0 {
        bail!("the thread count must be at least 1");
    }
    validate_version_file(&options.version_file)
}

fn validate_version_file(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("the version file must be a non-empty repository-relative path");
    }

    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!("the version file must not contain '.', '..', or a path root");
        }
    }

    if path
        .components()
        .next()
        .is_some_and(|part| part.as_os_str() == ".git")
    {
        bail!("the version file cannot be inside .git");
    }

    Ok(())
}

fn ensure_no_symlink_components(root: &Path, relative: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "the version file path contains a symbolic link: {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("could not inspect version path {}", current.display())
                });
            }
        }
    }
    Ok(())
}

fn target_seed(parent: Option<&str>, message: &str) -> Result<Vec<u8>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("the system clock is before the Unix epoch")?;
    let mut seed = Vec::new();
    seed.extend_from_slice(parent.unwrap_or("root").as_bytes());
    seed.push(0);
    seed.extend_from_slice(message.as_bytes());
    seed.extend_from_slice(&now.as_nanos().to_be_bytes());
    seed.extend_from_slice(&process::id().to_be_bytes());
    Ok(seed)
}

fn select_target(repo: &Repository, prefix_len: u8, seed: &[u8]) -> Result<(u32, String)> {
    for salt in 0_u64.. {
        let target = derive_target(seed, salt, prefix_len);
        let prefix = format!("{target:0width$x}", width = usize::from(prefix_len));

        // Git only supports --disambiguate for prefixes of at least four digits.
        if prefix_len < 4 || !repo.object_prefix_exists(&prefix)? {
            return Ok((target, prefix));
        }
    }

    unreachable!("the u64 target salt space was exhausted")
}

fn derive_target(seed: &[u8], salt: u64, prefix_len: u8) -> u32 {
    let mut hasher = Sha256::new();
    hasher.update(seed);
    hasher.update(salt.to_be_bytes());
    let digest = hasher.finalize();
    let first = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    first >> (32 - u32::from(prefix_len) * 4)
}

fn commit_body_prefix(
    tree_oid: &str,
    parent_oid: Option<&str>,
    author: &str,
    committer: &str,
    message: &str,
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("tree {tree_oid}\n").as_bytes());
    if let Some(parent_oid) = parent_oid {
        body.extend_from_slice(format!("parent {parent_oid}\n").as_bytes());
    }
    body.extend_from_slice(format!("author {author}\n").as_bytes());
    body.extend_from_slice(format!("committer {committer}\n\n").as_bytes());
    body.extend_from_slice(message.trim_end_matches('\n').as_bytes());
    body.extend_from_slice(b"\n\n");
    body.extend_from_slice(NONCE_LABEL.as_bytes());
    body
}

fn finish_commit_body(mut body_prefix: Vec<u8>, nonce: u64) -> Vec<u8> {
    body_prefix.extend_from_slice(&miner::encode_nonce(nonce));
    body_prefix.push(b'\n');
    body_prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_has_requested_width() {
        let target = derive_target(b"seed", 0, 5);
        assert!(target < 16_u32.pow(5));
    }

    #[test]
    fn commit_body_has_nonce_at_the_end() {
        let prefix = commit_body_prefix(
            "tree-id",
            Some("parent-id"),
            "A <a@example.com> 1 +0000",
            "C <c@example.com> 2 +0000",
            "subject",
        );
        let body = finish_commit_body(prefix, 0x2a);
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("parent parent-id\n"));
        assert!(text.ends_with("Shasha-Nonce: 000000000000002a\n"));
    }

    #[test]
    fn rejects_paths_outside_the_repository() {
        assert!(validate_version_file(Path::new("../.shasha")).is_err());
        assert!(validate_version_file(Path::new("/.shasha")).is_err());
        assert!(validate_version_file(Path::new(".git/shasha")).is_err());
        assert!(validate_version_file(Path::new("metadata/.shasha")).is_ok());
    }
}
