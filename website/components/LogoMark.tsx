// The CriogenAI mark (lib/mark.ts): a hex-cell rocket with an amber core and
// flame. Colours follow the theme (--ab-running, --ab-waiting). On load the
// outline draws itself, the core pops in and the engine ignites; after that
// the flame flickers. Hovering the brand link launches it: the rocket shakes
// and lifts, the flame roars and exhaust puffs fall away (base.css). Pure CSS:
// no script, and reduced motion shows the finished, still mark.
import { MARK } from "@/lib/mark";

export default function LogoMark({ size = 26 }: { size?: number }) {
  return (
    <svg className="logo-mark" width={size} height={size} viewBox={MARK.viewBox} fill="none" aria-hidden="true">
      <g className="logo-smoke">
        <circle cx="13.6" cy="29" r="1.3" />
        <circle cx="18.4" cy="29.5" r="1.1" />
        <circle cx="16" cy="30.5" r="1.5" />
      </g>
      <g className="logo-rocket">
        <g className="logo-ignite">
          <path className="logo-flame" d={MARK.flame} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
          <path className="logo-flame-core" d={MARK.flameCore} />
        </g>
        <path className="logo-hex logo-body" pathLength={1} d={MARK.body} strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" />
        <path className="logo-hex" pathLength={1} d={MARK.fins} strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" />
        <circle className="logo-core" cx={MARK.core.cx} cy={MARK.core.cy} r={MARK.core.r} />
      </g>
    </svg>
  );
}
