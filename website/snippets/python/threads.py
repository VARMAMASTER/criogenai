from criogenai import Agent


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


async def recover(agent: Agent) -> None:
    # [show recover]
    from criogenai import CriogenError, ThreadSuspendedError

    chat = agent.thread("user-42")
    try:
        reply = await chat.send("Book my follow-up visit")
    except ThreadSuspendedError:
        reply = await chat.reject("Cancelled by the user")  # or approve() / decide(...)
    except CriogenError as error:
        if error.code != "thread_interrupted":  # a crash left the last run unfinished
            raise
        reply = await chat.retry()  # finish that run first; finished tool calls are not run again

    # The stored conversation (the same message format as result.messages).
    for message in await chat.messages():
        print(message["role"], message["content"][0].get("text", ""))
    # [/show recover]
