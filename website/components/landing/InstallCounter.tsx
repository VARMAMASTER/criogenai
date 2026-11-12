// Progress toward the 10,000 installs at which CriogenAI's code opens
// (lib/installs.ts reads the counts at build time). Remove it once reached.
import { GOAL, type Installs } from "@/lib/installs";

export default function InstallCounter({ installs }: { installs: Installs }) {
  const total = installs.status === "counted" ? installs.total : 0;
  const share = Math.min(total / GOAL, 1);
  const label =
    installs.status === "counted"
      ? `${total.toLocaleString("en-US")} of ${GOAL.toLocaleString("en-US")} installs`
      : installs.status === "unpublished"
        ? `0 of ${GOAL.toLocaleString("en-US")} installs: counting starts with the first release`
        : `Toward ${GOAL.toLocaleString("en-US")} installs`;
  return (
    <aside className="install-goal glass" aria-label="Installs toward opening the code">
      <div className="install-goal-head">
        <span className="install-goal-title">The code opens at 10,000 installs</span>
        <span className="install-goal-count">{label}</span>
      </div>
      <div
        className="install-goal-track"
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={GOAL}
        aria-valuenow={total}
        aria-valuetext={label}
      >
        <span className="install-goal-fill" style={{ width: `${Math.max(share * 100, installs.status === "counted" && total > 0 ? 1 : 0)}%` }} />
      </div>
      <p className="install-goal-note">
        Free to use now. When CriogenAI reaches 10,000 installs, its source code opens under MIT or Apache-2.0.
        {installs.status === "counted" &&
          ` Counted from PyPI (${installs.pypi.toLocaleString("en-US")}) and npm (${installs.npm.toLocaleString("en-US")}) on ${installs.asOf}.`}
        {installs.status === "unavailable" && " The live count could not be read when this page was built."}
      </p>
    </aside>
  );
}
