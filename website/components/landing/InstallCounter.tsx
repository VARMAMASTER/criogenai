// The open-source note on the home page, with the live install count
// (lib/installs.ts reads the counts at build time).
import type { Installs } from "@/lib/installs";

export const REPO = "https://github.com/VARMAMASTER/criogenai";

export default function InstallCounter({ installs }: { installs: Installs }) {
  const label =
    installs.status === "counted"
      ? `${installs.total.toLocaleString("en-US")} installs so far`
      : installs.status === "unpublished"
        ? "Counting starts with the first release"
        : "Installs could not be counted for this build";
  return (
    <aside className="install-goal glass" aria-label="Open source">
      <div className="install-goal-head">
        <span className="install-goal-title">Open source, MIT or Apache-2.0</span>
        <span className="install-goal-count">{label}</span>
      </div>
      <p className="install-goal-note">
        Read the code, open an issue or send a change on <a href={REPO}>GitHub</a>.
        {installs.status === "counted" &&
          ` Counted from PyPI (${installs.pypi.toLocaleString("en-US")}) and npm (${installs.npm.toLocaleString("en-US")}) on ${installs.asOf}.`}
      </p>
    </aside>
  );
}
