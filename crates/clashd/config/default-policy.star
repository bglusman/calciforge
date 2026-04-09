# Default clashd policy — Starlark
#
# This policy is evaluated for every OpenClaw tool call.
# Return "allow", "deny", or "review" (or {"verdict": "...", "reason": "..."}).
#
# Available context:
#   tool         — tool name (e.g., "exec", "gateway", "browser")
#   args         — tool arguments (dict)
#   agent_id     — which agent is making the call (if set)
#   domain       — domain extracted from args (if any)
#   domain_lists — which threat feeds matched this domain
#   agent_allowed_domains — per-agent allow list
#   agent_denied_domains  — per-agent deny list

def evaluate(tool, args, context):
    """Evaluate a tool call against policy."""

    # ── Domain filtering ──────────────────────────────────────

    domain = context.get("domain")
    if domain:
        # Block domains in threat intelligence feeds
        matched_feeds = context.get("domain_lists", [])
        if matched_feeds:
            feeds_str = ", ".join(matched_feeds)
            return {
                "verdict": "deny",
                "reason": "Domain " + domain + " found in threat feeds: " + feeds_str
            }

        # Check per-agent denied domains
        denied = context.get("agent_denied_domains", [])
        if domain in denied:
            return {
                "verdict": "deny",
                "reason": "Domain " + domain + " denied for this agent"
            }

        # If agent has an explicit allow list, require domain to be in it
        allowed = context.get("agent_allowed_domains", [])
        if allowed and domain not in allowed:
            return {
                "verdict": "review",
                "reason": "Domain " + domain + " not in agent allow list"
            }

    # ── Tool-level rules ──────────────────────────────────────

    # Gateway config changes — always require review
    if tool == "gateway":
        action = args.get("action", "")
        if action in ("config.patch", "config.apply", "restart"):
            return {
                "verdict": "review",
                "reason": "Gateway " + action + " requires custodian approval"
            }

    # Destructive shell commands — deny
    if tool == "exec":
        cmd = args.get("command", "")
        destructive = ["rm -rf /", "mkfs", "wipefs", "dd if=/dev/", ":(){ :|:& };:"]
        for pattern in destructive:
            if pattern in cmd:
                return {
                    "verdict": "deny",
                    "reason": "Destructive command pattern blocked: " + pattern
                }

    # Browser navigating to URLs in blocked domains
    if tool == "browser" and domain:
        # Already checked above — this is additional browser-specific logic
        pass

    # Cron job modification — review during business hours only
    if tool == "cron":
        action = args.get("action", "")
        if action in ("add", "remove", "update"):
            return {
                "verdict": "review",
                "reason": "Cron " + action + " requires approval"
            }

    # Default: allow
    return "allow"
