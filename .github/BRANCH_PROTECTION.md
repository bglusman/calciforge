# Branch Protection

## Protected Branches

- `main` — Protected, requires PR review
- `master` — Protected (legacy), requires PR review

## Required Checks

All PRs must pass:

1. **Format & Clippy** — Code must be formatted and pass linting
2. **Test Suite** — All crate tests must pass
3. **Release Build** — Release binaries must compile
4. **Loom Concurrency Tests** — No data races detected

## PR Requirements

- At least 1 approving review
- All CI checks must pass
- Branch must be up-to-date with `main`

## Setting Up Branch Protection (Repository Admin)

Go to Settings → Branches → Add rule:

1. **Branch name pattern**: `main`
2. **Require pull request reviews before merging**: ✅
   - Required approving reviews: 1
3. **Require status checks to pass**: ✅
   - Search for and select:
     - `fmt-and-clippy`
     - `test` (all matrix jobs)
     - `build`
     - `loom`
4. **Require branches to be up to date before merging**: ✅
5. **Restrict pushes that create files larger than**: 100 MB

Repeat for `master` if still used.
