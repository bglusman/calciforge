# NonZeroClawed Installer — Open Questions & Scope Not Yet Implemented

_Captured 2026-03-30 during session 4 kickoff. These were discovered AFTER the session prompt was written._
_Session 4 will NOT cover these. They need to be addressed in follow-on sessions after Opus review._

---

## Things I said that were WRONG and need correcting

### "Custom claws don't need SSH"
I said the installer only needs SSH for NZC and OpenClaw. **This is wrong.**

Even "custom" or externally-hosted claws may need SSH for:
- Creating system users/accounts on the remote host
- Generating and deploying SSH keypairs (stored in vault)
- Installing the clash/permissions layer
- Deploying the claw binary and systemd service
- Kill/restart of services during install

The correct distinction is:
- **Config format knowledge** (kind-specific): NZC and OpenClaw have known config formats the installer can parse/edit. Custom claws don't.
- **SSH infrastructure access** (needed for all claws on managed hosts): user provisioning, key deployment, clash policy, service management — applies regardless of claw kind.

A truly external claw (someone else's hosted service) might not need SSH. But any claw we're installing on our own infrastructure does.

---

## Scope NOT in session 4 (needs follow-on)

### 1. Clash/permissions policy sync
- The installer should offer to deploy and idempotently synchronize a clash policy to each target
- Central policy defined once, scoped versions pushed to each claw
- This is a `nonzeroclawed sync-policy` concern as much as install-time
- Needs its own design: what does the policy format look like? how are per-claw scopes defined?

### 2. System user/account provisioning
- Installer generates a dedicated user account on each remote host
- SSH keypair generated, stored in vault, deployed to remote `authorized_keys`
- This is the "secure by default" story: NonZeroClawed never uses root SSH

### 3. Service install/management
- Installing NZC or NonZeroClawed binary on remote hosts
- Writing systemd service files
- Enable/start/restart/stop service as part of install
- Health check after service restart (different from health check after config change)

### 4. Kill/restart handling
- Some config changes require service restart
- Installer needs to know: does this change require restart? how do we restart safely?
- Restart + health check loop with rollback if service doesn't come back

### 5. Clash adapter for non-NZC/OpenClaw claws
- Currently only NZC and OpenClaw have known clash integration
- Need a generic "deploy clash wrapper" that works for arbitrary claws

---

## Architecture question for Opus review

Session 2 defined `ChannelAssignment` and `OpenClawInstallation` structs in NZC crate.
Session 4 needs similar structs in the NonZeroClawed crate.
**Should these be in a shared `nonzeroclawed-common` crate?** Currently they'd be duplicated.

---

## Brian's framing (verbatim, 2026-03-30)

> "you may even need to kill and restart"
> "clash-nono/or whatever we landed on as our central permissions system offers to also coordinate and idempotently synchronize custom scoped versions of central permissions across any number of targets"
> "it does need to SSH in actually to install the permissions layer/users/ssh keys we generate"
> "all of that is optional but..."
> "the name is nonzeroclawed so we need to assume many [claws]"
> "installer running on one machine while targeting a remote machine, in fact several remote machines"
> "NZC target, nonzeroclawed target and openclaw target need to be separately configurable endpoints (and separately configured SSH keys/user accounts)"
