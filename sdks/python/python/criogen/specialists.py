"""Specialist agents: default recipes for common jobs that you configure, extend or replace.

    from criogen import specialists

    agent = specialists.researcher.build("openai:gpt-5-mini", tools=[search])
    careful = specialists.researcher.extend("careful-researcher", add="Always give two sources.")
    specialists.register(careful)
    specialists.get("careful-researcher")

A :class:`Specialist` is a *recipe*, not an agent: a name, a description (what
a coordinator reads to decide when to use it), instructions, and default
agent settings. ``build`` makes the agent. The built-in ones are prompts and
sensible defaults; tools come from you, and no claim is made about their
quality on your data.

Built-in recipes, registered under their names and available as module
attributes: ``researcher`` (web search and page reading when
``TAVILY_API_KEY`` or ``BRAVE_API_KEY`` is set and you give it no tools),
``summarizer``, ``classifier``, ``extractor``, ``planner``, ``writer``,
``critic``, ``fact_checker``, ``code_reviewer``, ``translator`` and
``support_agent``.
"""

from typing import Any, Dict, List, Mapping, Optional, Sequence, Union

from .agent import Agent

__all__ = [
    "Specialist", "register", "get", "available", "unregister",
    "researcher", "summarizer", "classifier", "extractor", "planner", "writer",
    "critic", "fact_checker", "code_reviewer", "translator", "support_agent",
]

# Agent settings a recipe may carry (anything else is rejected at build time by Agent itself).
_NAME_LIMIT = 64


class Specialist:
    """A recipe for an agent: a name, a description, instructions and default ``Agent`` settings.

    Use it to describe an agent once and build it with different models, or
    to give :func:`~criogen.patterns.coordinator` and the other patterns a
    step by name (after :func:`register`).

    Args:
        name: Letters, digits, ``-`` and ``_``, at most 64 characters.
        description: What the agent is for, in one sentence; coordinators
            choose specialists by it. Must not be empty.
        instructions: The agent's instructions (system prompt).
        **settings: Default ``Agent`` keyword arguments (``tools``,
            ``output``, ``limits``, ``approve``, ``hooks``, ``storage``,
            ``memory``, ``fallbacks``, ...). Anything ``Agent`` does not accept
            fails when :meth:`build` runs.

    Raises:
        ValueError: An invalid name or an empty description.

    Example:
        from criogen import FakeModel, specialists

        triage = specialists.Specialist(
            "triage",
            "Sorts incoming tickets by team.",
            "Sort the ticket into billing, shipping or other. Reply with one word.",
        )
        agent = triage.build(FakeModel(["billing"]))
        agent.run_sync("I was charged twice").text   # "billing"
    """

    def __init__(self, name: str, description: str, instructions: str, **settings: Any):
        if not name or len(name) > _NAME_LIMIT or not name.replace("-", "_").replace("_", "").isalnum():
            raise ValueError(f"specialist name {name!r}: use letters, digits, - and _ (at most {_NAME_LIMIT})")
        if not description.strip():
            raise ValueError("a specialist needs a description: coordinators choose by it")
        self.name = name
        self.description = description
        self.instructions = instructions
        self.settings: Dict[str, Any] = dict(settings)

    def build(self, model: Any, **overrides: Any) -> Agent:
        """Builds the agent from this recipe.

        Args:
            model: The agent's model (a model string such as
                ``"openai:gpt-5-mini"`` or a model object).
            **overrides: ``Agent`` keyword arguments that win over the recipe's
                settings. ``instructions=`` replaces the instructions and
                ``tools=`` replaces the tools (use :meth:`extend` to add tools).

        Returns:
            A new ``Agent``.

        Example:
            from criogen import specialists

            agent = specialists.summarizer.build("openai:gpt-5-mini", limits={"model_calls": 2})
            result = await agent.run(long_text)
        """
        settings = {**self.settings, **overrides}
        instructions = settings.pop("instructions", self.instructions)
        factory = settings.pop("default_tools", None)
        tools = list(settings.get("tools", ()))
        if not tools and factory is not None:
            tools = list(factory())  # e.g. web search when a key is set: plug and play
        settings["tools"] = tools
        return Agent(model, instructions=instructions, **settings)

    def extend(
        self,
        name: Optional[str] = None,
        *,
        add: Optional[str] = None,
        instructions: Optional[str] = None,
        description: Optional[str] = None,
        tools: Sequence[Any] = (),
        **settings: Any,
    ) -> "Specialist":
        """A new recipe based on this one; this recipe is unchanged.

        Args:
            name: The new recipe's name. Default: ``"<this name>-extended"``.
            add: Text appended to the instructions.
            instructions: Replaces the instructions (``add`` is then appended to
                these).
            description: Replaces the description.
            tools: Tools added to the recipe's tools.
            **settings: Other ``Agent`` settings, overriding the recipe's.

        Returns:
            The new :class:`Specialist` (not registered: call :func:`register`).

        Example:
            from criogen import specialists

            careful = specialists.researcher.extend("careful-researcher", add="Always give two sources.")
            specialists.register(careful)
        """
        base = instructions if instructions is not None else self.instructions
        if add:
            base = f"{base.rstrip()}\n\n{add.strip()}"
        merged = {**self.settings, **settings}
        merged["tools"] = [*self.settings.get("tools", ()), *tools]
        return Specialist(name or f"{self.name}-extended", description or self.description, base, **merged)

    def __repr__(self) -> str:
        return f"Specialist({self.name!r})"


