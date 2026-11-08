"use client";
// A pattern's graph in the home page's scene style (styles/scene.css): each
// node and edge takes its run state's colour (running, waiting for you,
// done, failed); the node at work glows and the edge being taken flows.
// Edges leave a node towards the other one, so back edges and loops draw
// as well as forward ones.
import type { Graph, GNode } from "@/lib/patterns/engine";

export type RunState = "idle" | "run" | "wait" | "done" | "fail";

const H = 40;
const widthOf = (n: GNode) => Math.max(96, n.label.length * 8.4 + 32);
const rounded = (n: GNode) => n.kind === "model" || n.kind === "human" || n.kind === "io";

function anchor(a: GNode, b: GNode): [number, number] {
  // Where the line from a's centre towards b leaves a's box.
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  if (dx === 0 && dy === 0) return [a.x, a.y];
  const s = Math.max(Math.abs(dx) / (widthOf(a) / 2), Math.abs(dy) / (H / 2));
  return [a.x + dx / s, a.y + dy / s];
}

export const edgeKey = (from: string, to: string) => `${from}>${to}`;

/** A point of the quadratic curve from (x1, y1) to (x2, y2) through control (cx, cy). */
function at(t: number, x1: number, y1: number, cx: number, cy: number, x2: number, y2: number): [number, number] {
  const u = 1 - t;
  return [u * u * x1 + 2 * u * t * cx + t * t * x2, u * u * y1 + 2 * u * t * cy + t * t * y2];
}

/** Whether the curve passes through a node other than its two ends. */
function crosses(nodes: GNode[], a: GNode, b: GNode, pts: [number, number, number, number, number, number]): boolean {
  for (let i = 1; i < 20; i++) {
    const [x, y] = at(i / 20, ...pts);
    for (const n of nodes) {
      if (n === a || n === b) continue;
      if (Math.abs(x - n.x) < widthOf(n) / 2 + 6 && Math.abs(y - n.y) < H / 2 + 6) return true;
    }
  }
  return false;
}

export default function Diagram({
  graph,
  nodes,
  edges,
  title,
  uid = "pg",
}: {
  graph: Graph;
  /** Each node's state; missing means idle. */
  nodes: Record<string, RunState>;
  /** Each edge's state by `edgeKey(from, to)`; missing means idle. */
  edges: Record<string, RunState>;
  title: string;
  /** Prefix of the SVG ids (several diagrams on one page need their own). */
  uid?: string;
}) {
  const byId = new Map(graph.nodes.map((n) => [n.id, n]));
  const pairs = new Set(graph.edges.map(([a, b]) => edgeKey(a, b)));
  const pad = 30;
  return (
    <svg
      className="pg-graph"
      viewBox={`${-pad} ${-pad} ${graph.w + pad * 2} ${graph.h + pad * 2}`}
      role="img"
      aria-label={`${title}: diagram of the pattern`}
    >
      <defs>
        <filter id={`${uid}-glow`} x="-50%" y="-50%" width="200%" height="200%">
          <feGaussianBlur stdDeviation="5" />
        </filter>
        <marker id={`${uid}-arrow`} viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
          <path d="M0,1 L9,5 L0,9 z" className="pg-arrowhead" />
        </marker>
      </defs>
      {graph.edges.map(([from, to, label]) => {
        const a = byId.get(from);
        const b = byId.get(to);
        if (!a || !b) return null;
        const [x1, y1] = anchor(a, b);
        const [x2, y2] = anchor(b, a);
        const len = Math.hypot(x2 - x1, y2 - y1) || 1;
        const nx = -(y2 - y1) / len;
        const ny = (x2 - x1) / len;
        // Two-way pairs bend apart so both directions show; others curve a
        // little, and arc around any node they would cross.
        const base = pairs.has(edgeKey(to, from)) ? 18 : 6;
        let bend = base;
        for (const tryBend of [base, 45, -45, 80, -80, 120, -120]) {
          bend = tryBend;
          const c: [number, number] = [(x1 + x2) / 2 + nx * bend, (y1 + y2) / 2 + ny * bend];
          if (!crosses(graph.nodes, a, b, [x1, y1, c[0], c[1], x2, y2])) break;
        }
        const cx = (x1 + x2) / 2 + nx * bend;
        const cy = (y1 + y2) / 2 + ny * bend;
        const d = `M${x1},${y1} Q${cx},${cy} ${x2},${y2}`;
        const [lx, ly] = at(0.5, x1, y1, cx, cy, x2, y2);
        const state = edges[edgeKey(from, to)] ?? "idle";
        return (
          <g key={edgeKey(from, to)} className="edge" data-state={state}>
            <path className="edge-line" d={d} markerEnd={`url(#${uid}-arrow)`} />
            <path className="edge-flow" d={d} />
            {label && (
              <text x={lx} y={ly - 6} textAnchor="middle" className="pg-edge-label">
                {label}
              </text>
            )}
          </g>
        );
      })}
      {graph.nodes.map((n) => {
        const w = widthOf(n);
        const r = rounded(n) ? H / 2 : 10;
        return (
          <g key={n.id} className={`node pg-k-${n.kind ?? "logic"}`} data-state={nodes[n.id] ?? "idle"} transform={`translate(${n.x} ${n.y})`}>
            <rect className="node-halo" x={-w / 2 - 3} y={-H / 2 - 3} width={w + 6} height={H + 6} rx={r + 3} filter={`url(#${uid}-glow)`} />
            <rect className="node-box" x={-w / 2} y={-H / 2} width={w} height={H} rx={r} />
            <text className="node-label" textAnchor="middle" dominantBaseline="central">
              {n.label}
            </text>
          </g>
        );
      })}
    </svg>
  );
}
