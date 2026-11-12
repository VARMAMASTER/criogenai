"""The version the package reports is the version that was installed."""

from importlib.metadata import version

import criogenai


def test_version_matches_the_installed_package() -> None:
    # __version__ comes from the Rust crate (Cargo.toml), the wheel's version
    # from pyproject.toml: a release that bumps only one of them fails here.
    assert criogenai.__version__ == version("criogenai")
