# Clash profile: david (research)
# Same restrictions as renee profile.
# See profiles/renee.star for full allowlist and rationale.
#
# Shell commands: restricted to an explicit read-only allowlist.
# File writes: require human review before proceeding.

RESEARCH_ALLOWED_COMMANDS = [
    "ls", "cat", "grep", "find", "echo", "pwd", "wc", "head", "tail",
    "date", "curl", "wget", "df", "du", "ps", "free", "uname", "which",
    "hostname", "whoami", "id", "env", "stat", "file", "sort", "uniq",
    "cut", "tr", "awk", "sed", "jq", "python3", "diff", "md5sum", "sha256sum",
]

def first_word(cmd):
    parts = cmd.strip().split()
    if len(parts) > 0:
        return parts[0].split("/")[-1]  # basename
    return ""

def evaluate(action, identity, agent, command="", path=""):
    # Shell: restrict to read-only allowlist
    if action == "tool:shell" and command != "":
        cmd_first = first_word(command)
        if cmd_first not in RESEARCH_ALLOWED_COMMANDS:
            return "deny:Shell command not permitted for research profile: " + cmd_first

    # File writes: require review
    if action == "tool:file_write":
        return "review:File write requires approval for research profile"

    return "allow"
