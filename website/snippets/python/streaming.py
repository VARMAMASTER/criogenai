from agentbuild import Agent


async def main(agent: Agent) -> None:
    # [show]
    stream = agent.thread("user-42").stream("Summarise my last order")
    async for event in stream:
        if event.type == "text":
            print(event.text, end="")
        elif event.type == "tool_call_started":
            print(f"\n[{event.tool}…]")
        elif event.type == "approval_needed":
            print("\nwaiting for approval")
    reply = await stream.reply()  # saved, like send()
    # Leaving the loop early cancels the run; it is still saved, and retry() continues it.
    # [/show]
