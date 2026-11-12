"""Gateway (design 0018): model calls without an agent, simulated models only."""

import asyncio
import time

import pytest

from criogenai import (
    CriogenError,
    Embedder,
    FakeModel,
    Gateway,
    ModelError,
    Retry,
    ValidationError,
)


def test_chat_with_text_or_messages_and_instructions():
    gw = Gateway()
    model = FakeModel(["Paris.", "Second answer."])
    reply = gw.chat_sync(model, "Capital of France?", instructions="Answer in one word.")
    assert reply.text == "Paris."
    assert reply.message["role"] == "assistant"
    assert reply.model == "fake:fake"
    assert reply.stop_reason == "stop"
    # The same model object goes on with its script; content may be a string.
    reply = gw.chat_sync(
        model,
        [
            {"role": "user", "content": "Hi"},
            {"role": "assistant", "content": [{"type": "text", "text": "Hello"}]},
            {"role": "user", "content": "And now?"},
        ],
    )
    assert reply.text == "Second answer."


def test_retries_and_fallbacks_as_for_an_agent():
    fast = Retry(initial_backoff=0.01, jitter=False)
    flaky = FakeModel(["after a retry"], fail_first=1)
    assert Gateway(retry=fast).chat_sync(flaky, "hi").text == "after a retry"

    gw = Gateway(fallbacks=[FakeModel(["from the backup"], name="backup")], retry=fast)
    reply = gw.chat_sync(FakeModel([], fail_first=10, name="primary"), "hi")
    assert reply.text == "from the backup"
    assert reply.model == "fake:backup"

    with pytest.raises(ModelError) as failed:
        Gateway(retry=Retry(max_attempts=2, initial_backoff=0.01)).chat_sync(FakeModel([], fail_first=10), "hi")
    assert failed.value.code == "model_failed"
    assert failed.value.details["model_error"]["kind"] == "rate_limited"


def test_json_output_is_parsed_and_checked():
    weather = {
        "type": "object",
        "properties": {"city": {"type": "string"}, "temp": {"type": "integer"}},
        "required": ["city", "temp"],
    }

    gw = Gateway()
    reply = gw.chat_sync(FakeModel(['{"city": "Oslo", "temp": 3}']), "Weather?", output=weather)
    assert reply.output == {"city": "Oslo", "temp": 3}
    with pytest.raises(CriogenError) as wrong:
        gw.chat_sync(FakeModel(['{"city": "Oslo"}']), "Weather?", output=weather)
    assert wrong.value.code == "output_invalid"
    with pytest.raises(CriogenError):
        gw.chat_sync(FakeModel(["not json"]), "Weather?", output={"type": "object"})


def test_stream_chunks_add_up_to_the_reply():
    async def main():
        stream = Gateway().stream(FakeModel(["Hello there."]), "hi")
        chunks = [chunk async for chunk in stream]
        return chunks, stream.reply

    chunks, reply = asyncio.run(main())
    assert "".join(c.text for c in chunks) == reply.text == "Hello there."
    assert not any(c.restart for c in chunks)
    with pytest.raises(RuntimeError):
        _ = Gateway().stream(FakeModel(["x"]), "hi").reply


def test_cancelling_a_call_stops_it():
    async def main():
        task = asyncio.create_task(Gateway().chat(FakeModel(["late"], delay=30), "hi"))
        await asyncio.sleep(0.05)
        task.cancel()
        started = time.monotonic()
        with pytest.raises(asyncio.CancelledError):
            await task
        return time.monotonic() - started

    assert asyncio.run(main()) < 5


def test_embeddings_and_bad_requests():
    gw = Gateway()
    vectors = gw.embed_sync(Embedder.fake(), ["first text", "second text"])
    assert len(vectors) == 2 and len(vectors[0]) == len(vectors[1]) > 0
    with pytest.raises(ValueError):
        gw.chat_sync("nosuch:model", "hi")
    with pytest.raises(ValidationError):
        gw.chat_sync(FakeModel(["x"]), [{"role": "wizard", "content": "hi"}])
