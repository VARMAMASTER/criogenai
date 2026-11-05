// The Criogen mark (lib/mark.ts): a hex-cell rocket with an amber core and
// flame. Colours follow the theme (--ab-running, --ab-waiting). On load the
// outline draws itself, the core pops in and the flame lights; hovering the
// brand link plays it again (base.css). Pure CSS: no script, and reduced
// motion shows the finished mark.
import { MARK } from "@/lib/mark";

export default function LogoMark({ size = 26 }: { size?: number }) {
  return (
    <svg className="logo-mark" width={size} height={size} viewBox={MARK.viewBox} fill="none" aria-hidden="true">
      <path className="logo-flame" d={MARK.flame} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
      <path className="logo-hex logo-body" pathLength={1} d={MARK.body} strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" />
      <path className="logo-hex" pathLength={1} d={MARK.fins} strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" />
      <circle className="logo-core" cx={MARK.core.cx} cy={MARK.core.cy} r={MARK.core.r} />
    </svg>
  );
}
