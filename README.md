# starjj

A fast [jujutsu](https://github.com/jj-vcs/jj) prompt segment for
[Starship](https://starship.rs). Shows ancestor bookmarks (with `⇡N`
ahead-counts), state warnings (`(CONFLICT)`, `(DIVERGENT)`, `(HIDDEN)`,
`(IMMUTABLE)`, `(EMPTY)`), and diff metrics (`[changed +added-removed]`).

## Usage

Add to `starship.toml`:

```toml
[custom.jj]
command = "prompt"
format = "$output"
ignore_timeout = true
shell = ["starjj"]
use_stdin = false
when = true
```

## Builds

Release binaries for `x86_64`/`aarch64` Linux and `aarch64` macOS are built on
native GitHub Actions runners (`.github/workflows/release.yml`).

## Releasing

- Bump `version` in `Cargo.toml`.
- Tag it: `git tag vX.Y.Z && git push origin vX.Y.Z`.
- The `release` workflow builds all three targets and attaches them to the GitHub release.
