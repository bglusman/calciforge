# nonzeroclaw default Clash policy template.
#
# Install to: ~/.nonzeroclaw/policy.star
#
# This policy is evaluated before EVERY tool call. Return values:
#   "allow"          — proceed with execution
#   "deny:<reason>"  — block, surface reason to LLM
#   "review:<reason>"— require human approval before proceeding
#
# Profile chain: place identity-specific overrides in
#   ~/.nonzeroclaw/profiles/{identity}.star
# Profiles may only ADD restrictions — they cannot loosen a base Deny.

# ── Always-allow actions ─────────────────────────────────────────────────────
# Read-only and search operations are permitted without review.
SAFE_ACTIONS = [
    "tool:read",
    "tool:file_read",
    "tool:list",
    "tool:file_list",
    "tool:search",
    "tool:glob_search",
    "tool:content_search",
    "tool:web_search_tool",
    "tool:memory_recall",
]

# ── Shell: patterns that require human review ─────────────────────────────────
DANGEROUS_SHELL_PATTERNS = [
    "rm -rf",
    "rm -fr",
    "rm -r",
    "mkfs",
    "dd if=",
    "del /f",
    "format ",
    "wipefs",
    "shred /dev/",
    ":(){ :|:& };:",  # fork bomb
]

# ── Network tools that warrant review from untrusted identities ──────────────
NETWORK_COMMANDS = ["curl", "wget", "nc -", "netcat", "ncat"]

# ── Trusted identities (customize to your setup) ─────────────────────────────
TRUSTED_IDENTITIES = ["owner"]


def normalize(cmd):
    """Collapse whitespace to prevent double-space/tab evasion."""
    return " ".join(cmd.lower().strip().split())


def command_matches_any(cmd, patterns):
    norm = normalize(cmd)
    for p in patterns:
        if p in norm:
            return True
    return False


def evaluate(action, identity, agent, command="", path=""):
    """
    Default nonzeroclaw policy.
    - Read-only actions are always allowed.
    - Destructive shell commands require review.
    - Network commands from non-trusted identities require review.
    - File deletion always requires review.
    - Non-HTTPS web fetches require review.
    """

    # Always allow safe / read-only actions.
    if action in SAFE_ACTIONS:
        return "allow"

    # Shell tool: command-aware enforcement.
    if action == "tool:shell" and command != "":
        # Destructive commands require review regardless of identity.
        if command_matches_any(command, DANGEROUS_SHELL_PATTERNS):
            return "review:destructive_command: " + command[:80]

        # Network commands from untrusted identities require review.
        if identity not in TRUSTED_IDENTITIES:
            if command_matches_any(command, NETWORK_COMMANDS):
                return "review:network_from_untrusted: " + command[:80]

    # File deletion always requires review.
    if action == "tool:delete":
        return "review:file_deletion: " + path[:80]

    # Non-HTTPS web fetches require review.
    if action in ("tool:web_fetch", "tool:http_request"):
        if command != "" and not command.startswith("https://"):
            return "review:insecure_fetch: " + command[:80]

    return "allow"
