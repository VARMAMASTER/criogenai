"""Creates one virtual environment per framework for the comparison
(design 0016 §1): their dependencies conflict, and each venv's size is the
install footprint.

    python benches/compare/setup.py [--agentbuild-wheel PATH]

Venvs go to target/compare/venvs/<framework> (in the benchmark kit:
venvs/<framework> next to this file). AgentBuild comes from a local wheel
when there is one (the newest in target/wheels, or the one given), else
from PyPI.
"""

import argparse
import glob
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
# In the repository the venvs go to target/; in the downloaded kit, next to it.
ROOT = HERE.parents[1] if (HERE.parents[1] / "Cargo.toml").exists() else HERE
VENVS = ROOT / "target" / "compare" / "venvs" if ROOT != HERE else HERE / "venvs"
# AgentBuild's Python package on PyPI ("agentbuild" there is another
# project's; ours is agentbuild-sdk, imported as `agentbuild`).
AGENTBUILD = "agentbuild-sdk==0.1.0"

# Pinned: the latest stable releases when the comparison was set up.
PSUTIL = "psutil==7.2.2"
PACKAGES = {
    "langgraph": ["langgraph==1.2.16", "langchain==1.4.6", "langgraph-checkpoint-sqlite==3.1.2", PSUTIL],
    "crewai": ["crewai==1.15.28", PSUTIL],
}


def python_of(venv: Path) -> Path:
    windows = venv / "Scripts" / "python.exe"
    return windows if windows.exists() else venv / "bin" / "python"


def create(name: str, packages: list, isolated: bool = False) -> None:
    venv = VENVS / name
    if not python_of(venv).exists():
        print(f"[{name}] creating {venv}", flush=True)
        subprocess.run([sys.executable, "-m", "venv", str(venv)], check=True)
    print(f"[{name}] installing {' '.join(packages)}", flush=True)
    subprocess.run(
        [str(python_of(venv)), "-m", "pip", "install", "--quiet", "--disable-pip-version-check", *packages],
        check=True,
        # pip's documented switch for "read no configuration file".
        env={**os.environ, "PIP_CONFIG_FILE": os.devnull} if isolated else None,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--agentbuild-wheel", help="the AgentBuild wheel to install")
    parser.add_argument("--only", choices=["agentbuild", *PACKAGES], help="set up one framework")
    parser.add_argument("--pip-isolated", action="store_true", help="read no pip configuration file (e.g. one with a broken extra index)")
    args = parser.parse_args()

    wheel = args.agentbuild_wheel
    if wheel is None:
        wheels = sorted(glob.glob(str(ROOT / "target" / "wheels" / "agentbuild_sdk-*.whl")), key=os.path.getmtime)
        wheel = wheels[-1] if wheels else None
    source = wheel or AGENTBUILD
    plan = {"agentbuild": [source, PSUTIL] if source else None, **PACKAGES}
    for name, packages in plan.items():
        if args.only and name != args.only:
            continue
        if packages is None:
            print("[agentbuild] skipped: no AgentBuild wheel found; pass --agentbuild-wheel PATH", flush=True)
            continue
        create(name, packages, args.pip_isolated)
    print("ready:", ", ".join(str(python_of(VENVS / n)) for n in plan if not args.only or n == args.only))


if __name__ == "__main__":
    main()
