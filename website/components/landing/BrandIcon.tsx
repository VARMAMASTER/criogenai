// A maker's icon from Simple Icons (logos.ts), or, when there is none, a
// plain initial in a ring: never a drawn imitation of someone's logo.
import { logoFor } from "./logos";

export default function BrandIcon({ slug, name, className = "" }: { slug?: string; name: string; className?: string }) {
  const logo = logoFor(slug);
  if (logo)
    return (
      <svg viewBox="0 0 24 24" aria-hidden="true" className={`brand-icon ${className}`}>
        <path d={logo.path} />
      </svg>
    );
  return (
    <span aria-hidden="true" className={`brand-icon brand-initial ${className}`}>
      {name.replace(/^[^A-Za-z0-9]+/, "").charAt(0).toUpperCase()}
    </span>
  );
}
