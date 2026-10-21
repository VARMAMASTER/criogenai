"use client";
// An animated run: an SVG graph whose nodes and edges take the run-state
// colours as a timeline plays, with a live log beside it. A beat can ask the
// viewer to approve or reject (the run waits; it approves itself after a
// while). Plays when visible and loops; Pause stops it. Without JavaScript or
// with reduced motion it shows the final frame.
//
// The markup is rendered once by React; the timeline then sets attributes
// directly (data-state, log lines), as the original script did, so nothing
// re-renders while it plays.
import { useEffect, useRef } from "react";
import type { Beat, SceneEdge, SceneNode } from "./scene-types";
import type { SceneData } from "./scenes";

const NODE_H = 40;
const ASK_WAIT = 4500; // the run approves itself after this long
const REST = 3500; // the final frame, before it plays again

const widthOf = (n: SceneNode) => Math.max(84, n.label.length * 8.4 + 30);
const rounded = (n: SceneNode) => n.kind === "model" || n.kind === "person";

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);
}

export default function Scene({ id, title, description, nodes, edges, beats, width, height }: SceneData) {
  const root = useRef<HTMLElement>(null);
  const byId = Object.fromEntries(nodes.map((n) => [n.id, n]));
  const edgePath = (e: SceneEdge) => {
    const a = byId[e.from];
    const b = byId[e.to];
    const ax = a.x + widthOf(a) / 2;
    const bx = b.x - widthOf(b) / 2;
    const mx = (ax + bx) / 2;
    return `M ${ax} ${a.y} C ${mx} ${a.y}, ${mx} ${b.y}, ${bx} ${b.y}`;
  };

  // The final log: the main line, taking the approve branch at a question.
  const finalLog: Beat[] = [];
  for (const b of beats) {
    if (b.log) finalLog.push(b);
    if (b.ask) finalLog.push(...b.ask.approve.filter((x) => x.log));
  }
  const finalLogHtml = finalLog.map((b) => `<li data-tone="${b.tone ?? "note"}">${escapeHtml(b.log!)}</li>`).join("");

  useEffect(() => {
    const scene = root.current;
    if (!scene) return;
    const parts = new Map<string, Element>();
    scene.querySelectorAll<SVGElement>("[data-id]").forEach((el) => parts.set(el.dataset.id!, el));
    const finals = new Map<string, string>();
    parts.forEach((el, key) => finals.set(key, el.getAttribute("data-state")!));
    const log = scene.querySelector<HTMLOListElement>(".scene-log ol")!;
    const flash = scene.querySelector<HTMLElement>(".scene-flash")!;
    const ask = scene.querySelector<HTMLElement>(".scene-ask")!;
    const askPrompt = ask.querySelector<HTMLElement>(".scene-ask-prompt")!;
    const askTimer = ask.querySelector<HTMLElement>(".scene-ask-timer")!;
    const toggle = scene.querySelector<HTMLButtonElement>(".scene-toggle")!;
    const scroller = scene.querySelector<HTMLElement>(".scene-scroll")!;

    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      toggle.hidden = true;
      return; // the final frame stays
    }

    // On narrow screens the graph scrolls sideways: keep the active node in view
    // (the stage scrolls, never the page).
    const follow = () => {
      if (scroller.scrollWidth <= scroller.clientWidth) return;
      const active = scene.querySelector('.node[data-state="run"], .node[data-state="wait"]');
      if (!active) return;
      const node = active.getBoundingClientRect();
      const view = scroller.getBoundingClientRect();
      scroller.scrollBy({ left: node.left + node.width / 2 - (view.left + view.width / 2), behavior: "smooth" });
    };

    // Keep the height steady while the log fills.
    log.style.minHeight = `${log.offsetHeight}px`;

    let timers: number[] = [];
    let playing = false;
    let paused = false;
    let visible = false;
    let choose: ((choice: "approve" | "reject") => void) | null = null;

    const later = (ms: number, fn: () => void) => timers.push(window.setTimeout(fn, ms));
    const clear = () => {
      timers.forEach(clearTimeout);
      timers = [];
    };
    const stop = () => {
      clear();
      playing = false;
      choose = null;
      ask.hidden = true;
    };
    const showFinal = () => {
      parts.forEach((el, key) => el.setAttribute("data-state", finals.get(key)!));
      log.innerHTML = finalLogHtml;
    };
    const apply = (beat: Beat) => {
      for (const [key, state] of Object.entries(beat.set ?? {})) parts.get(key)?.setAttribute("data-state", state);
      if (beat.set) follow();
      if (beat.log) {
        const li = document.createElement("li");
        li.dataset.tone = beat.tone ?? "note";
        li.textContent = beat.log;
        log.append(li);
      }
      if (beat.flash) {
        flash.firstElementChild!.textContent = beat.flash;
        flash.classList.remove("on");
        void flash.offsetWidth;
        flash.classList.add("on");
      }
    };
    // Run a list of beats from now; at a question, wait for the viewer.
    const schedule = (list: Beat[]) => {
      let end = 0;
      for (const beat of list) {
        end = Math.max(end, beat.at);
        if (beat.ask) {
          const question = beat.ask;
          later(beat.at, () => {
            apply(beat);
            askPrompt.textContent = question.prompt;
            ask.hidden = false;
            askTimer.animate([{ transform: "scaleX(1)" }, { transform: "scaleX(0)" }], { duration: ASK_WAIT, fill: "forwards" });
            choose = (choice) => {
              choose = null;
              ask.hidden = true;
              clear();
              schedule(question[choice]);
            };
            later(ASK_WAIT, () => choose?.("approve"));
          });
          return; // the rest comes from the branch
        }
        later(beat.at, () => apply(beat));
      }
      later(end + REST, () => {
        playing = false;
        if (visible && !paused) play();
      });
    };
    const play = () => {
      if (playing || paused) return;
      playing = true;
      parts.forEach((el) => el.setAttribute("data-state", "idle"));
      log.innerHTML = "";
      schedule(beats);
    };

    const onAsk = (e: MouseEvent) => {
      const choice = (e.target as HTMLElement).closest<HTMLButtonElement>("[data-choice]")?.dataset.choice;
      if (choice === "approve" || choice === "reject") choose?.(choice);
    };
    const onToggle = () => {
      paused = !paused;
      toggle.setAttribute("aria-pressed", String(paused));
      toggle.textContent = paused ? "Play" : "Pause";
      if (paused) {
        stop();
        showFinal();
      } else if (visible) {
        play();
      }
    };
    ask.addEventListener("click", onAsk);
    toggle.addEventListener("click", onToggle);
    const observer = new IntersectionObserver(
      (entries) => {
        visible = entries.some((e) => e.isIntersecting);
        if (visible) play();
      },
      { threshold: 0.4 },
    );
    observer.observe(scene);
    return () => {
      observer.disconnect();
      ask.removeEventListener("click", onAsk);
      toggle.removeEventListener("click", onToggle);
      clear();
    };
  }, [beats, finalLogHtml]);

  return (
    <figure className="scene" id={id} ref={root}>
      <div className="scene-stage">
        <div className="scene-scroll">
          <svg viewBox={`0 0 ${width} ${height}`} role="img" aria-label={description}>
            <defs>
              <filter id={`${id}-glow`} x="-50%" y="-50%" width="200%" height="200%">
                <feGaussianBlur stdDeviation="5" />
              </filter>
            </defs>
            {edges.map((e) => (
              <g key={e.id} className="edge" data-id={e.id} data-state={e.final ?? "done"}>
                <path className="edge-line" d={edgePath(e)} />
                <path className="edge-flow" d={edgePath(e)} />
              </g>
            ))}
            {nodes.map((n) => (
              <g key={n.id} className={`node kind-${n.kind ?? "tool"}`} data-id={n.id} data-state={n.final ?? "done"} transform={`translate(${n.x} ${n.y})`}>
                <rect
                  className="node-halo"
                  x={-widthOf(n) / 2 - 3}
                  y={-NODE_H / 2 - 3}
                  width={widthOf(n) + 6}
                  height={NODE_H + 6}
                  rx={rounded(n) ? 23 : 13}
                  filter={`url(#${id}-glow)`}
                />
                <rect className="node-box" x={-widthOf(n) / 2} y={-NODE_H / 2} width={widthOf(n)} height={NODE_H} rx={rounded(n) ? 20 : 10} />
                <text className="node-label" textAnchor="middle" dominantBaseline="central">
                  {n.label}
                </text>
              </g>
            ))}
          </svg>
        </div>
        <div className="scene-flash" aria-hidden="true">
          <span></span>
        </div>
        <div className="scene-ask" hidden>
          <p className="scene-ask-prompt"></p>
          <div className="scene-ask-buttons">
            <button type="button" data-choice="approve">
              Approve
            </button>
            <button type="button" data-choice="reject">
              Reject
            </button>
          </div>
          <span className="scene-ask-timer" aria-hidden="true"></span>
        </div>
      </div>
      <figcaption className="scene-log">
        <div className="scene-log-head">
          <span>{title}</span>
          <button type="button" className="scene-toggle" aria-pressed="false">
            Pause
          </button>
        </div>
        <ol dangerouslySetInnerHTML={{ __html: finalLogHtml }} />
      </figcaption>
    </figure>
  );
}
