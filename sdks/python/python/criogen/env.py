"""API keys from the environment, and a ``.env`` file to keep several in.

Each provider reads its key from its usual variable, so you can keep all of
them side by side and switch models by changing a string:

    OPENAI_API_KEY      ``"openai:gpt-5-mini"``
    ANTHROPIC_API_KEY   ``"anthropic:claude-sonnet-5-5"``
    GEMINI_API_KEY      ``"gemini:gemini-2.5-flash"``
    OPENROUTER_API_KEY  ``"openrouter:meta-llama/llama-3.3-70b-instruct"``

``load_env()`` reads a ``.env`` file into the process environment (call it
before you build a model). It never prints or returns a value, and never
replaces a variable that is already set unless you ask.
"""

import os
import re
from typing import Dict, List, Union

from .models import _KEYS

_MAX_BYTES = 1_000_000
_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def parse_env(text: str) -> Dict[str, str]:
    """Parse ``.env`` text: ``NAME=value`` lines, ``export NAME=value``, ``#``
    comments, and values in single or double quotes. No variable expansion.
    A malformed line raises ``ValueError`` naming the line number (never its
    content: it may be a secret)."""
    out: Dict[str, str] = {}
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export ") or line.startswith("export\t"):
            line = line[7:].lstrip()
        name, sep, value = line.partition("=")
        name = name.strip()
        if not sep or not _NAME.match(name):
            raise ValueError(f".env line {number}: expected NAME=value")
        value = value.strip()
        if value[:1] in ("'", '"'):
            quote = value[0]
            end = value.find(quote, 1)
            if end == -1:
                raise ValueError(f".env line {number}: a quoted value is not closed")
            rest = value[end + 1 :].strip()
            if rest and not rest.startswith("#"):
                raise ValueError(f".env line {number}: unexpected text after a quoted value")
            value = value[1:end]
        else:
            hash_at = value.find(" #")
            if hash_at != -1:
                value = value[:hash_at].rstrip()
        out[name] = value
    return out


def load_env(path: Union[str, "os.PathLike[str]"] = ".env", *, override: bool = False, required: bool = False) -> List[str]:
    """Read a ``.env`` file into ``os.environ`` and return the NAMES it set
    (sorted; never the values).

    - A variable that is already set is kept, unless ``override=True``.
    - A missing file is fine (returns ``[]``), unless ``required=True``.
    - Files over 1 MB are refused.

    Keep the file out of git (add ``.env`` to ``.gitignore``).
    """
    try:
        size = os.path.getsize(path)
    except FileNotFoundError:
        if required:
            raise
        return []
    if size > _MAX_BYTES:
        raise ValueError(f"{os.fspath(path)} is larger than {_MAX_BYTES} bytes; not an environment file")
    with open(path, encoding="utf-8-sig") as f:
        values = parse_env(f.read())
    loaded = []
    for name, value in values.items():
        if override or name not in os.environ:
            os.environ[name] = value
            loaded.append(name)
    return sorted(loaded)


def status() -> Dict[str, bool]:
    """Which providers have a key right now: ``{"openai": True, "gemini": False, ...}``.
    Names and yes/no only, never a key."""
    return {provider: bool(os.environ.get(name)) for provider, name in _KEYS.items()}


def key_name(provider: str) -> str:
    """The environment variable a provider's key comes from (``"openai"`` -> ``"OPENAI_API_KEY"``)."""
    try:
        return _KEYS[provider]
    except KeyError:
        raise ValueError(f"unknown provider {provider!r}; known: {', '.join(sorted(_KEYS))}") from None
