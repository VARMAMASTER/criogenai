"use client";
// Shared pieces of the benchmark lab.
import { useEffect, useRef, useState } from "react";
import { colorOf, labelOf, type BenchFile, type BenchResult } from "@/lib/bench";

export interface DataSet {
  key: "ours" | "yours";
  /** Shown next to a framework when both sets are on the page. */
  tag: string;
  data: BenchFile;
}

export interface Lane {
  fw: string;
  set: DataSet;
  r: BenchResult;
}

export function lanesFor(sets: DataSet[], fws: string[], scenario: string): Lane[] {
  const out: Lane[] = [];
  for (const fw of fws)
    for (const set of sets) {
      const r = set.data.results.find((x) => x.scenario === scenario && x.framework === fw);
      if (r) out.push({ fw, set, r });
    }
  return out;
}

export function FwName({ fw, set, sets, files }: { fw: string; set?: DataSet; sets: DataSet[]; files: BenchFile[] }) {
  const mine = set?.key === "yours";
  return (
    <span className="fw-name">
      <span className={mine ? "swatch swatch-yours" : "swatch"} style={{ ["--c" as string]: colorOf(fw) }} aria-hidden="true" />
      <span>{labelOf(files, fw)}</span>
      {set && sets.length > 1 && <span className={mine ? "set-tag set-yours" : "set-tag"}>{set.tag}</span>}
    </span>
  );
}

/** The width of an element, kept current (for SVG drawn in pixels). */
export function useWidth<T extends HTMLElement>(): [React.RefObject<T | null>, number] {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    setWidth(el.clientWidth);
    const ro = new ResizeObserver(() => setWidth(el.clientWidth));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, width];
}

export function useReducedMotion(): boolean {
  const [reduce, setReduce] = useState(false);
  useEffect(() => {
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
    setReduce(mq.matches);
    const on = () => setReduce(mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return reduce;
}

/** Nice round ticks for a linear or log axis. */
export function ticks(lo: number, hi: number, log: boolean, count = 5): number[] {
  if (!(hi > lo)) return [lo];
  if (log) {
    const out: number[] = [];
    for (let e = Math.floor(Math.log10(lo)); e <= Math.ceil(Math.log10(hi)); e++)
      for (const m of [1, 2, 5]) {
        const v = m * 10 ** e;
        if (v >= lo && v <= hi) out.push(v);
      }
    return out.length > 7 ? out.filter((v) => Math.abs(Math.log10(v) % 1) < 1e-9) : out;
  }
  const raw = (hi - lo) / count;
  const mag = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? raw;
  const out: number[] = [];
  for (let v = Math.ceil(lo / step) * step; v <= hi + step * 1e-9; v += step) out.push(Number(v.toPrecision(12)));
  return out;
}
