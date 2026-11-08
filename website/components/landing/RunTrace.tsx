"use client";
// The landing page's hero: one agent run, step by step, in the run-state
// colours. It plays once (Replay plays it again); without JavaScript or with
// reduced motion the finished run shows. `data-phase` (running, waiting,
// done) colours the light behind the glass (styles/landing.css).
import { useEffect, useRef, type CSSProperties } from "react";

export default function RunTrace() {
  const ref = useRef<HTMLElement>(null);

  useEffect(() => {
    const trace = ref.current;
    if (!trace || window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    let timers: number[] = [];
    const later = (ms: number, fn: () => void) => timers.push(window.setTimeout(fn, ms));
    const play = () => {
      timers.forEach(clearTimeout);
      timers = [];
      trace.querySelectorAll<HTMLElement>("[data-at], .saved[data-after]").forEach((el) => el.classList.remove("on"));
      trace.querySelectorAll(".step-wait").forEach((el) => el.classList.remove("waiting"));
      trace.classList.add("playing");
      trace.dataset.phase = "running";
      for (const el of trace.querySelectorAll<HTMLElement>("[data-at]")) {
        const at = Number(el.dataset.at);
        later(at, () => el.classList.add("on"));
        const wait = Number(el.dataset.wait ?? 0);
        if (wait) {
          later(at, () => {
            el.classList.add("waiting");
            trace.dataset.phase = "waiting";
          });
          later(at + wait, () => {
            el.classList.remove("waiting");
            trace.dataset.phase = "running";
          });
        }
        const saved = el.querySelector<HTMLElement>(".saved[data-after]");
        if (saved) later(at + Number(saved.dataset.after), () => saved.classList.add("on"));
      }
      later(7000, () => {
        trace.dataset.phase = "done";
      });
      later(7200, () => trace.classList.remove("playing"));
    };
    const button = trace.querySelector(".replay");
    button?.addEventListener("click", play);
    play();
    return () => {
      button?.removeEventListener("click", play);
      timers.forEach(clearTimeout);
    };
  }, []);

  const d = (s: string) => ({ "--d": s }) as CSSProperties;

  return (
    <figure
      className="trace"
      data-phase="done"
      ref={ref}
      aria-label="An agent run: the model calls three tools at once, a refund waits for a person's approval, then the run finishes. Every step is saved."
    >
      <header className="trace-head">
        <span className="dot"></span>
        <span>
          thread <b>ticket-7</b>
        </span>
        <span className="trace-actor">actor user:dr-rao</span>
      </header>

      <ol className="steps">
        <li className="step" data-at="200">
          <span className="step-n">step 1</span>
          <div className="lane">
            <span className="call model">model</span>
            <span className="note">wants the order and the refund policy</span>
          </div>
          <span className="saved">saved v1</span>
        </li>

        <li className="step" data-at="1300">
          <span className="step-n">step 2</span>
          <div className="lane lane-parallel">
            <span className="call tool" style={d("0.7s")}>
              get_order
            </span>
            <span className="call tool" style={d("1.1s")}>
              check_refund_policy
            </span>
            <span className="call tool" style={d("0.5s")}>
              get_customer
            </span>
          </div>
          <span className="saved" data-after="1200">
            saved v2
          </span>
        </li>

        <li className="step step-wait" data-at="2900" data-wait="2600">
          <span className="step-n">step 3</span>
          <div className="lane">
            <span className="call tool approval">issue_refund</span>
            <span className="note note-wait">waiting for a person</span>
            <span className="note note-ok">approved by user:dr-rao</span>
          </div>
          <span className="saved" data-after="2700">
            saved v3
          </span>
        </li>

        <li className="step" data-at="6000">
          <span className="step-n">step 4</span>
          <div className="lane">
            <span className="call model">model</span>
            <span className="note">&quot;Your refund of 40.00 is on its way.&quot;</span>
          </div>
          <span className="saved" data-after="500">
            saved v4
          </span>
        </li>
      </ol>

      <footer className="trace-foot" data-at="7000">
        <span className="dot"></span>
        <span>completed in 4 steps, audit chain verified</span>
        <button type="button" className="replay">
          Replay
        </button>
      </footer>
    </figure>
  );
}
