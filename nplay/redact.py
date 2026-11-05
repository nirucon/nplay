"""Credential redaction for URLs and free text.

Subsonic/Navidrome URLs carry a replayable token+salt pair (or a password)
in the query string. Nothing that can contain a source URL may be written to
a log, a process title or an error shown to the user without passing through
``redact_url`` or ``redact_text``.
"""
import re
import urllib.parse

SECRET_KEYS = ("t", "s", "p", "token", "apiKey", "access_token")
MASK = "REDACTED"
_KEYS = {k.lower() for k in SECRET_KEYS}
# A query parameter is secret when its name is one of SECRET_KEYS and it
# follows '?' or '&' (also the percent-encoded forms that appear in mpv logs).
_TEXT_RE = re.compile(
    r"(?P<lead>[?&]|%3[Ff]|%26)(?P<key>" + "|".join(re.escape(k) for k in SECRET_KEYS) + r")(?P<eq>=|%3[Dd])(?P<val>[^&\s\"'<>#,;)\]}]*)",
    re.IGNORECASE,
)


def _sub(m):
    if not m.group("val") or m.group("val") == MASK:
        return m.group(0)
    return m.group("lead") + m.group("key") + m.group("eq") + MASK


def redact_text(text):
    """Return ``text`` with every secret query value replaced by REDACTED."""
    if text is None:
        return ""
    s = str(text)
    if "=" not in s and "%3" not in s.lower():
        return s
    return _TEXT_RE.sub(_sub, s)


def redact_url(url):
    """Return ``url`` without credential query values.

    Non-secret parameters (user, id, client, version) are kept so logs stay
    useful for diagnosis.
    """
    if not url:
        return ""
    s = str(url)
    try:
        parts = urllib.parse.urlsplit(s)
    except ValueError:
        return redact_text(s)
    if not parts.query:
        return redact_text(s)
    pairs = urllib.parse.parse_qsl(parts.query, keep_blank_values=True)
    if not any(k.lower() in _KEYS for k, _ in pairs):
        return s
    query = "&".join(
        f"{urllib.parse.quote(k, safe='')}={MASK if k.lower() in _KEYS else urllib.parse.quote(v, safe='')}"
        for k, v in pairs
    )
    return urllib.parse.urlunsplit((parts.scheme, parts.netloc, parts.path, query, parts.fragment))


def contains_secret(text):
    """True if ``text`` still contains an unredacted secret query value."""
    for m in _TEXT_RE.finditer(str(text or "")):
        if m.group("val") and m.group("val") != MASK:
            return True
    return False
