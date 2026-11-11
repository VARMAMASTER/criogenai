"""Errors: one exception class per family of failures.

Every exception carries the stable ``code`` the Rust engine uses (for
example ``model_failed`` or ``thread_busy``), the same in every language;
the error reference (/docs/reference/errors/) lists them all. Catch a class for a
family, or compare ``error.code`` for one failure.
"""

from typing import Any, Dict, Optional


class CriogenError(Exception):
    """Any Criogen failure; the base class of the others.

    The engine raises these; you rarely build one yourself. ``str(error)``
    is ``"<code>: <message>"``.

    Args:
        code: The stable snake_case error code, such as ``"model_failed"``.
        message: What went wrong, for people.
        details: More about the failure as a JSON-like dict (it may hold the
            run's ``snapshot``). Default ``{}``.

    Example:
        from criogen import CriogenError, LimitExceededError

        try:
            result = agent.run_sync("Plan the week.")
        except LimitExceededError:
            print("the run hit a limit")
        except CriogenError as error:
            print(error.code, error.message)
    """

    def __init__(self, code: str, message: str, details: Optional[Dict[str, Any]] = None):
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message
        self.details = details or {}

    @property
    def snapshot(self) -> Optional[Dict[str, Any]]:
        """The run's snapshot, if the error carries one."""
        return self.details.get("snapshot")


class ModelError(CriogenError):
    """The model failed after retries (``model_failed``)."""


class ToolExecutionError(CriogenError):
    """A tool crashed (``tool_failed``); ``__cause__`` is the exception."""


class LimitExceededError(CriogenError):
    """A run limit was reached (``limit_exceeded``)."""


class ThreadBusyError(CriogenError):
    """Another run holds the thread (``thread_busy``)."""


class ThreadSuspendedError(CriogenError):
    """The thread waits for approval (``thread_suspended``)."""


class StorageError(CriogenError):
    """Storage failed (``storage_*``, ``encryption_*``, ``audit_failed``)."""


class ValidationError(CriogenError):
    """Something given was not accepted (``invalid_*``, ``resume_invalid``)."""


class CancelledError(CriogenError):
    """The run was cancelled (``cancelled``)."""


# By code, as schemas/error-codes.json's `family` says (design 0013); the
# tests check every code against it.
_BY_CODE = {
    "model_failed": ModelError,
    "tool_failed": ToolExecutionError,
    "limit_exceeded": LimitExceededError,
    "step_limit_exceeded": LimitExceededError,
    "deadline_exceeded": LimitExceededError,
    "workflow_mismatch": ValidationError,
    "thread_busy": ThreadBusyError,
    "thread_suspended": ThreadSuspendedError,
    "cancelled": CancelledError,
    "audit_failed": StorageError,
    "resume_invalid": ValidationError,
}


def error_for(code: str, message: str, details: Optional[Dict[str, Any]] = None) -> CriogenError:
    """The exception for a Rust error code."""
    cls = _BY_CODE.get(code)
    if cls is None:
        if code.startswith(("storage_", "encryption_", "checkpoint_")):
            cls = StorageError
        elif code.startswith("invalid_"):
            cls = ValidationError
        else:
            cls = CriogenError
    return cls(code, message, details)
