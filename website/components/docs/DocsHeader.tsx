"use client";
// The docs header, Docusaurus-style: the logo and the main sections on the
// left, search and the theme switch on the right. On narrow screens
// a menu button opens the sidebar as a drawer (a modal dialog: focus stays
// inside, Escape closes it).
import { useEffect, useRef } from "react";
import Link from "next/link";
import { SECTIONS, sectionOf } from "@/lib/nav";
import { Close, Menu } from "../icons";
import LogoMark from "../LogoMark";
import Search from "../Search";
import ThemeToggle from "../ThemeToggle";
import SidebarNav from "./SidebarNav";

export default function DocsHeader({ slug }: { slug: string }) {
  const section = sectionOf(slug);
  const drawer = useRef<HTMLDialogElement>(null);
  const menuButton = useRef<HTMLButtonElement>(null);

  const close = () => drawer.current?.close();
  useEffect(() => {
    const d = drawer.current;
    if (!d) return;
    const onClose = () => {
      document.documentElement.classList.remove("drawer-open");
      menuButton.current?.focus();
    };
    d.addEventListener("close", onClose);
    // Wider than the drawer breakpoint: the sidebar is on screen; close it.
    const mq = window.matchMedia("(min-width: 50rem)");
    const onWide = () => mq.matches && d.open && d.close();
    mq.addEventListener("change", onWide);
    return () => {
      d.removeEventListener("close", onClose);
      mq.removeEventListener("change", onWide);
    };
  }, []);
  // A new page (client navigation) closes the drawer.
  useEffect(() => close(), [slug]);

  return (
    <header className="dh">
      <div className="dh-left">
        <button
          ref={menuButton}
          type="button"
          className="icon-button dh-menu"
          aria-label="Open the docs menu"
          aria-haspopup="dialog"
          onClick={() => {
            drawer.current?.showModal();
            document.documentElement.classList.add("drawer-open");
          }}
        >
          <Menu />
        </button>
        <Link className="dh-brand" href="/">
          <LogoMark size={32} />
          <span>criogenai</span>
        </Link>
        <nav className="dh-sections" aria-label="Sections">
          {SECTIONS.map((s) => (
            <Link key={s.id} href={s.href} aria-current={section === s.id ? "page" : undefined}>
              {s.label}
            </Link>
          ))}
        </nav>
      </div>
      <div className="dh-right">
        <Search />
        <span className="dh-wide">
          <ThemeToggle />
        </span>
      </div>

      <dialog
        ref={drawer}
        className="drawer"
        aria-label="Docs menu"
        onClick={(e) => {
          if (e.target === drawer.current) close();
        }}
      >
        <div className="drawer-panel">
          <div className="drawer-head">
            <Link className="dh-brand" href="/">
              <LogoMark size={28} />
              <span>criogenai</span>
            </Link>
            <button type="button" className="icon-button" aria-label="Close the menu" onClick={close}>
              <Close />
            </button>
          </div>
          <nav className="drawer-sections" aria-label="Sections">
            {SECTIONS.map((s) => (
              <Link key={s.id} href={s.href} aria-current={section === s.id ? "page" : undefined} onClick={close}>
                {s.label}
              </Link>
            ))}
            <Link href="/benchmarks/">Benchmarks</Link>
          </nav>
          <div className="d-sidebar-scroll drawer-scroll">
            <SidebarNav current={slug} onNavigate={close} idPrefix="drawer" />
          </div>
          <div className="drawer-foot">
            <ThemeToggle />
          </div>
        </div>
      </dialog>
    </header>
  );
}
