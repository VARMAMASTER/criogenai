# The tutorial's terminal chat (docs: tutorial/support-agent).
# [show]
import asyncio
import sys

from criogenai import Reply, Storage, Thread, load_env
from support_agent import build_agent

load_env()  # OPENAI_API_KEY from .env, if there is one


async def settle_approvals(desk: Thread, reply: Reply) -> Reply:
    """Asks a staff member about every refund the agent wants to make."""
    while reply.needs_approval:
        for request in reply.approvals:
            print(f"  [staff] {request.tool} {request.args}")
        if input("  [staff] approve? y/n > ").strip().lower() == "y":
            reply = await desk.approve()
        else:
            reply = await desk.reject("A staff member declined the refund.")
    return reply


async def main(customer_id: str, ticket_id: str) -> None:
    storage = Storage("sqlite://support.db")
    agent = build_agent("openai:gpt-5-mini", storage)
    chat = agent.thread(ticket_id, actor=f"customer:{customer_id}")
    desk = agent.thread(ticket_id, actor="staff:sam")  # same thread, another person
    try:
        while True:
            text = input("you > ").strip()
            if text in ("", "quit"):
                break
            reply = await chat.send(text, customer_id=customer_id)
            reply = await settle_approvals(desk, reply)
            print("agent >", reply.text)
    finally:
        await storage.close()


if __name__ == "__main__":
    # python support_chat.py <customer id> <ticket id>
    asyncio.run(main(*(sys.argv[1:3] if len(sys.argv) >= 3 else ["c-7", "ticket-1"])))
# [/show]


# [show stream]
async def send_streaming(chat: Thread, text: str, customer_id: str) -> Reply:
    """send(), printing the answer as it is written."""
    stream = chat.stream(text, customer_id=customer_id)
    print("agent > ", end="", flush=True)
    async for event in stream:
        if event.type == "text":
            print(event.text, end="", flush=True)
        elif event.type == "tool_call_started":
            print(f"[{event.tool}] ", end="", flush=True)
    print()
    return await stream.reply()  # the same saved reply send() returns
# [/show stream]
