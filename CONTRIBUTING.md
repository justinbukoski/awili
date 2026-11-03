# Contributing

Thanks for your interest. This is a small project run by one person, so please
open an issue before starting anything large.

- Build and test: `cargo build -p awili-app` and `cargo test --workspace`
  (see the README for the toolchain requirements).
- Format and lint: `cargo fmt --all` and `cargo clippy --workspace`.
- Keep changes focused, add a test where behaviour changes, and do not add
  telemetry or network calls that the user has not configured.
- By submitting a contribution you agree it is licensed under the same terms as
  the project: MIT OR Apache-2.0.
