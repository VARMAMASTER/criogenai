// A slow band of model names under the hero, moving left to right. Every name
// is a model Criogen agents could call on 2026-11-01: through OpenRouter's
// public list, or tested on Kaggle's allowance. Pure CSS: it pauses on hover
// and stands still (wrapped) for visitors who ask for reduced motion.

const MODELS: [maker: string, model: string][] = [
  ["Anthropic", "Claude Sonnet 5"],
  ["Google", "Gemini 3.6 Flash"],
  ["OpenAI", "GPT-5.4 nano"],
  ["NVIDIA", "Nemotron 3.5 Super"],
  ["Meta", "Llama 4 Maverick"],
  ["Microsoft", "Phi 4"],
  ["Mistral", "Mistral Large 4"],
  ["DeepSeek", "DeepSeek V4.1 Flash"],
  ["Qwen", "Qwen3.8 Max"],
  ["xAI", "Grok 4.7"],
  ["Google", "Gemma 4 31B"],
  ["Moonshot", "Kimi K3"],
  ["Cohere", "Command A+"],
  ["Amazon", "Nova Premier"],
  ["OpenAI", "gpt-oss-120b"],
  ["IBM", "Granite 4.0"],
];

function Track({ hidden }: { hidden?: boolean }) {
  return (
    <ul className="marquee-list" aria-hidden={hidden || undefined}>
      {MODELS.map(([maker, model]) => (
        <li className="marquee-pill" key={`${maker}-${model}`}>
          <span className="marquee-maker">{maker}</span>
          <span className="marquee-model">{model}</span>
        </li>
      ))}
    </ul>
  );
}

export default function ModelMarquee() {
  return (
    <section className="marquee" aria-label="Some of the models Criogen agents can call">
      <p className="marquee-caption">
        One agent API for 450+ models: OpenAI, Anthropic, Gemini, OpenRouter, Kaggle, or your own servers
      </p>
      <div className="marquee-window">
        <div className="marquee-track">
          <Track />
          <Track hidden />
        </div>
      </div>
    </section>
  );
}
