"use client";
// The two measured numbers of the landing page as bars that grow when they
// scroll into view. Same figures as the table below them, drawn to scale.
import { animate, motion, useInView, useReducedMotion } from "motion/react";
import { useEffect, useRef, useState } from "react";

export type Bench = { label: string; ours: number; theirs: number; unit: string };

function Count({ to, digits = 1 }: { to: number; digits?: number }) {
  const ref = useRef<HTMLSpanElement>(null);
  const seen = useInView(ref, { once: true });
  const reduce = useReducedMotion();
  const [value, setValue] = useState(to);
  useEffect(() => {
    if (!seen || reduce) return;
    const controls = animate(0, to, { duration: 1.1, ease: "easeOut", onUpdate: setValue });
    return () => controls.stop();
  }, [seen, reduce, to]);
  return <span ref={ref}>{value.toFixed(digits)}</span>;
}

export default function BenchBars({ rows }: { rows: Bench[] }) {
  const reduce = useReducedMotion();
  return (
    <div className="bars" role="group" aria-label="Time per step, drawn to scale; shorter is faster">
      {rows.map((row, i) => {
        const ratio = row.theirs / row.ours;
        const ours = Math.max((row.ours / row.theirs) * 100, 1.5);
        return (
          <div className="bars-group" key={row.label}>
            <h3>{row.label}</h3>
            <div className="bar-row">
              <span className="bar-name">AgentBuild</span>
              <div className="bar-track">
                <motion.span
                  className="bar bar-ours"
                  initial={reduce ? false : { width: 0 }}
                  whileInView={{ width: `${ours}%` }}
                  viewport={{ once: true }}
                  transition={{ duration: 0.9, delay: 0.1 + i * 0.12, ease: [0.22, 1, 0.36, 1] }}
                />
              </div>
              <span className="bar-value">
                <Count to={row.ours} /> {row.unit}
              </span>
            </div>
            <div className="bar-row">
              <span className="bar-name">LangGraph</span>
              <div className="bar-track">
                <motion.span
                  className="bar bar-theirs"
                  initial={reduce ? false : { width: 0 }}
                  whileInView={{ width: "100%" }}
                  viewport={{ once: true }}
                  transition={{ duration: 0.9, delay: 0.2 + i * 0.12, ease: [0.22, 1, 0.36, 1] }}
                />
              </div>
              <span className="bar-value">
                <Count to={row.theirs} digits={0} /> {row.unit}
              </span>
            </div>
            <p className="bar-ratio">
              <b>{ratio.toFixed(0)}×</b> less framework time per {row.label.toLowerCase()}
            </p>
          </div>
        );
      })}
    </div>
  );
}
