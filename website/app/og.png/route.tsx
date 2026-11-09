import { ImageResponse } from "next/og";
import { MARK } from "@/lib/mark";
import { facts, ms } from "@/lib/seo";

export const dynamic = "force-static";
const size = { width: 1200, height: 630 };

/** /og.png: the link card every page shares (LinkedIn, X, Slack, search results); see OG_IMAGE in lib/seo.ts. */
export function GET() {
  const f = facts();
  const bars = [
    { name: "Criogen", value: f.turn.criogen, color: "#3ecf8e" },
    { name: "LangGraph", value: f.turn.langgraph, color: "#6b7280" },
    { name: "CrewAI", value: f.turn.crewai, color: "#6b7280" },
  ];
  // Log scale: CrewAI is ~100× Criogen, a linear bar would hide Criogen.
  const max = Math.log10(Math.max(...bars.map((b) => b.value)) * 10);
  const width = (v: number) => Math.max(4, (Math.log10(v * 10) / max) * 560);
  return new ImageResponse(
    (
      <div style={{ width: "100%", height: "100%", display: "flex", flexDirection: "column", background: "#0b0d10", color: "#e8eaed", padding: "64px 72px", fontFamily: "sans-serif" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 20 }}>
          <svg width="72" height="72" viewBox="0 0 32 32" fill="none">
            <path d={MARK.flame} fill={MARK.amber} fillOpacity={0.4} stroke={MARK.amber} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
            <path d={MARK.flameCore} fill="#fbd38a" />
            <path d={MARK.body} fill={MARK.green} fillOpacity={0.2} stroke={MARK.green} strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" />
            <path d={MARK.fins} stroke={MARK.green} strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" />
            <circle cx={MARK.core.cx} cy={MARK.core.cy} r={MARK.core.r} fill={MARK.amber} />
          </svg>
          <div style={{ fontSize: 52, fontWeight: 700 }}>Criogen</div>
        </div>
        <div style={{ fontSize: 58, fontWeight: 700, marginTop: 36, lineHeight: 1.1 }}>The fast AI agent framework</div>
        <div style={{ fontSize: 30, color: "#9aa0a6", marginTop: 14 }}>A LangChain alternative on a Rust engine, for Python and TypeScript</div>
        <div style={{ display: "flex", flexDirection: "column", gap: 14, marginTop: 44 }}>
          <div style={{ fontSize: 22, color: "#9aa0a6" }}>{`Framework overhead per agent turn, log scale (benchmark of ${f.date})`}</div>
          {bars.map((b) => (
            <div key={b.name} style={{ display: "flex", alignItems: "center", gap: 18 }}>
              <div style={{ width: 150, fontSize: 26 }}>{b.name}</div>
              <div style={{ width: width(b.value), height: 26, background: b.color, borderRadius: 6 }} />
              <div style={{ fontSize: 26, color: b.name === "Criogen" ? "#3ecf8e" : "#e8eaed" }}>{ms(b.value)}</div>
            </div>
          ))}
        </div>
        <div style={{ marginTop: "auto", fontSize: 24, color: "#9aa0a6" }}>criogen.vercel.app · npm install criogen</div>
      </div>
    ),
    size,
  );
}
