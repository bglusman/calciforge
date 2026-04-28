# Blocks high-risk shell-command patterns in agent-visible content.
# Keep this list narrow to avoid turning normal documentation into noise.

DESTRUCTIVE_PATTERNS = [
    "rm -rf /",
    "rm -rf /*",
    "mkfs.",
    "dd if=",
    " of=/dev/",
    ":(){ :|:& };:",
    "chmod -r 777 /",
    "chmod 777 /",
    "chown -r ",
    "shutdown now",
    "reboot now",
]

DOWNLOAD_COMMANDS = [
    "curl ",
    "wget ",
]

PIPE_TO_SHELL_PATTERNS = [
    "| sh",
    "| bash",
    "| zsh",
    "| sudo sh",
    "| sudo bash",
]

def contains_any(content, terms):
    for term in terms:
        if term in content:
            return True
    return False

def scan(input):
    content = input["content"].lower()

    if contains_any(content, DESTRUCTIVE_PATTERNS):
        return {
            "verdict": "unsafe",
            "reason": "destructive shell-command pattern blocked by operator policy",
        }

    if contains_any(content, DOWNLOAD_COMMANDS):
        if contains_any(content, PIPE_TO_SHELL_PATTERNS):
            return {
                "verdict": "unsafe",
                "reason": "download piped to shell blocked by operator policy",
            }
        return {
            "verdict": "review",
            "reason": "network download command should be reviewed before agent use",
        }

    return "clean"
