# Reviews or blocks credential-shaped content sent to unapproved destinations.
# Edit these constants for your own deployment.

ALLOWED_CREDENTIAL_DOMAINS = [
    "api.anthropic.com",
    "api.openai.com",
    "api.mistral.ai",
]

CREDENTIAL_TERMS = [
    "api key",
    "apikey",
    "auth token",
    "bearer token",
    "client secret",
    "password",
    "private key",
    "secret key",
]

REVIEW_ONLY_TERMS = [
    "redacted",
    "example",
    "placeholder",
]

def host_from_url(url):
    value = url.lower()
    if "://" in value:
        parts = value.split("://")
        value = parts[1]
    value = value.split("#")[0]
    value = value.split("?")[0]
    value = value.split("/")[0]
    if "@" in value:
        parts = value.split("@")
        value = parts[len(parts) - 1]
    if value.startswith("[") and "]" in value:
        return value.split("]")[0] + "]"
    value = value.split(":")[0]
    return value

def host_allowed(host):
    for domain in ALLOWED_CREDENTIAL_DOMAINS:
        if host == domain or host.endswith("." + domain):
            return True
    return False

def contains_any(content, terms):
    for term in terms:
        if term in content:
            return True
    return False

def scan(input):
    content = input["content"].lower()
    host = host_from_url(input["url"])

    if not contains_any(content, CREDENTIAL_TERMS):
        return "clean"

    if host_allowed(host):
        return "clean"

    if contains_any(content, REVIEW_ONLY_TERMS):
        return {
            "verdict": "review",
            "reason": "credential-like example content is leaving the destination allowlist",
        }

    return {
        "verdict": "unsafe",
        "reason": "credential-shaped content is leaving the destination allowlist",
    }
