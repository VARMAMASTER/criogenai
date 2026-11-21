// The download count on the home page, with the open-source note
// (lib/installs.ts reads the counts at build time).
import type { Installs } from "@/lib/installs";

export default function InstallCounter({ installs }: { installs: Installs }) {
  return (
    <aside className="install-goal glass" aria-label="Downloads">
      {installs.status === "counted" ? (
        <p className="install-goal-figure">
          <span className="install-goal-number">{installs.total.toLocaleString("en-US")}</span>
          <span className="install-goal-unit">downloads so far</span>
        </p>
      ) : (
        <p className="install-goal-figure">
          <span className="install-goal-unit">
            {installs.status === "unpublished" ? "Counting starts with the first release" : "Downloads could not be counted for this build"}
          </span>
        </p>
      )}
      <p className="install-goal-note">
        {installs.status === "counted" &&
          `PyPI ${installs.pypi.toLocaleString("en-US")}, npm ${installs.npm.toLocaleString("en-US")}, counted on ${installs.asOf}. `}
        Open source, MIT or Apache-2.0.
      </p>
    </aside>
  );
}
