// Two slow bands under the hero, moving in opposite directions: the providers
// CriogenAI calls by name (their icons), and models CriogenAI agents could call
// on 2026-11-01 (through OpenRouter's public list, or tested on Kaggle's
// allowance). Pure CSS: they pause on hover and stand still (wrapped) for
// visitors who ask for reduced motion.
import BrandIcon from "./BrandIcon";

/** Named providers (criogenai_providers::PRESETS, plus Kaggle): [icon slug, name]. */
const PROVIDERS: [slug: string | undefined, name: string][] = [
  [undefined, "OpenAI"],
  ["anthropic", "Anthropic"],
  ["googlegemini", "Gemini"],
  ["openrouter", "OpenRouter"],
  ["deepseek", "DeepSeek"],
  ["mistralai", "Mistral"],
  [undefined, "xAI"],
  ["qwen", "Qwen"],
  ["moonshotai", "Moonshot · Kimi"],
  [undefined, "Z.ai · GLM"],
  [undefined, "Groq"],
  [undefined, "Cerebras"],
  [undefined, "Together AI"],
  [undefined, "Fireworks AI"],
  ["kaggle", "Kaggle"],
  ["ollama", "Ollama"],
  ["lmstudio", "LM Studio"],
  ["vllm", "vLLM"],
];

/** [icon slug, maker, model]. */
const MODELS: [slug: string | undefined, maker: string, model: string][] = [
  ["claude", "Anthropic", "Claude Sonnet 5"],
  ["googlegemini", "Google", "Gemini 3.6 Flash"],
  [undefined, "OpenAI", "GPT-5.4 nano"],
  ["nvidia", "NVIDIA", "Nemotron 3.5 Super"],
  ["meta", "Meta", "Llama 4 Maverick"],
  [undefined, "Microsoft", "Phi 4"],
  ["mistralai", "Mistral", "Mistral Large 4"],
  ["deepseek", "DeepSeek", "DeepSeek V4.1 Flash"],
  ["qwen", "Qwen", "Qwen3.8 Max"],
  [undefined, "xAI", "Grok 4.7"],
  ["google", "Google", "Gemma 4 31B"],
  ["kimi", "Moonshot", "Kimi K3"],
  [undefined, "Cohere", "Command A+"],
  [undefined, "Amazon", "Nova Premier"],
  [undefined, "OpenAI", "gpt-oss-120b"],
  [undefined, "IBM", "Granite 4.0"],
];

function ProviderTrack({ hidden }: { hidden?: boolean }) {
  return (
    <ul className="marquee-list" aria-hidden={hidden || undefined}>
      {PROVIDERS.map(([slug, name]) => (
        <li className="marquee-pill marquee-provider" key={name}>
          <BrandIcon slug={slug} name={name} />
          <span className="marquee-maker-name">{name}</span>
        </li>
      ))}
    </ul>
  );
}

function ModelTrack({ hidden }: { hidden?: boolean }) {
  return (
    <ul className="marquee-list" aria-hidden={hidden || undefined}>
      {MODELS.map(([slug, maker, model]) => (
        <li className="marquee-pill" key={`${maker}-${model}`}>
          <BrandIcon slug={slug} name={maker} />
          <span className="marquee-maker">{maker}</span>
          <span className="marquee-model">{model}</span>
        </li>
      ))}
    </ul>
  );
}

export default function ModelMarquee() {
  return (
    <section className="marquee" aria-label="Providers and some of the models CriogenAI agents can call">
      <p className="marquee-caption">One agent API for 450+ models, from every major provider or your own servers</p>
      <div className="marquee-window">
        <div className="marquee-track marquee-rtl">
          <ProviderTrack />
          <ProviderTrack hidden />
        </div>
      </div>
      <div className="marquee-window">
        <div className="marquee-track">
          <ModelTrack />
          <ModelTrack hidden />
        </div>
      </div>
    </section>
  );
}
