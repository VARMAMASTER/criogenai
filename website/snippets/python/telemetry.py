from criogen import Agent


async def main() -> None:
    # [show]
    from criogen import Agent, Telemetry

    # LANGFUSE_PUBLIC_KEY, LANGFUSE_SECRET_KEY, LANGFUSE_BASE_URL
    langfuse = Telemetry.langfuse()
    # Or any OpenTelemetry collector:
    otel = Telemetry.otlp("http://collector:4318/v1/traces")
    # Prompts and answers stay out unless you ask: capture_content=True.

    agent = Agent("openai:gpt-5-mini", telemetry=langfuse)  # every run is a trace
    await agent.run("Where is order A-1001?", user_id="u-42")
    await langfuse.flush()  # before the script ends
    # [/show]


async def local() -> None:
    # [show local]
    # In a terminal: python -m criogen.trace   (then open http://127.0.0.1:4318)
    from criogen import Agent, Telemetry

    traces = Telemetry.local()  # capture_content=True to see prompts and outputs too
    agent = Agent("openai:gpt-5-mini", telemetry=traces)
    await agent.run("Where is order A-1001?")
    await traces.flush()
    # [/show local]
