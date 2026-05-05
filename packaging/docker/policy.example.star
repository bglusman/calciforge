def evaluate(tool, args, context):
    domain = context.get("domain")
    if domain:
        denied = context.get("agent_denied_domains", [])
        if domain in denied:
            return {
                "verdict": "deny",
                "reason": "Domain " + domain + " denied for this agent",
            }

        allowed = context.get("agent_allowed_domains", [])
        if allowed and domain not in allowed:
            return {
                "verdict": "review",
                "reason": "Domain " + domain + " is not in this agent allow list",
            }

    if tool == "exec":
        command = args.get("command", "")
        for pattern in ["rm -rf /", "mkfs", "wipefs", "dd if=/dev/"]:
            if pattern in command:
                return {
                    "verdict": "deny",
                    "reason": "Destructive command pattern blocked",
                }

    return "allow"
