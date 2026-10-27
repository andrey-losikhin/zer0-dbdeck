# Contributing

Contributions and focused bug reports are welcome. This is a personal project;
there is no guaranteed review or support response time.

## Before starting

Read the [README](README.md), [architecture](docs/ARCHITECTURE.md),
[testing guide](docs/TESTING.md) and [code of conduct](CODE_OF_CONDUCT.md).
Open an issue before a large redesign, a new database driver or a storage-format
change. Small fixes can go straight to a pull request.

## Development

Use Rust stable and the native development libraries listed in the README.
Run the checks from a checkout with `Cargo.lock` intact:

```sh
cargo fmt --all -- --check
cargo check --locked
cargo test --locked
cargo clippy --all-targets --locked -- -D clippy::correctness
node --test tests/grid_logic.test.cjs
```

For database changes, run relevant integration tests against local fixtures.
For UI changes, use the isolated headless runner; do not test against a production
connection. Keep unrelated formatting/refactoring out of a behavioral fix.

## Pull requests

- Keep changes focused and explain the problem, behavior and validation.
- Include a regression test for data integrity, security or query behavior changes.
- Preserve dialect differences, transaction boundaries, cancellation and stale-event guards.
- Bind SQL values; quote identifiers with the engine-specific helpers.
- Keep secrets out of arguments, diagnostics, screenshots, fixtures and Git history.
- Do not skip failing tests or suppress checks to make a change pass.
- Update user documentation for changed behavior and add an Unreleased changelog entry.
- Use descriptive commits such as `fix: preserve SQL result selection after refresh`.

Do not add a dependency, cloud service, telemetry or a stored-data migration
without explaining why it is necessary. Changes to encrypted storage need migration
and failure-path tests. Add keyboard access for new interactive actions.

## Licensing

By submitting a contribution, you agree that your original contribution is
licensed under Apache-2.0, as provided in section 5 of [LICENSE](LICENSE).
No separate CLA is currently required. Preserve third-party notices and identify
any externally sourced code/assets and their licenses in your pull request.
