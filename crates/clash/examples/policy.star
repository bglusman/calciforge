# NZC Clash Base Policy — command and action enforcement, no identity branching.
#
# Identity-specific restrictions live in profiles/{identity}.star.
# Base policy runs first; profiles can only add restrictions (never loosen).
#
# Identities with NO profile file (brian, max, nonzeroclaw, etc.) get base policy only.

# Commands that are ALWAYS denied — no exceptions, no approval path.
# NOTE: Use specific-enough patterns to avoid false positives.
# "rm -rf /" would match "rm -rf /tmp/foo" via substring — use is_root_wipe() instead.
ALWAYS_DENY_PATTERNS = [
    "zfs destroy -r ",      # recursive pool/dataset destroy
    "zfs destroy -rf",      # recursive force destroy
    "dd if=",               # raw disk writes (almost always catastrophic)
    "mkfs",                 # filesystem creation (destroys data)
    "wipefs",               # wipe filesystem signatures
    "shred /dev/",          # shred block device
    ":(){ :|:& };:",        # fork bomb
]

# Commands that require review (blocked until human approves)
REVIEW_PATTERNS = [
    "zfs destroy",          # single dataset destroy (no -r)
    "zfs rollback",         # rollback snapshot
    "zfs rename",           # rename dataset
    "rm -rf",               # recursive force remove (any path)
    "rm -fr",               # same, alternate flag order
    "rm -r",                # recursive remove without force
    "parted ",              # partition table edits
    "fdisk ",               # partition table edits
    "lvremove",             # LVM volume remove
    "vgremove",             # LVM volume group remove
    "pvremove",             # LVM physical volume remove
    "cryptsetup luksFormat", # LUKS format (destroys data)
    "truncate -s 0",        # truncate file to zero
]

def normalize(cmd):
    """Normalize command: lowercase, strip outer whitespace, collapse internal whitespace.
    Uses split() (no args) which splits on any whitespace (spaces, tabs, newlines)
    and discards empty tokens — same as Python's str.split().
    This prevents double-space and tab evasion of the substring pattern matchers.
    """
    return " ".join(cmd.lower().strip().split())

def command_matches_any(cmd, patterns):
    cmd_normalized = normalize(cmd)
    for p in patterns:
        if p in cmd_normalized:
            return True
    return False

WIPE_COMMANDS = ["rm", "shred", "dd", "wipe", "srm"]

def is_root_wipe(cmd):
    """Detect commands targeting the root filesystem specifically.
    Only fires for known destructive commands — not read-only commands like ls, cat, df.
    """
    cmd_stripped = cmd.strip()
    parts = cmd_stripped.split()
    if not parts:
        return False

    # Only destructive commands can be root wipes
    base = parts[0].split("/")[-1].lower()  # basename, lowercase
    if base not in WIPE_COMMANDS:
        return False

    # Check if any argument is exactly / or /*
    for i, part in enumerate(parts):
        if i == 0:
            continue  # skip the command itself
        if part in ["/", "/*"]:
            return True
        if part in ['"/\"', "'/'"]:
            return True
    return False

def first_word(cmd):
    # Use split() (no args) to normalize whitespace — handles tabs/double-spaces.
    parts = cmd.strip().split()
    if len(parts) > 0:
        return parts[0].split("/")[-1]  # basename
    return ""

def evaluate(action, identity, agent, command="", path=""):
    # ── Shell tool: command-aware enforcement ──────────────────────────────
    if action == "tool:shell" and command != "":
        # Root filesystem wipe — always deny, no exceptions
        if is_root_wipe(command):
            return "deny:Command blocked (root filesystem wipe): " + command[:80]

        # Other catastrophic patterns — always deny
        if command_matches_any(command, ALWAYS_DENY_PATTERNS):
            return "deny:Command blocked (catastrophic risk): " + command[:80]

        # Review gate — blocked pending human approval
        if command_matches_any(command, REVIEW_PATTERNS):
            return "review:Command requires approval: " + command[:80]

    # ── Allow everything else ──────────────────────────────────────────────
    return "allow"
