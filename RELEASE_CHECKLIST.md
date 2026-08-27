# Release Checklist

Use this checklist before publishing a new version to crates.io.

## Pre-Release

- [ ] All tests pass: `cargo test --all-features`
- [ ] All targets compile: `cargo test --no-run --all-features --all-targets`
      (not `cargo check` — it skips `#[cfg(test)]` modules, integration tests,
      examples and benches, several of which carry `required-features`)
- [ ] `legacy-bincode1-read` builds in both directions:
      `cargo test --no-default-features --features in-memory,serde,redis-backend`
      and the same line with `,legacy-bincode1-read` appended
- [ ] `bincode` is absent from the graph: `cargo tree -i bincode --all-features`
      finds nothing
- [ ] `cargo deny check` reports `advisories ok, bans ok, licenses ok, sources ok`
      with an empty `ignore` list
- [ ] No clippy warnings: `cargo clippy --all-features --all-targets -- -D warnings`
- [ ] Documentation builds: `cargo doc --no-deps --all-features`
- [ ] Benchmarks compile: `cargo test --no-run --benches --features in-memory,serde`
      — **run `cargo bench` locally only, and never add it to CI**: Criterion
      executes the whole suite, which takes minutes and produces timings that are
      meaningless on a shared runner
- [ ] README examples are tested and up-to-date
- [ ] CHANGELOG.md is updated with new version and changes
- [ ] Version bumped in Cargo.toml
- [ ] MSRV in README matches Cargo.toml `rust-version` field, including the
      separate 1.88 floor for `redis-backend`
- [ ] All CI checks pass (if applicable)

## Redis Integration Tests

- [ ] Redis tests pass: `REDIS_URL=redis://127.0.0.1:6379/ cargo test --features redis-backend`
- [ ] Redis example runs: `cargo run --example redis_smoke --features redis-backend`
- [ ] Redis smoke test passes: `python3 scripts/redis_smoke.py`

## Documentation

- [ ] Public API has documentation comments
- [ ] Examples compile and run
- [ ] Doc tests pass: `cargo test --doc`
- [ ] CHANGELOG reflects all breaking changes
- [ ] Migration guide added (if breaking changes)

## Release

- [ ] Create git tag: `git tag -a v0.X.Y -m "Release v0.X.Y"`
- [ ] Push tag: `git push origin v0.X.Y`
- [ ] Dry run: `cargo publish --dry-run`, and confirm the packaged `.crate`
      contains no internal planning docs (`PLAN-*.md`)
- [ ] Publish to crates.io: `cargo publish`
- [ ] Verify published docs at docs.rs
- [ ] Create GitHub release with CHANGELOG excerpt
- [ ] Update dependent projects (if any)

## Post-Release

- [ ] Verify crate appears on crates.io
- [ ] Test installation: `cargo new test-project && cd test-project && cargo add tower-http-cache`
- [ ] Check docs.rs build status
- [ ] Announce release (if applicable)
