// "Works with your models": the providers Criogen has presets for, with their
// logos (logos.ts), what each count means, and the models a standard Kaggle
// allowance can call. Numbers carry the date they were checked.
import Link from "next/link";
import Reveal from "@/components/motion/Reveal";
import { LOGOS } from "./logos";

/** OpenRouter's public model list, counted on this date. */
const OPENROUTER_MODELS = { count: 452, checked: "2026-11-01" };

/** Models a standard Kaggle account's allowance could call (each one called, 2026-11-01). */
const KAGGLE_MODELS = [
  "anthropic/claude-sonnet-5@default",
  "google/gemini-3.6-flash",
  "google/gemini-3-flash-preview",
  "google/gemini-3.5-flash-lite",
  "google/gemini-3.1-flash-lite-preview",
  "openai/gpt-5.4-nano-2026-03-17",
  "openai/gpt-oss-120b",
  "ibm/granite-4.0-h-small",
];

const ORDER = ["anthropic", "googlegemini", "openrouter", "kaggle", "ollama", "lmstudio", "vllm"];

export default function Providers() {
  const logos = ORDER.map((slug) => LOGOS.find((l) => l.slug === slug)).filter((l) => l !== undefined);
  return (
    <section className="providers" aria-labelledby="providers-title">
      <h2 id="providers-title">Works with your models</h2>
      <p className="section-lede">
        Hosted APIs, model routers, Kaggle&apos;s free allowance and models on your own hardware, behind one agent API.
        Switching is one string, such as <code>&quot;anthropic:claude-sonnet-5-5&quot;</code> or{" "}
        <code>&quot;ollama:llama3.2&quot;</code>.
      </p>

      <Reveal>
        <ul className="logo-grid" aria-label="Providers with a built-in preset">
          <li className="logo-tile">
            <span className="logo-word" aria-hidden="true">
              OpenAI
            </span>
            <span className="logo-name">OpenAI</span>
          </li>
          {logos.map((logo) => (
            <li className="logo-tile" key={logo.slug}>
              <svg viewBox="0 0 24 24" role="img" aria-hidden="true" className="logo-svg">
                <path d={logo.path} />
              </svg>
              <span className="logo-name">{logo.title}</span>
            </li>
          ))}
          <li className="logo-tile logo-any">
            <span className="logo-plus" aria-hidden="true">
              +
            </span>
            <span className="logo-name">Any OpenAI- or Anthropic-compatible server</span>
          </li>
        </ul>
      </Reveal>

      <div className="provider-stats">
        <Reveal className="stat glass" delay={0.05}>
          <b>8</b>
          <span>providers with a built-in preset, plus any compatible server you point it at</span>
        </Reveal>
        <Reveal className="stat glass" delay={0.1}>
          <b>{OPENROUTER_MODELS.count}</b>
          <span>
            models reachable through OpenRouter alone (their list, counted {OPENROUTER_MODELS.checked}); tool calls depend
            on the model
          </span>
        </Reveal>
        <Reveal className="stat glass" delay={0.15}>
          <b>{KAGGLE_MODELS.length}</b>
          <span>models a standard Kaggle account&apos;s free AI allowance could call (each one tested, 2026-11-01)</span>
        </Reveal>
      </div>

      <Reveal className="kaggle-models glass" delay={0.1}>
        <h3>Free for prototypes: Kaggle&apos;s model allowance</h3>
        <p>
          Use <code>&quot;kaggle:&lt;model&gt;&quot;</code>; Criogen gets and renews the hourly token from your Kaggle
          token. For prototypes and tests, not production traffic.{" "}
          <Link href="/docs/guides/models/">How it works</Link>
        </p>
        <ul>
          {KAGGLE_MODELS.map((m) => (
            <li key={m}>
              <code>{m}</code>
            </li>
          ))}
        </ul>
      </Reveal>

      <p className="fine">
        Logos are trademarks of their owners and are shown only to say which services Criogen works with (icons from
        Simple Icons, CC0). Criogen is not affiliated with or endorsed by them.
      </p>
    </section>
  );
}
