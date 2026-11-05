"use client";
// A pattern's graph as SVG: the active node glows, visited nodes stay
// tinted, and the edge just taken is drawn as moving dashes.
import type { Graph, GNode } from "@/lib/patterns/engine";

const W = 112;
const H = 38;

function anchor(a: GNode, b: GNode): [number, number] {
  // Where the line from a's centre towards b leaves a's box.
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  if (dx === 0 && dy === 0) return [a.x, a.y];
  const sx = Math.abs(dx) / (W / 2);
  const sy = Math.abs(dy) / (H / 2);
  const s = Math.max(sx, sy);
  return [a.x + dx / s, a.y + dy / s];
}

export default function Diagram({
  graph,
  active,
  edge,
  visited,
  title,
}: {
  graph: Graph;
  active: string | null;
  edge: [string, string] | null;
  visited: Set<string>;
  title: string;
}) {
  const byId = new Map(graph.nodes.map((n) => [n.id, n]));
  const pairs = new Set(graph.edges.map(([a, b]) => `${a}>${b}`));
  return (
    <svg className="sb-diagram" viewBox={`0 0 ${graph.w} ${graph.h}`} role="img" aria-label={`${title}: diagram`}>
      <defs>
        <marker id="sb-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
          <path d="M0,0 L10,5 L0,10 z" className="sb-arrowhead" />
        </marker>
        <marker id="sb-arrow-on" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
          <path d="M0,0 L10,5 L0,10 z" className="sb-arrowhead-on" />
        </marker>
      </defs>
      {graph.edges.map(([from, to, label], i) => {
        const a = byId.get(from);
        const b = byId.get(to);
        if (!a || !b) return null;
        // Two-way pairs bend apart so both arrows show.
        const twoWay = pairs.has(`${to}>${from}`);
        const [x1, y1] = anchor(a, b);
        const [x2, y2] = anchor(b, a);
        const mx = (x1 + x2) / 2;
        const my = (y1 + y2) / 2;
        const len = Math.hypot(x2 - x1, y2 - y1) || 1;
        const bend = twoWay ? 14 : 0;
        const cx = mx + (-(y2 - y1) / len) * bend;
        const cy = my + ((x2 - x1) / len) * bend;
        const on = edge?.[0] === from && edge?.[1] === to;
        return (
          <g key={i} className={on ? "sb-edge sb-edge-on" : "sb-edge"}>
            <path d={`M${x1},${y1} Q${cx},${cy} ${x2},${y2}`} markerEnd={on ? "url(#sb-arrow-on)" : "url(#sb-arrow)"} />
            {label && (
              <text x={cx} y={cy - 5} textAnchor="middle" className="sb-edge-label">
                {label}
              </text>
            )}
          </g>
        );
      })}
      {graph.nodes.map((n) => (
        <g
          key={n.id}
          className={`sb-node sb-k-${n.kind ?? "logic"}${active === n.id ? " sb-on" : ""}${visited.has(n.id) ? " sb-seen" : ""}`}
          transform={`translate(${n.x - W / 2},${n.y - H / 2})`}
        >
          <rect width={W} height={H} rx={n.kind === "io" ? 19 : 9} />
          <text x={W / 2} y={H / 2 + 4.5} textAnchor="middle">
            {n.label}
          </text>
        </g>
      ))}
    </svg>
  );
}
