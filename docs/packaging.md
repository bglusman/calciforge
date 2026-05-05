---
layout: default
title: Packaging and Install Options
---

# Packaging and Install Options

Calciforge supports four install shapes. They serve different audiences and
should not be mixed up in docs or support notes.

## Source Installer

Use this when developing Calciforge or testing current `main`:

```bash
git clone https://github.com/bglusman/calciforge
cd calciforge
bash scripts/install.sh
```

This path builds from source, manages launchd/systemd services, configures local
state, and can wire supported agents. It requires Rust.

## Homebrew Binary Formula

Use this for normal macOS installs once release archives are published. The
formula installs released binaries; it does not build from source.

Packaging maintainers render the formula from:

```bash
scripts/render-homebrew-formula.sh --help
```

This is currently a binary packaging path, not the full managed service
installer. Use it for manual service-manager setups, Docker image assembly, and
smoke testing. Homebrew-managed launchd service wiring is a follow-up item; the
source installer remains the managed path for local services and agent wiring.

## Docker Compose

Use this for trials, LAN staging, and repeatable smoke environments:

```bash
cd packaging/docker
cp calciforge.env.example .env
mkdir -p data
docker compose --env-file .env up --build
```

The Compose example runs Calciforge, `security-proxy`, and `clashd` from the
same image. It is separate from `scripts/docker-compose.yml`, which remains the
CI/mock-LLM smoke stack.

## Manual Release Archives

Release operators can build tarballs with:

```bash
scripts/build-dist-archive.sh
```

The archive layout is the same one consumed by the Homebrew formula template:
core binaries under `bin/`, plus license/readme metadata.
