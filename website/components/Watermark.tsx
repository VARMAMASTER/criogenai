"use client";
// A large, faint Criogen mark behind every page. It draws itself in on load,
// breathes slowly, and drifts and turns a little as the page scrolls. It is
// decoration only: no pointer events, hidden from assistive technology, and
// still (and just as faint) when the visitor asks for reduced motion.
import { motion, useReducedMotion, useScroll, useSpring, useTransform } from "motion/react";

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
        <path
          className="watermark-hex"
          pathLength={1}
          d="M22 6.2 H10.4 L4.6 16 L10.4 25.8 H22"
          strokeWidth="1.1"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
        <circle className="watermark-core" cx="16.5" cy="16" r="2.2" />
      </motion.svg>
    </div>
  );
}
