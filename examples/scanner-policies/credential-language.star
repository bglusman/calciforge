# Reviews or blocks language that asks agents to reveal, send, or redirect secrets.

CREDENTIAL_TERMS = [
    "api key",
    "apikey",
    "auth token",
    "bearer token",
    "credential",
    "password",
    "private key",
    "secret",
]

EXFILTRATION_TERMS = [
    "post it",
    "post this",
    "post to",
    "send",
    "upload",
    "forward",
    "paste",
    "report back",
    "beacon",
    "webhook",
]

REQUEST_TERMS = [
    "show",
    "print",
    "reveal",
    "list",
    "dump",
    "what is",
]

DISCUSSION_TERMS = [
    "red team",
    "threat model",
    "security review",
    "example",
    "documentation",
]

def contains_any(content, terms):
    for term in terms:
        if term in content:
            return True
    return False

def scan(input):
    content = input["content"].lower()
    has_credential = contains_any(content, CREDENTIAL_TERMS)

    if not has_credential:
        return "clean"

    if contains_any(content, DISCUSSION_TERMS):
        return {
            "verdict": "review",
            "reason": "credential-language appears in security or documentation context",
        }

    if contains_any(content, EXFILTRATION_TERMS):
        return {
            "verdict": "unsafe",
            "reason": "credential exfiltration language blocked by operator policy",
        }

    if contains_any(content, REQUEST_TERMS):
        return {
            "verdict": "review",
            "reason": "credential disclosure request needs operator review",
        }

    return "clean"
