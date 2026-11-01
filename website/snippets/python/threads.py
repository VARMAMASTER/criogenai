from criogen import Agent


async def main(agent: Agent) -> None:
    # [show]
    chat = agent.thread("user-42")
    await chat.send("Hi, I am Ada")
    reply = await chat.send("What is my name?")  # remembers the conversation

    history = await chat.history(20)  # checkpoints, newest first
    await chat.rewind(history[1]["id"])  # go back one step

    # After a crash mid-run: continue where it stopped.
    # Finished tool calls are not run again.
    await chat.retry()

    await chat.delete()
    # [/show]


def sync_scripts(agent: Agent) -> None:
    # [show sync]
    # Scripts and notebooks: the same calls without asyncio.
    reply = agent.thread("user-42").send_sync("Hello")
    result = agent.run_sync("Where is order A-1001?")
    # [/show sync]
