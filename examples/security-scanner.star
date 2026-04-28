def scan(input):
    content = input["content"].lower()
    url = input["url"].lower()
    context = input["context"]

    if context == "api" and "wire money" in content:
        return {
            "verdict": "unsafe",
            "reason": "operator policy blocks wire-transfer instructions in API traffic",
        }

    if url.endswith(".internal.example") and "password" in content:
        return {
            "verdict": "review",
            "reason": "operator policy reviews password-like internal content",
        }

    return "clean"
