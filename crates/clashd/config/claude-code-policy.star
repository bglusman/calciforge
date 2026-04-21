# clashd policy for Claude Code tool calls (PreToolUse hook)
#
# Tool names match Claude Code's tool names exactly:
#   Bash, Read, Write, Edit, Glob, Grep, WebFetch, WebSearch, Agent
#
# Return "allow", "deny", or "review" (or {"verdict": "...", "reason": "..."}).
# "review" causes the hook to block and explain — adjust policy to allow things
# you're comfortable with.

def evaluate(tool, args, context):
    cwd = context.get("cwd", "")
    agent_id = context.get("agent_id", "claude-code")

    # ── Bash ─────────────────────────────────────────────────────────────────

    if tool == "Bash":
        cmd = args.get("command", "")

        # Hard deny: filesystem wipes
        wipes = ["rm -rf /", "rm -rf ~", "rm -rf $HOME", "wipefs", ":(){ :|:& };:"]
        for pattern in wipes:
            if pattern in cmd:
                return {"verdict": "deny", "reason": "Destructive command blocked: " + pattern}

        # Hard deny: disk-level operations
        disk_ops = ["mkfs", "fdisk", "diskutil eraseDisk", "dd if=", "dd bs="]
        for op in disk_ops:
            if op in cmd:
                return {"verdict": "deny", "reason": "Disk operation blocked: " + op}

        # Hard deny: force-push to main/master (branch protection)
        if "git push" in cmd and ("--force" in cmd or " -f " in cmd):
            if "origin/main" in cmd or "origin/master" in cmd or "main" in cmd.split("origin")[-1] or "master" in cmd.split("origin")[-1]:
                if "--force-with-lease" not in cmd:
                    return {"verdict": "deny", "reason": "Force-push to main/master without --force-with-lease is blocked"}

        # Review: SQL destructive operations
        sql_ops = ["DROP TABLE", "DROP DATABASE", "TRUNCATE TABLE"]
        for op in sql_ops:
            if op in cmd.upper():
                return {"verdict": "review", "reason": "SQL destructive operation requires review: " + op}

        # Allow everything else
        return "allow"

    # ── Write / Edit ──────────────────────────────────────────────────────────

    if tool == "Write" or tool == "Edit":
        path = args.get("file_path", "")

        # Protect critical system files
        system_paths = ["/etc/passwd", "/etc/shadow", "/etc/sudoers", "/etc/hosts"]
        for sp in system_paths:
            if path.startswith(sp):
                return {"verdict": "deny", "reason": "Write to system file blocked: " + path}

        return "allow"

    # ── WebFetch ──────────────────────────────────────────────────────────────

    if tool == "WebFetch":
        url = args.get("url", "")
        domain = context.get("domain", "")

        # Block threat-feed domains
        matched_feeds = context.get("domain_lists", [])
        if matched_feeds:
            return {
                "verdict": "deny",
                "reason": "Domain in threat feed (" + ", ".join(matched_feeds) + "): " + domain
            }

        return "allow"

    # ── Agent (spawning sub-agents) ───────────────────────────────────────────

    if tool == "Agent":
        # Allow sub-agents but log for awareness
        return "allow"

    # ── Everything else: Read, Glob, Grep, WebSearch, etc. ───────────────────

    return "allow"
