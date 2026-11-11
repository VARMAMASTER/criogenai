# Docs: concepts/errors-and-retries. Checked with mypy against the SDK.
from criogen import Agent


async def handle(agent: Agent) -> None:
    # [show catch]
    from criogen import (
        CancelledError,
        CriogenError,
        LimitExceededError,
        ModelError,
        ThreadBusyError,
        ThreadSuspendedError,
    )

    chat = agent.thread("ticket-7")
    try:
        reply = await chat.send("Where is my order?")
    except ThreadSuspendedError:
        ...  # an approval is waiting: answer it first (approve, reject or decide)
    except ThreadBusyError:
        ...  # another run holds the thread (on_busy="reject", or the wait timed out)
    except LimitExceededError as error:
        print(error.details["limit"])  # "model_calls", "tool_calls", "tokens", ...
    except ModelError as error:
        kind = error.details["model_error"]["kind"]  # "rate_limited", "auth", "context_overflow", ...
        if kind in ("rate_limited", "overloaded", "server", "timeout", "network"):
            reply = await chat.retry()  # transient: continue from the failed step
    except CancelledError:
        ...  # criogen.CancelledError (not asyncio's): the run was cancelled, retry() continues it
    except CriogenError as error:
        # Every other failure: decide by the stable code, never by the message.
        if error.code == "thread_interrupted":
            reply = await chat.retry()  # the process died mid-run: continue it
        else:
            raise
    # [/show catch]


def resilience() -> None:
    # [show retry]
    from criogen import Agent, Retry

    agent = Agent(
        "openai:gpt-5-mini",
        # Transient model errors (rate limits, overload, 5xx, timeouts, network)
        # are retried with backoff; Retry-After from the provider is honoured.
        model_retry=Retry(max_attempts=4, initial_backoff=1.0, max_backoff=30.0),
        max_retry_after=60,  # never wait longer than this for one Retry-After
        # Then the next model is tried, in order.
        fallbacks=["anthropic:claude-haiku-5-5", "ollama:llama3.2"],
        # Also move on for errors that another model may not have.
        fallback_on=["context_overflow", "content_filter"],
    )
    # [/show retry]
