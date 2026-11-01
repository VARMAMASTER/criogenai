import type { ReactNode } from "react";
import Link from "next/link";
import { Arrow } from "./icons";

export function CardGrid({ children }: { children?: ReactNode }) {
  return <div className="card-grid">{children}</div>;
}

export function LinkCard({ title, description, href }: { title: string; description?: string; href: string }) {
  const inner = (
    <>
      <span className="link-card-text">
        <span className="link-card-title">{title}</span>
        {description && <span className="link-card-desc">{description}</span>}
      </span>
      <Arrow className="link-card-arrow" />
    </>
  );
  // The generated API references (/api/...) are separate static sites, not pages of this app.
  if (href.startsWith("/api/") || !href.startsWith("/"))
    return (
      <a className="link-card" href={href}>
        {inner}
      </a>
    );
  return (
    <Link className="link-card" href={href}>
      {inner}
    </Link>
  );
}
