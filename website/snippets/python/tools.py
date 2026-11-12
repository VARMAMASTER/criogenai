# Docs: guides/tools. Checked with mypy against the SDK.
from typing import List, Literal, Optional

from pydantic import BaseModel

# [show hints]
from criogenai import tool


class Address(BaseModel):
    street: str
    city: str
    postcode: str


@tool
def schedule_visit(
    patient_ref: str,                              # "type": "string", required
    kind: Literal["home", "clinic", "video"],      # "enum": [...]
    slots: List[str],                              # "type": "array" of strings
    duration_minutes: int = 30,                    # "type": "integer", optional
    note: Optional[str] = None,                    # string or null, optional
    address: Optional[Address] = None,             # a Pydantic model: its JSON Schema
) -> dict:
    """Book a visit in the first free slot. Slots are ISO 8601 times."""
    return {"booked": slots[0], "kind": kind, "minutes": duration_minutes}


print(schedule_visit.schema)  # the JSON Schema the model sees
# [/show hints]


# [show errors]
from criogenai import ToolError


@tool(read_only=True)
def get_lab_result(test_id: str) -> dict:
    """Read one lab result by its id."""
    if not test_id.startswith("LAB-"):
        # Goes back to the model as the tool's result; the run goes on.
        raise ToolError("Lab ids look like LAB-12345")
    if lab_system_is_down():
        # retryable=True: calling again may work. A read_only or idempotent
        # tool is then retried by the engine before the model sees the error.
        raise ToolError("The lab system is not answering", retryable=True)
    return {"test_id": test_id, "status": "final"}
# [/show errors]


def lab_system_is_down() -> bool:
    return False


# [show context]
from criogenai import ToolContext


@tool(name="send_message", description="Send a message to the care team.", destructive=True)
async def send(text: str, ctx: ToolContext) -> str:
    # ctx is filled by CriogenAI and is not part of the schema the model sees.
    # call_id is stable across retries of the same call: use it as an idempotency key.
    await outbox_put(key=ctx.call_id, text=text)
    return "sent"
# [/show context]


async def outbox_put(key: str, text: str) -> None:
    pass


# [show retry]
from criogenai import Agent, Retry

agent = Agent(
    "openai:gpt-5-mini",
    tools=[get_lab_result, send],
    # Read-only and idempotent tools are retried after ToolError(retryable=True)
    # or a timeout; other tools never are. This sets how often and how patiently.
    tool_retry=Retry(max_attempts=4, initial_backoff=1.0),
    limits={"tool_timeout_secs": 20},  # each call; the default is 60
    on_tool_exception="return_generic",  # unexpected exceptions: hide their text from the model
)
# [/show retry]
