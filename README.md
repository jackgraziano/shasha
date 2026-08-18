# shasha

`shasha` creates Git commits that contain their own abbreviated object ID.
It writes a candidate prefix to `.shasha`, mines a nonce in the commit message,
and only publishes the commit when its object ID starts with that prefix.

```text
$ git add src/
$ shasha -m "Add the landing page"
[main 7e3a1] Add the landing page
mined 934201 candidates in 5.2ms (179.7 MH/s)
.shasha contains 7e3a1; full commit is 7e3a1e4...

$ cat .shasha
7e3a1
$ git rev-parse HEAD
7e3a1e4...
```

The default prefix is five hexadecimal characters (20 bits), so a commit
requires 1,048,576 candidate hashes on average. The result is a normal Git
commit: Git stores it, verifies it, and addresses it by its full object ID.

## Status

Shasha is an early, working implementation. Use it on repositories whose
history you can recover until the project has had wider review.

Currently supported:

- SHA-1 and SHA-256 Git repositories;
- initial and ordinary single-parent commits;
- staged changes, following the index-based Git workflow;
- configurable prefix length, output file, and worker count;
- linked worktrees and packed refs;
- atomic branch or detached-HEAD updates.

Shasha currently refuses to run during merges, rebases, cherry-picks, and
reverts. It does not run Git commit hooks and does not create signed commits.
Sign an annotated tag if the resulting revision needs a signature.

## Installation

Build from source with a current stable Rust toolchain and Git available on
`PATH`:

```sh
cargo install --path .
```

For development builds:

```sh
cargo build
```

## Usage

Stage the content to commit, then replace `git commit` with `shasha`:

```sh
git add src Cargo.toml
shasha -m "Describe the change"
```

Multiple `-m` arguments become separate paragraphs:

```sh
shasha -m "Subject" -m "Longer explanation"
```

Read a message from a file or standard input:

```sh
shasha -F message.txt
printf '%s\n' "Message from stdin" | shasha -F -
```

Important options:

```text
--sha-file <PATH>  Version file relative to the repository root [default: .shasha]
--length <N>       Number of hexadecimal characters to mine [default: 5]
--threads <N>      Mining threads [default: available parallelism]
```

`.shasha` is intentionally tracked. Shasha force-adds that one generated file to
the index, even if a broad ignore rule would otherwise match it. Other files
are committed only when already staged.

## How it works

Shasha first chooses a five-character target and writes it to `.shasha`. It then
asks Git to create the staged tree. That fixes the tree ID, parent, identities,
timestamps, and commit message except for a fixed-width trailer:

```text
Shasha-Nonce: 00000000000f28a3
```

Workers hash distinct nonce sequences in memory until the commit object ID
starts with the value in `.shasha`. Shasha recomputes the winner through an
independent digest path, atomically stores the standard zlib-compressed loose
object, and falls back to `git hash-object` on unusual permission or filesystem
setups. It moves the current reference with `git update-ref`, using the old
object ID as an atomic compare-and-swap guard.

The mining loop does not launch a Git process per candidate. It pre-compresses
the constant SHA blocks, changes only the final block for each fixed-width
nonce, and uses all available CPU workers by default. On AArch64 processors it
detects and uses the ARMv8 SHA instructions at runtime; other CPUs use the
portable RustCrypto backend.

## Prefix length and cost

Each additional hexadecimal character multiplies the expected work by 16.

| Characters | Expected candidates |
| ---: | ---: |
| 4 | 65,536 |
| 5 | 1,048,576 |
| 6 | 16,777,216 |
| 7 | 268,435,456 |
| 8 | 4,294,967,296 |

Five characters are convenient but not globally unique. Shasha avoids a
prefix already used by an object at creation time, but a future object can
still acquire the same prefix. Git may then require more characters to
disambiguate the revision; the full object ID remains authoritative.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```

Integration tests create temporary SHA-1 and, when supported by the installed
Git, SHA-256 repositories. They cover packed objects and refs, linked worktrees,
and validate the resulting object graphs with `git fsck --strict`.

## License

MIT. See [LICENSE](LICENSE).
