"""API keys from the environment and a .env file (design 0011 §2)."""

import os

import pytest

from criogen import OpenAI, env, load_env
from criogen.models import _KEYS


@pytest.fixture(autouse=True)
def clean(monkeypatch):
    for name in (*_KEYS.values(), "MY_OTHER_VAR"):
        monkeypatch.delenv(name, raising=False)


def test_parse_handles_comments_quotes_export_and_inline_comments():
    values = env.parse_env(
        "# keys\n"
        "OPENAI_API_KEY=sk-one\n"
        "export GEMINI_API_KEY = 'g-two'\n"
        'ANTHROPIC_API_KEY="a three"  # the Claude key\n'
        "MY_OTHER_VAR=plain # trailing\n"
        "\n"
        "EMPTY=\n"
    )
    assert values == {
        "OPENAI_API_KEY": "sk-one",
        "GEMINI_API_KEY": "g-two",
        "ANTHROPIC_API_KEY": "a three",
        "MY_OTHER_VAR": "plain",
        "EMPTY": "",
    }


def test_a_bad_line_names_the_line_but_never_shows_its_content():
    with pytest.raises(ValueError) as e:
        env.parse_env("OPENAI_API_KEY=ok\nthis is secret-ish text\n")
    assert "line 2" in str(e.value) and "secret" not in str(e.value)
    with pytest.raises(ValueError, match="not closed"):
        env.parse_env("A='oops\n")
    with pytest.raises(ValueError, match="after a quoted value"):
        env.parse_env("A='x' y\n")


def test_load_env_sets_missing_variables_and_returns_only_names(tmp_path, monkeypatch):
    f = tmp_path / ".env"
    f.write_text("OPENAI_API_KEY=sk-from-file\nGEMINI_API_KEY=g-from-file\n", encoding="utf-8")
    monkeypatch.setenv("GEMINI_API_KEY", "already-set")
    names = load_env(f)
    assert names == ["OPENAI_API_KEY"]  # names, never values
    assert os.environ["OPENAI_API_KEY"] == "sk-from-file"
    assert os.environ["GEMINI_API_KEY"] == "already-set"  # not replaced
    assert load_env(f, override=True) == ["GEMINI_API_KEY", "OPENAI_API_KEY"]
    assert os.environ["GEMINI_API_KEY"] == "g-from-file"


def test_a_missing_file_is_fine_unless_required(tmp_path):
    assert load_env(tmp_path / "nope.env") == []
    with pytest.raises(FileNotFoundError):
        load_env(tmp_path / "nope.env", required=True)


def test_a_huge_file_is_refused(tmp_path):
    f = tmp_path / ".env"
    f.write_bytes(b"A=" + b"x" * 1_100_000)
    with pytest.raises(ValueError, match="not an environment file"):
        load_env(f)


def test_several_providers_side_by_side_and_status(tmp_path):
    f = tmp_path / ".env"
    f.write_text("OPENAI_API_KEY=o\nANTHROPIC_API_KEY=a\n", encoding="utf-8")
    load_env(f)
    assert env.status() == {name: name in ("openai", "anthropic") for name in _KEYS}
    assert OpenAI("gpt-5-mini").spec()["api_key"] == "o"
    assert env.key_name("gemini") == "GEMINI_API_KEY"
    with pytest.raises(ValueError, match="unknown provider"):
        env.key_name("nope")


def test_a_missing_key_says_which_variable_to_set_and_never_leaks_others(monkeypatch):
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-ant-very-secret")
    with pytest.raises(ValueError) as e:
        OpenAI("gpt-5-mini").spec()
    text = str(e.value)
    assert "OPENAI_API_KEY" in text and ".env" in text and "sk-ant-very-secret" not in text
    # a custom base_url (a gateway or local server) may need no key
    assert OpenAI("gpt-5-mini", base_url="http://127.0.0.1:9").spec()["base_url"] == "http://127.0.0.1:9"
