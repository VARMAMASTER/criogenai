"use client";
// The visitor's own API key, held in React state only: never written to
// localStorage, sessionStorage, cookies or the URL, and never sent to this
// site. It lives as long as the tab and is gone on reload. Moving between
// patterns keeps it (the playground layout stays mounted).
import { createContext, useContext, useState, type ReactNode } from "react";
import type { Connection } from "@/lib/patterns/providers";

interface KeyState {
  conn: Connection | null;
  set(conn: Connection | null): void;
}

const Ctx = createContext<KeyState>({ conn: null, set: () => undefined });

export function KeyProvider({ children }: { children: ReactNode }) {
  const [conn, set] = useState<Connection | null>(null);
  return <Ctx.Provider value={{ conn, set }}>{children}</Ctx.Provider>;
}

export const useKey = () => useContext(Ctx);
