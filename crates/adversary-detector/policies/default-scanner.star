# Default Calciforge scanner policy.
#
# This policy defines Calciforge's default scanner checks while keeping the
# rule text editable. It runs in-process through
# adversary-detector's Starlark evaluator with `load()` disabled.

INJECTION_PHRASES = [
    "ignore previous instructions",
    "ignore all previous instructions",
    "disregard previous instructions",
    "disregard all previous",
    "disregard the above",
    "forget previous instructions",
    "forget all previous",
    "you are now",
    "act as if you are",
    "act as a",
    "pretend you are",
    "pretend to be",
    "roleplay as",
    "jailbreak",
    "dan mode",
    "developer mode enabled",
    "ignore your instructions",
    "override your instructions",
    "your new instructions are",
    "new system prompt",
    "ignore the system prompt",
    "bypass your",
    "you have no restrictions",
    "you have no rules",
    "unlimited mode",
]

DISCUSSION_CONTEXT = [
    "prompt injection",
    "jailbreak attempt",
    "adversarial prompt",
    "this is an example",
    "example of injection",
    "how attackers",
    "researchers have found",
    "security researchers",
    "cve-",
    "vulnerability",
    "proof of concept",
    "poc exploit",
]

def count_matches(content, terms):
    count = 0
    for term in terms:
        if term in content:
            count += 1
    return count

def verdict(verdict, reason):
    return {"verdict": verdict, "reason": reason}

def scan(input):
    content = input["content"]
    lower = content.lower()
    review_reason = None

    if regex_match(r"[\u{200B}\u{200C}\u{200D}\u{2060}\u{00AD}]", content):
        return verdict("unsafe", "zero-width invisible characters detected")

    if regex_match(r"[\u{E0000}-\u{E007F}]", content):
        return verdict("unsafe", "Unicode tag characters (U+E0000 range) detected")

    if regex_match(r"(?is)<!--.{0,200}(ignore|disregard|override|system prompt|secret|exfiltrate).{0,200}-->", content):
        review_reason = "HTML comment contains instruction-like or secret-related language"

    if regex_match(r"(?is)<[^>]+\b(data-[a-z0-9_-]+|aria-label|alt|title)\s*=\s*['\"][^'\"]{0,300}(ignore|disregard|override|system prompt|secret|exfiltrate)[^'\"]{0,300}['\"]", content):
        review_reason = review_reason or "HTML metadata attribute contains instruction-like or secret-related language"

    if regex_match(
        r"(?ix)display\s*:\s*none|visibility\s*:\s*hidden|font-size\s*:\s*(0(\s*px)?|[01](\.\d+)?px)|opacity\s*:\s*0(\.0+)?|color\s*:\s*(white|\#fff(fff)?)|color\s*:\s*rgba?\s*\(\s*255\s*,\s*255\s*,\s*255|position\s*:\s*(absolute|fixed)[^>]{0,160}(left|right|top|bottom)\s*:\s*-?[0-9]{4,}",
        content,
    ):
        review_reason = review_reason or "CSS content-hiding pattern detected"

    if regex_match(r"[A-Za-z0-9+/\-_]{500,}={0,2}", content):
        review_reason = review_reason or "large base64 blob detected (possible hidden payload)"

    if base64_decoded_regex_match(
        r"(?i)ignore previous instructions|disregard previous instructions|new system prompt|send\s+to\s+https?://|POST\s+to\s+https?://|exfiltrate|api[-_\s]?key|secret[-_\s]?key",
        content,
    ):
        review_reason = review_reason or "base64-decoded content matched adversarial language"

    injection_count = count_matches(lower, INJECTION_PHRASES)
    discussion_count = count_matches(lower, DISCUSSION_CONTEXT)

    if injection_count > 0:
        min_signals = input["min_signals_for_ratio"]
        threshold = input["discussion_ratio_threshold"]
        if injection_count >= min_signals and (discussion_count / injection_count) > threshold:
            review_reason = review_reason or "injection phrases found but discussion context detected (%s injection, %s discussion signals)" % (injection_count, discussion_count)
        else:
            return verdict("unsafe", "prompt injection phrases detected (%s match(es))" % injection_count)

    if regex_match(
        r"(?ix)(if|when).{0,80}(blocked|denied|not allowed|fails).{0,160}(python|perl|ruby|node|powershell|bash|sh)\s+(-c|/c|<<|script|macro)|sudo\s+tcpdump\b.{0,160}\s-z\s+['\"]?|base64\s+[^|]{0,120}\|\s*base64\s+(-d|--decode)",
        content,
    ):
        review_reason = review_reason or "tool-policy bypass pattern detected"

    if regex_match(
        r"(?ix)(send|give|provide|share|reveal|disclose|tell\s+me|what\s+is)\s+(me\s+)?(your|the)?\s*(password|api[-_\s]?key|secret[-_\s]?key|auth[-_\s]?token|access[-_\s]?token|credential|private[-_\s]?key|ssh[-_\s]?key|bearer[-_\s]?token|two[-_\s]?factor|2fa|otp|recovery[-_\s]?code)",
        content,
    ):
        return verdict("unsafe", "PII harvesting pattern detected")

    if regex_match(
        r"(?ix)exfiltrate|POST\s+to\s+https?://|send\s+to\s+https?://|report\s+back\s+to|beacon\s+to|(curl|wget|nc|netcat)\s+.{0,80}https?://",
        content,
    ):
        return verdict("unsafe", "exfiltration signal detected")

    if review_reason:
        return verdict("review", review_reason)

    return "clean"
