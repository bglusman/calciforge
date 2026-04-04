# jai Analysis

_Research date: 2026-03-30_
_Source: https://github.com/stanford-scs/jai (master branch)_
_Author: Stanford Security Lab / David Mazieres_

---

## What Is jai?

jai ("Jail your AI") is a lightweight Linux sandbox for AI CLI tools. It uses Linux kernel namespaces and overlayfs to give a process:

- Full read/write access to the current working directory
- Copy-on-write (COW) access to the rest of the home directory
- Private `/tmp` and `/var/tmp`
- Read-only access to the rest of the filesystem

The design philosophy is "better than nothing, simpler than Docker." It's explicitly not a security boundary for untrusted code—more a blast-radius reducer.

---

## How jai Implements Its COW Overlay

**Mechanism: Linux kernel overlayfs via the new-style `fsopen`/`fsconfig`/`fsmount` API (not legacy `mount()`).**

It is NOT user-space COW. It is NOT a user-space reimplementation. It uses the kernel's overlayfs directly.

Key code path (`jai.cc`, function `make_home_overlay()`):

```
1. Create a directory: ~/.jai/<sandbox>.home   (the sandboxed home mount point)
2. Create a directory: <storage>/<sandbox>.changes  (the overlayfs "upper" dir)
3. Create a directory: <storage>/<sandbox>.work     (overlayfs work dir, required)
4. Call fsopen("overlay", ...)
5. Set lowerdir+ = real home directory (via fsconfig FSCONFIG_SET_FD)
6. Set upperdir = .changes dir
7. Set workdir = .work dir
8. Call fsmount() → get mount fd
9. Call move_mount() to attach at ~/.jai/<sandbox>.home
```

The jailed process then runs in a new mount namespace (`unshare(CLONE_NEWNS)`) where the sandboxed home is bind-mounted over the real home.

**Everything is kernel-native overlayfs.** No FUSE. No user-space copy. The kernel intercepts writes and redirects them to the `upperdir`.

### Namespace stack

jai forks with multiple new namespaces:
- `CLONE_NEWUSER` — user namespace (capability escalation for mount ops without real root)
- `CLONE_NEWNS` — mount namespace (private filesystem view)
- `CLONE_NEWPID` — PID namespace (can't kill outside processes)
- `CLONE_NEWIPC` — IPC namespace

This means jai requires either setuid root OR a kernel configured to allow unprivileged user namespaces (which Ubuntu/Debian enable by default since ~2016).

---

## The "Changes" Directory

After the jailed process runs, all filesystem writes made to the home directory land in `~/.jai/<sandbox>.changes/` (or `<storage>/<sandbox>.changes/`). This directory contains the overlayfs upper layer:

- New files: appear directly
- Modified files: full COW copy of the modified file
- Deleted files: "whiteout" entries (character device with major/minor 0)

The man page explicitly documents this: *"If you wanted these changes in your home directory, you can destroy the jail with `jai -u`, move the changed files back into your home directory..."*

**This is the commit path.** jai doesn't automate it — but the upper dir is a regular filesystem directory you can read and replay manually.

---

## Does jai Have a "Commit" Concept?

**No. jai is strictly run-and-discard.**

The `-u` flag unmounts overlays and cleans up work directories — it does NOT commit changes back. Changes in the upper dir are left in `~/.jai/<sandbox>.changes/` but it's entirely up to the user to manually move them.

There is no `jai commit`, no `jai apply`, no API for it.

---

## What Would It Take to Implement a Commit Step on Top of jai?

A commit step would need to:

1. **Wait for the jailed process to exit** (or capture state while it's running, which is risky)
2. **Walk the overlayfs upper dir** (`<sandbox>.changes/`)
3. **For each entry in the upper dir:**
   - If it's a regular file → copy/rename it to the corresponding path in the real home
   - If it's a whiteout (char device 0:0 or extended attribute `trusted.overlay.whiteout`) → delete the corresponding real file
   - If it's a directory → ensure the target directory exists
   - Preserve permissions, ownership, timestamps, xattrs
4. **Clean up** the upper dir after successful commit

**Gotchas:**
- Overlayfs uses two whiteout mechanisms: character device nodes (old style) and xattrs. Need to handle both.
- Nested directories with mixed whiteouts + new files require careful ordering.
- The upper dir is only reliable after the jailed process exits and the overlay is unmounted (or at minimum synced). jai uses a `.lock` file to coordinate this.
- ACLs and xattrs on the original files need to be carried through.
- You'd need to run the commit step as the user (or as root), since the changes dir may have ownership set to the sandbox user.

**Estimated effort:** A robust commit step is ~200–400 lines of Rust/Python. It's non-trivial but not hard. The main complexity is whiteout handling and atomicity (use `rename()` for individual files, but the overall commit is NOT atomic across multiple files).

---

## What's the Startup Cost?

jai startup involves:
- Kernel namespace creation (fast, ~1–5ms)
- overlayfs mount via `fsopen`/`fsmount` (kernel call, ~5–20ms)
- Process fork + exec

**Estimated total: 30–100ms** for the overlay setup on a modern system.

This is fast enough for per-operation use if wrapping a long-running tool session. It is NOT fast enough for wrapping individual file writes (hundreds of per-write operations per second would each cost 50–100ms → unacceptable).

**The right granularity for jai is per-session (wrap the entire agent run) or per-high-risk-operation-group (wrap a multi-step config change), NOT per-file-write.**

### Kernel version constraint

jai requires **Linux kernel 6.13 or later** and uses `fsopen`/`fsconfig`/`fsmount` (the new-style mount API). It also requires **gcc 15 or clang 22** to build (C++23 features).

This is a real deployment constraint:
- Ubuntu 24.04 LTS ships kernel 6.8 — jai may NOT work out of the box
- Ubuntu 25.04+ or kernel 6.13+ required
- NixOS 24.11+ should be fine

---

## License

**GPL version 3 or later.** All rights reserved by David Mazieres / Stanford.

`COPYING` file header: `Copyright (C) 2026 David Mazieres. All rights reserved. Distribution permitted under the GNU General Public License (GPL) version 3 or later.`

**Implications for NZC/NonZeroClawed:**
- GPL v3 is copyleft. If we **statically link** or **embed** jai's code, the entire codebase would need to be GPL v3.
- GPL v3 is **compatible with NZC's MIT OR Apache-2.0** license only if jai is used as a **separate process** (subprocess invocation), NOT as a linked library.
- **Conclusion: We can shell out to `jai` binary but cannot embed or statically link its code.** The subprocess model is fine and is how jai is designed to be used anyway.

---

## Summary Assessment

| Question | Finding |
|---|---|
| COW mechanism | Kernel overlayfs (fsopen/fsmount API) |
| User-space component? | None — pure kernel overlayfs |
| Commit concept? | No — manual upper dir copy only |
| Upper dir accessible? | Yes, at `~/.jai/<sandbox>.changes/` |
| Startup cost | ~30–100ms (per-session fine, per-write not) |
| Can we build commit on top? | Yes, ~200–400 lines, non-trivial but doable |
| Kernel requirement | 6.13+ (significant constraint) |
| License | GPL v3 — subprocess OK, embedding not |
| Requires setuid? | Yes (or unprivileged user namespaces enabled) |

**Bottom line:** jai is elegant and well-designed. As a "wrap entire agent session in COW" tool it's excellent. As a programmatic transaction backend for NZC, it has two blockers: (1) kernel 6.13+ requirement limits deployability, (2) no native commit — we'd build on top. The subprocess model is the only licensing-safe approach.
