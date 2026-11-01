// The Criogen mark: a hexagonal ice-crystal cell, open like a C, with a core
// that is a run waiting at a gate. Colours follow the theme (--ab-running,
// --ab-waiting). On load the cell draws itself and the core pops in and
// pulses once; hovering the brand link plays it again (base.css). Pure CSS:
// it needs no script, and reduced motion shows the finished mark.
export default function LogoMark({ size = 26 }: { size?: number }) {
  return (
    <svg className="logo-mark" width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
      <path
        className="logo-hex"
        pathLength={1}
        d="M22 6.2 H10.4 L4.6 16 L10.4 25.8 H22"
        strokeWidth="3.2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <circle className="logo-core" cx="16.5" cy="16" r="3.4" />
    </svg>
  );
}
