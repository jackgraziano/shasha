# Contributing

Thank you for helping improve Shasha.

## Development setup

Install a current stable Rust toolchain and Git, then run:

```sh
cargo test --all-targets
```

Before opening a pull request, run the complete local checks:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Changes to commit serialization or reference updates should include an
integration test that lets Git parse and verify the resulting object. Avoid
benchmarks that depend on a specific absolute hash rate; candidate throughput
varies substantially across machines and object formats.

## Reporting problems

Include the Shasha version, `git --version`, operating system, object format
from `git rev-parse --show-object-format`, and the complete error message.
Never attach a repository containing secrets solely to reproduce a bug.