_REGISTRY: Dict[str, Specialist] = {}


def register(specialist: Specialist, *, replace: bool = False) -> Specialist:
    """Keeps a recipe under its name, so patterns and configuration can refer to it by name.

    Args:
        specialist: The recipe.
        replace: ``True`` replaces a recipe registered under the same name.
            Default ``False``.

    Returns:
        The recipe, unchanged.

    Raises:
        ValueError: The name is taken and ``replace`` is ``False``.

    Example:
        from criogen import patterns, specialists

        specialists.register(specialists.writer.extend("short-writer", add="At most 50 words."))
        flow = patterns.quick("researcher > short-writer", model="openai:gpt-5-mini")
    """
    if specialist.name in _REGISTRY and not replace:
        raise ValueError(f"a specialist named {specialist.name!r} is already registered (replace=True to overwrite)")
    _REGISTRY[specialist.name] = specialist
    return specialist


def unregister(name: str) -> None:
    """Forgets a registered recipe; nothing happens if the name is not registered.

    The built-in recipes stay available as module attributes (e.g.
    ``specialists.writer``), so they can be registered again.

    Args:
        name: The registered name.
    """
    _REGISTRY.pop(name, None)


def get(name: str) -> Specialist:
    """The recipe registered under ``name``.

    Args:
        name: The registered name, e.g. ``"researcher"``.

    Returns:
        The :class:`Specialist`.

    Raises:
        KeyError: No recipe has this name (the message lists the registered
            names).
    """
    try:
        return _REGISTRY[name]
    except KeyError:
        raise KeyError(f"no specialist {name!r}; registered: {sorted(_REGISTRY)}") from None


def available() -> List[str]:
    """The registered names, sorted."""
    return sorted(_REGISTRY)


def _web_tools() -> List[Any]:
    from . import web  # imported when needed: the specialists work without it

    return web.default_tools()


def _builtin(name: str, description: str, instructions: str, **settings: Any) -> Specialist:
    return register(Specialist(name, description, instructions, **settings))


researcher = _builtin(
    "researcher",
    "Finds and gathers facts on a topic, with sources, using its tools. Use it before writing.",
    "You are a careful researcher. Use your tools to find facts; never invent facts or sources. "
    "Reply with short bullet points, each with where it came from. If you cannot find something, say so.",
    default_tools=_web_tools,  # web search + page reading when TAVILY_API_KEY or BRAVE_API_KEY is set
)
summarizer = _builtin(
    "summarizer",
    "Condenses long text into a short, faithful summary.",
    "You summarize. Keep every important fact and number, drop the rest, add nothing that is not in the text. "
    "Reply with the summary only.",
)
classifier = _builtin(
    "classifier",
    "Puts a message into one category and rates its urgency. Use it to triage.",
    "You classify the user's message. Choose exactly one category from the list you were given (if none was given, "
    "use: billing, shipping, technical, other) and rate urgency as low, medium or high. Reply as JSON: "
    '{"category": "...", "urgency": "..."}.',
)
extractor = _builtin(
    "extractor",
    "Pulls structured fields out of free text. Give it an output schema.",
    "You extract fields from the user's text. Fill only what the text states; leave a field null when it is not there. "
    "Never guess.",
)
planner = _builtin(
    "planner",
    "Breaks a goal into a short ordered list of concrete steps.",
    "You plan. Break the goal into at most 7 concrete, ordered steps, each one a single action someone could do. "
    "Reply with a numbered list only.",
)
writer = _builtin(
    "writer",
    "Writes clear prose from notes or a brief. Use it after research.",
    "You are a writer. Turn the notes you are given into clear, well-organised prose for the stated audience. "
    "Use only facts from the notes; if the notes are thin, say what is missing instead of filling it in.",
)
critic = _builtin(
    "critic",
    "Reviews a draft against criteria and either approves it or says what to fix.",
    "You are a strict reviewer. Check the draft against the criteria you were given (accuracy, completeness, clarity, "
    "and anything the user specified). If it is good enough to use, reply with exactly: APPROVED. Otherwise reply with: "
    "REVISE: followed by a short, specific list of what to change.",
)
fact_checker = _builtin(
    "fact_checker",
    "Checks claims in a text against its tools and reports which hold.",
    "You check facts. List each factual claim in the text, verify it with your tools, and label it SUPPORTED, "
    "CONTRADICTED or UNVERIFIED with one line of evidence. Never mark a claim SUPPORTED without evidence.",
)
code_reviewer = _builtin(
    "code_reviewer",
    "Reviews code for bugs, security problems and unclear parts.",
    "You review code. Report real bugs first, then security problems, then readability, each with the line or "
    "function and a concrete fix. If the code is fine, reply with exactly: APPROVED.",
)
translator = _builtin(
    "translator",
    "Translates text faithfully into the language it is asked for.",
    "You translate. Keep the meaning, tone and formatting; do not add or remove information. Reply with the "
    "translation only.",
)
support_agent = _builtin(
    "support_agent",
    "Answers customer questions politely and briefly, using its tools for facts.",
    "You are a friendly customer support agent. Be brief. Use your tools for facts about orders and accounts; never "
    "guess. If an action needs a person's approval, say it is waiting for approval.",
)
