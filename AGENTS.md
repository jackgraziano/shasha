# Instructions for coding agents

These instructions apply to the entire repository.

## Project mission

Shasha is an external Git command. It creates a normal Git commit whose tracked
`.shasha` file contains a leading hexadecimal prefix of that same commit's
object ID. Preserve this invariant in every code path.

The long-term direction is to make `git shasha` a progressively broader
substitute for `git commit`. Compatibility must be added deliberately, one Git
behavior or option at a time, with tests. Do not blindly forward unknown options
to `git commit`: Shasha constructs and mines the commit itself, so it must know
about anything that changes the tree, parents, identities, message, signature,
hooks, or ref update.

Likely compatibility priorities include:

1. the configured editor, templates, and message cleanup;
2. `-a`/`--all`, `--author`, `--date`, `--signoff`, and trailers;
3. `--amend`, `--no-edit`, message reuse, fixup, and squash workflows;
4. standard commit hooks and their bypass options;
5. pathspec, interactive, merge, cherry-pick, revert, and rebase behavior;
6. signed commits, which require a design compatible with mining.

Treat Git's documented and observable behavior as the compatibility reference.
Keep the README's supported-options and limitations sections accurate whenever
behavior changes.

## Mandatory commit workflow

Every commit in this repository must be created with Shasha. Never use
`git commit`, including for documentation-only or maintenance commits.

When a task calls for a commit:

1. run the required checks;
2. build the current release executable with `cargo build --release`;
3. stage only the intended files with `git add`;
4. run the freshly built extension:

   ```sh
   PATH="$PWD/target/release:$PATH" git shasha -m "Describe the change"
   ```

The release binary is intentional: an older globally installed `git-shasha`
may not reflect changes in the checkout, and debug mining is much slower.

Do not edit `.shasha` manually. `git shasha` generates and stages it. After a
commit, verify that the value returned by `git show HEAD:.shasha` is a prefix of
`git rev-parse HEAD`, then run `git fsck --strict`. The default prefix length is
currently six characters.

Do not push, force-push, tag, or publish unless the task explicitly requests it.

## Publishing through protected `main`

The `main` branch requires the exact commit being pushed to have already passed
the Linux, macOS, and Windows checks. When a task explicitly requests a push:

1. push `HEAD` to a short-lived validation branch, for example:

   ```sh
   git push origin HEAD:refs/heads/validate/<topic>
   ```

2. open a pull request from the validation branch to `main` so that GitHub runs
   CodeQL against the exact mined commit;
3. wait for the Linux, macOS, Windows, and CodeQL checks on that exact commit to
   succeed;
4. fast-forward the same commit to `main`:

   ```sh
   git push origin HEAD:main
   ```

5. close the pull request without merging it, then delete the remote validation
   branch after the protected push succeeds.

Do not use GitHub's merge, squash, or rebase buttons for this repository. Each
of those operations can create a different commit ID without remining it, which
would make `.shasha` incorrect. Pull requests may still be used for discussion,
but their reviewed commit must reach `main` unchanged through a fast-forward.

## Required validation

Before committing, run the same core checks as CI:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo build --release
```

Changes to commit serialization, index handling, refs, repository discovery, or
platform-specific paths require an integration test that has Git parse the
result and, where applicable, validates it with `git fsck --strict`. Keep the
Linux, macOS, and Windows CI matrix working. A local pass on one platform is not
enough evidence for platform-sensitive filesystem or process code.

Avoid performance tests with a fixed absolute hash-rate threshold. Throughput
depends on CPU, object format, build profile, and worker count. Verify digest
correctness independently and benchmark relative changes when optimizing.

## Architecture and invariants

- `src/cli.rs` owns the shared command-line behavior for both executables.
- `src/bin/git-shasha.rs` provides the canonical `git shasha` extension.
- `src/main.rs` provides the standalone `shasha` compatibility command.
- `src/lib.rs` coordinates commit creation and exposes the library API.
- `src/git.rs` owns repository discovery, index/object/ref operations, and Git
  subprocesses.
- `src/miner.rs` is the performance-sensitive portable mining implementation.
- `src/sha1_arm.rs` is the AArch64 SHA-1 acceleration backend.

Preserve these behavioral guarantees:

- `.shasha` is tracked in the mined tree and equals the requested leading
  prefix of the resulting commit ID;
- SHA-1 and SHA-256 repositories remain supported;
- files other than `.shasha` are committed only according to the selected index
  semantics;
- the winning object is independently verified before moving a ref;
- ref updates remain atomic and reject a concurrently changed `HEAD`;
- unusual repository layouts, packed refs and objects, linked worktrees, and
  platform path conventions remain supported;
- the mining hot loop must not spawn a Git process per candidate.

Prefer focused changes that preserve the boundary between Git-compatible
porcelain behavior and the optimized in-memory mining core.
