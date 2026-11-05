"""Credential redaction for URLs and free text.

Subsonic/Navidrome URLs carry a replayable token+salt pair (or a password)
in the query string. Nothing that can contain a source URL may be written to
a log, a process title or an error shown to the user without passing through
``redact_url`` or ``redact_text``.

A query that carries a credential is rewritten as its non-secret parameters
followed by ``auth=REDACTED``: the secret values (``t``, ``s``, ``p``,
``token``, ``apiKey``, ``access_token``) and the Subsonic user name ``u`` are
dropped, ``id``, ``v``, ``c`` and the like stay for diagnosis::

    /rest/stream.view?u=me&t=…&s=…&v=1.16.1&c=nplay&id=42
    /rest/stream.view?v=1.16.1&c=nplay&id=42&auth=REDACTED
"""
import re
import urllib.parse

SECRET_KEYS = ("t", "s", "p", "token", "apiKey", "access_token")
# Dropped together with a secret: identifies the account the token belongs to.
ACCOUNT_KEYS = ("u",)
MASK = "REDACTED"
MARKER = "auth=" + MASK
_SECRET = {k.lower() for k in SECRET_KEYS}
_DROP = _SECRET | {k.lower() for k in ACCOUNT_KEYS}
_KEYS_RE = "|".join(re.escape(k) for k in SECRET_KEYS)
# A plain query string: '?' up to whitespace, quote, bracket or fragment.
_QUERY_RE = re.compile(r"\?([^\s\"'<>#\]\)]*)")
# Fallback for encoded or partial forms (e.g. percent-encoded URLs in mpv logs).
_PARAM_RE = re.compile(
    r"(?P<lead>[?&]|%3[Ff]|%26)(?P<key>" + _KEYS_RE + r")(?P<eq>=|%3[Dd])(?P<val>[^&\s\"'<>#,;)\]}]*)",
    re.IGNORECASE,
)


def _has_secret_pairs(pairs):
    return any(k.lower() in _SECRET and v and v != MASK for k, v in pairs)


def _clean_query(query):
    """Return the redacted query, or None when it carries no credential."""
    try:
        pairs = urllib.parse.parse_qsl(query, keep_blank_values=True)
    except ValueError:
        return None
    if not _has_secret_pairs(pairs):
        return None
    kept = [(k, v) for k, v in pairs if k.lower() not in _DROP]
    head = urllib.parse.urlencode(kept, safe=":/")
    return (head + "&" if head else "") + MARKER


def _param_sub(m):
    if not m.group("val") or m.group("val") == MASK:
        return m.group(0)
    return m.group("lead") + m.group("key") + m.group("eq") + MASK


def redact_text(text):
    """Return ``text`` with every credential-bearing query redacted."""
    if text is None:
        return ""
    s = str(text)
    if "=" not in s and "%3" not in s.lower():
        return s

    def query_sub(m):
        clean = _clean_query(m.group(1))
        return m.group(0) if clean is None else "?" + clean

    s = _QUERY_RE.sub(query_sub, s)
    return _PARAM_RE.sub(_param_sub, s)


def redact_url(url):
    """Return ``url`` without credentials (see the module docstring)."""
    if not url:
        return ""
    s = str(url)
    try:
        parts = urllib.parse.urlsplit(s)
    except ValueError:
        return redact_text(s)
    clean = _clean_query(parts.query) if parts.query else None
    if clean is None:
        return redact_text(s)
    return urllib.parse.urlunsplit((parts.scheme, parts.netloc, parts.path, clean, parts.fragment))


def contains_secret(text):
    """True if ``text`` still contains an unredacted credential query value."""
    for m in _PARAM_RE.finditer(str(text or "")):
        if m.group("val") and m.group("val") != MASK:
            return True
    return False
