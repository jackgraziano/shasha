# shasha

Shasha is an external Git command that creates self-identifying commits.

When Shasha creates a commit, it writes an abbreviated form of that commit's
object ID to `.shasha` **inside the same commit**. With the default five-character
prefix, the result has this invariant:

```text
$ git show HEAD:.shasha
7e3a1

$ git rev-parse HEAD
7e3a1e4...
```

The value stored in `.shasha` is a prefix, not the complete object ID. The full
object ID remains Git's authoritative identifier.

## Quick start

Install the extension from this checkout:

```sh
cargo install --path .
```

For an ordinary commit of already-staged changes, use `git shasha -m` in place
of `git commit -m`:

```text
$ git add src/
$ git shasha -m "Add the landing page"
[main 7e3a1] Add the landing page
mined 934201 candidates in 5.2ms (179.7 MH/s)
.shasha contains 7e3a1; full commit is 7e3a1e4...
```

No Git alias, hook, or patched Git installation is required.

## Why `git shasha` works

Git has a standard mechanism for external commands. When it receives an unknown
subcommand, it looks for an executable named `git-<subcommand>` on `PATH`:

```text
git shasha -m "Message"
    └── executes git-shasha -m "Message"
```

The Shasha package therefore installs two executables:

- `git-shasha`: the Git extension, invoked as `git shasha`;
- `shasha`: a standalone compatibility command with identical behavior.

`git shasha` is the canonical interface. After installation, these commands
should succeed:

```sh
git shasha --version
git shasha --help
```

Cargo normally installs executables in `~/.cargo/bin`. That directory must be
on `PATH` so Git can discover `git-shasha`.

## Commit workflow

Shasha supports a subset of `git commit`'s index-based workflow. For an
ordinary commit, stage changes first and provide the message with `-m` or `-F`:

```sh
git add src Cargo.toml
git shasha -m "Describe the change"
```

Only already-staged changes are committed, with one exception: Shasha generates
and force-adds `.shasha`, even when an ignore rule matches it. The file is meant
to be tracked and updated by every commit created with Shasha.

Running an ordinary `git commit` does not invoke Shasha and does not update
`.shasha`. Shasha is currently a separate commit implementation, not a wrapper
around `git commit` and not a pre-commit hook.

Shasha does not pass command-line options through to `git commit`, because it
does not invoke `git commit`. Unsupported options such as `--amend`, `-a`,
`--author`, `--signoff`, `--allow-empty`, and `-S` are rejected rather than
silently ignored.

Multiple `-m` arguments create separate message paragraphs:

```sh
git shasha -m "Subject" -m "Longer explanation"
```

A message can also come from a file or standard input:

```sh
git shasha -F message.txt
printf '%s\n' "Message from stdin" | git shasha -F -
```

Supported Shasha options:

```text
-m, --message <MESSAGE>  Commit message; repeat to create paragraphs
-F, --file <PATH>        Read the commit message from a file, or '-' for stdin
    --sha-file <PATH>    Version file relative to the repository root [default: .shasha]
    --length <N>         Number of hexadecimal prefix characters to mine [default: 5]
    --threads <N>        Mining threads [default: available parallelism]
```

## Status and limitations

Shasha is an early, working implementation. Use it on repositories whose
history you can recover until the project has had wider review.

Currently supported:

- SHA-1 and SHA-256 Git repositories;
- initial and ordinary single-parent commits;
- staged changes;
- configurable prefix length, output file, and worker count;
- linked worktrees and packed refs;
- atomic branch and detached-HEAD updates.

Shasha currently refuses to run during merges, rebases, cherry-picks, and
reverts. It does not run Git commit hooks and does not create signed commits.
Sign an annotated tag if the resulting revision needs a signature.

The result is otherwise a normal Git commit: Git stores it, verifies it, and
addresses it by its complete object ID.

## How it works

Shasha first chooses a target prefix and writes it to `.shasha`. It then asks
Git to create the staged tree. That fixes the tree ID, parent, identities,
timestamps, and commit message except for a fixed-width trailer:

```text
Shasha-Nonce: 00000000000f28a3
```

Workers vary this nonce and hash candidate commit objects in memory. Shasha
publishes a commit only after finding an object ID that starts with the value
already placed in `.shasha`.

Before moving `HEAD`, Shasha recomputes the winning object through an independent
digest path and stores a standard Git commit object. It updates the current ref
with `git update-ref`, using the previous object ID as an atomic compare-and-swap
guard.

The mining loop does not launch a Git process for every candidate. It
pre-compresses the constant hash blocks, changes only the final block for each
fixed-width nonce, and uses all available CPU workers by default. On AArch64
processors it detects and uses the ARMv8 SHA instructions at runtime; other CPUs
use the portable RustCrypto backend.

## Prefix length and cost

The default prefix is five hexadecimal characters, or 20 bits. It takes
1,048,576 candidate hashes on average. Each additional character multiplies the
expected work by 16.

| Characters | Expected candidates |
| ---: | ---: |
| 4 | 65,536 |
| 5 | 1,048,576 |
| 6 | 16,777,216 |
| 7 | 268,435,456 |
| 8 | 4,294,967,296 |

Five characters are convenient but not globally unique. Shasha avoids a prefix
already used by an object at creation time, but a future object can still
acquire the same prefix. Git may then require more characters to disambiguate
the revision; the full object ID remains authoritative.

## Development

Build without installing and expose the development binary to Git:

```sh
cargo build
PATH="$PWD/target/debug:$PATH" git shasha --help
```

Run the complete local checks:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```

Integration tests create temporary SHA-1 and, when supported by the installed
Git, SHA-256 repositories. They cover the actual `git shasha` dispatch, packed
objects and refs, linked worktrees, and validate resulting object graphs with
`git fsck --strict`.

## License

MIT. See [LICENSE](LICENSE).
