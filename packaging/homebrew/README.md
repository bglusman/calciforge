# Homebrew Packaging

Calciforge's Homebrew path is a binary formula, not a source build. Operators
should not need Rust just to install Calciforge.

The formula also depends on Homebrew's `fnox` package and defines a
`brew services` entry for supervising Calciforge with the config file at
`$(brew --prefix)/etc/calciforge/config.toml`. It does not replace the
source-tree installer for managed agent wiring, certificate setup, or full
secrets bootstrap.

The service runs with a Homebrew-oriented `PATH` so helper binaries installed by
the formula, including `calciforge-secrets` and Homebrew's `fnox`, are visible to
the supervised Calciforge process.

The release flow is:

1. Build platform archives with `scripts/build-dist-archive.sh`.
2. Publish the archives on a GitHub release.
3. Render the formula with `scripts/render-homebrew-formula.sh`.
4. Copy the rendered `calciforge.rb` into the Homebrew tap.

Homebrew 5.x rejects installing arbitrary formula files directly from `/tmp` or
another non-tap path. For local release-candidate testing, copy the rendered
formula into a test tap and install it by tap name, for example
`brew install local/calciforge-test/calciforge`.

The `Release Packaging` workflow automates steps 1 and 2 for `v*` tags and
also exposes a manual run for release-candidate artifact checks.

Example:

```bash
scripts/build-dist-archive.sh 0.1.0 aarch64-apple-darwin
scripts/render-homebrew-formula.sh \
  --version 0.1.0 \
  --mac-arm64-sha256 "$(cat dist/calciforge-0.1.0-aarch64-apple-darwin.tar.gz.sha256)" \
  --mac-intel-sha256 "<x86_64-apple-darwin-sha256>" \
  --linux-amd64-sha256 "<x86_64-unknown-linux-gnu-sha256>"
```

`scripts/check-packaging.sh` renders the template with dummy checksums and runs
Ruby syntax validation. It does not prove that release URLs exist.
