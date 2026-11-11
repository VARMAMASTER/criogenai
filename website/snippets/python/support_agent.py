# The tutorial's agent (docs: tutorial/support-agent). Checked with mypy;
# the tutorial's test (support_test.py) runs it with a FakeModel.
# [show tools]
from typing import Any, Dict

from criogen import ToolError, tool

# A stand-in for your order database.
ORDERS: Dict[str, Dict[str, Any]] = {
    "A-1001": {"status": "delivered", "total": 48.0},
    "A-1002": {"status": "in transit", "total": 120.0},
}


@tool(read_only=True)
def get_order(order_id: str) -> dict:
    """Look up an order by its id (like A-1001): its status and total."""
    order = ORDERS.get(order_id)
    if order is None:
        # The model sees this message and can ask the customer again.
        raise ToolError(f"No order {order_id}. Ask the customer to check the id.")
    return {"order_id": order_id, **order}


@tool(destructive=True)
def issue_refund(order_id: str, amount: float, reason: str) -> dict:
    """Refund part or all of an order."""
    order = ORDERS.get(order_id)
    if order is None:
        raise ToolError(f"No order {order_id}")
    if not 0 < amount <= order["total"]:
        raise ToolError(f"A refund must be more than 0 and at most {order['total']}")
    return {"order_id": order_id, "refunded": amount, "reason": reason}
# [/show tools]


# [show agent]
from criogen import Agent, Memory, Storage

INSTRUCTIONS = """You are the support assistant of an online shop.
Look an order up before you answer a question about it.
To refund, call issue_refund: a staff member approves it before it runs.
Save lasting facts about the customer (preferences, past problems) with remember."""


def build_agent(model: Any, storage: Storage) -> Agent:
    return Agent(
        model,
        instructions=INSTRUCTIONS,
        tools=[get_order, issue_refund],
        approve=["issue_refund"],  # pause before every refund
        storage=storage,  # conversations survive restarts
        memory=Memory.by_state("customer_id", "customers"),  # facts per customer
        limits={"model_calls": 12, "tool_calls": 20},
    )
# [/show agent]
