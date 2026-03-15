# Clash profile: lucien (infrastructure guardian)
# Lucien is generally unrestricted (no shell restrictions).
# Only restriction: cannot modify his own governance files.
#
# Base policy runs first and handles catastrophic/review-level shell commands.
# This profile adds file-write protection for specific governance files only.

PROTECTED_FILES = [
    "/etc/nonzeroclaw/workspace/.clash/policy.star",
    "/etc/nonzeroclaw/config.toml",
    "/etc/nonzeroclaw-david/workspace/.clash/policy.star",
    "/etc/nonzeroclaw-david/config.toml",
    "/usr/local/bin/nonzeroclaw",
]

def evaluate(action, identity, agent, command="", path=""):
    if action == "tool:file_write":
        for protected in PROTECTED_FILES:
            if path == protected or path.endswith(protected):
                return "deny:Protected file — Lucien cannot modify: " + path
    return "allow"
