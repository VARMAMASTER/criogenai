"use client";
// A large, faint Criogen mark behind every page. It draws itself in on load,
// breathes slowly, and drifts and turns a little as the page scrolls. It is
// decoration only: no pointer events, hidden from assistive technology, and
// still (and just as faint) when the visitor asks for reduced motion.
import { motion, useReducedMotion, useScroll, useSpring, useTransform } from "motion/react";
import { MARK } from "@/lib/mark";

export default function Watermark() {
  const reduce = useReducedMotion();
  const { scrollYProgress } = useScroll();
  const smooth = useSpring(scrollYProgress, { stiffness: 60, damping: 20, mass: 0.6 });
  const y = useTransform(smooth, [0, 1], ["0vh", "-18vh"]);
  const rotate = useTransform(smooth, [0, 1], [0, 14]);
  return (
    <div className="watermark" aria-hidden="true">
      <motion.svg
        className="watermark-mark"
        viewBox="0 0 32 32"
        fill="none"
        style={reduce ? undefined : { y, rotate }}
      >
        <path className="watermark-hex" pathLength={1} d={MARK.body} strokeWidth="0.9" strokeLinecap="round" strokeLinejoin="round" />
        <path className="watermark-hex" pathLength={1} d={MARK.fins} strokeWidth="0.8" strokeLinecap="round" strokeLinejoin="round" />
        <path className="watermark-flame" d={MARK.flame} strokeWidth="0.8" strokeLinecap="round" strokeLinejoin="round" />
        <circle className="watermark-core" cx={MARK.core.cx} cy={MARK.core.cy} r={MARK.core.r * 0.75} />
      </motion.svg>
    </div>
  );
}
