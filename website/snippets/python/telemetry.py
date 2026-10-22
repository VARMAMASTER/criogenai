from agentbuild import Agent


async def main() -> None:
    # [show]
    from agentbuild import Agent, Telemetry

    # LANGFUSE_PUBLIC_KEY, LANGFUSE_SECRET_KEY, LANGFUSE_BASE_URL
    langfuse = Telemetry.langfuse()
    # Or any OpenTelemetry collector:
    otel = Telemetry.otlp("http://collector:4318/v1/traces")
    # Prompts and answers stay out unless you ask: capture_content=True.

    agent = Agent("openai:gpt-5-mini", telemetry=langfuse)  # every run is a trace
    await agent.run("Where is order A-1001?", user_id="u-42")
    await langfuse.flush()  # before the script ends
    # [/show]
