"""Builds the public benchmark kit (design 0016 §5): the harness, the
framework adapters and the method, without any of AgentBuild's code (the
AgentBuild adapter uses only its public Python API, installed from PyPI).

    python benches/compare/package.py [out.zip]

Default output: website/public/bench/agentbuild-compare-kit.zip.
"""

import sys
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
FILES = {
    "setup.py": HERE / "setup.py",
    "run.py": HERE / "run.py",
    "child.py": HERE / "child.py",
    "common.py": HERE / "common.py",
    "frameworks/fw_agentbuild.py": HERE / "frameworks" / "fw_agentbuild.py",
    "frameworks/fw_langgraph.py": HERE / "frameworks" / "fw_langgraph.py",
    "frameworks/fw_crewai.py": HERE / "frameworks" / "fw_crewai.py",
    "README.md": HERE / "KIT_README.md",
    "METHOD.md": ROOT / "docs" / "design" / "0016-framework-comparison.md",
}


def main() -> None:
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "website" / "public" / "bench" / "agentbuild-compare-kit.zip"
    out.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as kit:
        for name, source in FILES.items():
            # A fixed timestamp: the same sources give the same zip.
            info = zipfile.ZipInfo(f"agentbuild-compare-kit/{name}", date_time=(2026, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            kit.writestr(info, source.read_text(encoding="utf-8").replace("\r\n", "\n"))
    print(f"wrote {out} ({out.stat().st_size // 1024} KB, {len(FILES)} files)")


if __name__ == "__main__":
    main()
