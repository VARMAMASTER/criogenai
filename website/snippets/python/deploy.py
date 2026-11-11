# Docs: guides/deploying (a FastAPI app). Checked with mypy against the SDK.
from fastapi import Request


async def customer_of(request: Request) -> str:
    """Your authentication: who is calling."""
    return "c-7"


async def staff_of(request: Request) -> str:
    """Your authentication: a signed-in staff member."""
    return "sam"


# [show]
import json
import os
from contextlib import asynccontextmanager
from typing import AsyncIterator

from fastapi import Depends, FastAPI
from fastapi.responses import JSONResponse, StreamingResponse
from pydantic import BaseModel

from criogen import CriogenError, Storage
from support_agent import build_agent

# One Storage and one Agent per process, shared by every request.
storage = Storage(os.environ.get("DATABASE_URL", "sqlite://support.db"))
agent = build_agent(os.environ.get("MODEL", "openai:gpt-5-mini"), storage)


@asynccontextmanager
async def lifespan(app: FastAPI) -> AsyncIterator[None]:
    yield
    await storage.close()  # release the database at shutdown


app = FastAPI(lifespan=lifespan)


class Message(BaseModel):
    text: str


@app.post("/tickets/{ticket_id}/messages")
async def send(ticket_id: str, body: Message, customer_id: str = Depends(customer_of)) -> JSONResponse:
    chat = agent.thread(ticket_id, actor=f"customer:{customer_id}")
    try:
        reply = await chat.send(body.text, customer_id=customer_id)
    except CriogenError as error:
        status = 409 if error.code in ("thread_busy", "thread_suspended") else 502
        return JSONResponse({"error": error.code}, status_code=status)  # the code, never the message
    return JSONResponse({"text": reply.text, "needs_approval": reply.needs_approval})


@app.post("/tickets/{ticket_id}/stream")
async def stream(ticket_id: str, body: Message, customer_id: str = Depends(customer_of)) -> StreamingResponse:
    chat = agent.thread(ticket_id, actor=f"customer:{customer_id}")

    async def events() -> AsyncIterator[str]:
        # If the client disconnects, this generator is closed, which cancels the
        # run: it is saved, and retry() continues it.
        run = chat.stream(body.text, customer_id=customer_id)
        try:
            async for event in run:
                if event.type == "text":
                    yield f"event: text\ndata: {json.dumps({'text': event.text})}\n\n"
                elif event.type == "tool_call_started":
                    yield f"event: tool\ndata: {json.dumps({'tool': event.tool})}\n\n"
            reply = await run.reply()
            yield f"event: done\ndata: {json.dumps({'needs_approval': reply.needs_approval})}\n\n"
        except CriogenError as error:
            yield f"event: error\ndata: {json.dumps({'code': error.code})}\n\n"

    return StreamingResponse(events(), media_type="text/event-stream")


@app.post("/tickets/{ticket_id}/approve")
async def approve(ticket_id: str, staff_id: str = Depends(staff_of)) -> JSONResponse:
    reply = await agent.thread(ticket_id, actor=f"staff:{staff_id}").approve()
    return JSONResponse({"text": reply.text})
# [/show]
