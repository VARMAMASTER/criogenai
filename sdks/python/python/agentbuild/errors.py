"""Errors (design 0011 §3): one class per family, each with the stable
``code`` the Rust engine uses (design 0003), so checks by code work in
every language."""

from typing import Any, Dict, Optional


class AgentBuildError(Exception):
    """Any AgentBuild failure. ``code`` is stable; ``details`` is JSON."""

    def __init__(self, code: str, message: str, details: Optional[Dict[str, Any]] = None):
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message
        self.details = details or {}

    @property
    def snapshot(self) -> Optional[Dict[str, Any]]:
        """The run's snapshot, if the error carries one."""
        return self.details.get("snapshot")


class ModelError(AgentBuildError):
    """The model failed after retries (``model_failed``)."""


class ToolExecutionError(AgentBuildError):
    """A tool crashed (``tool_failed``); ``__cause__`` is the exception."""


class LimitExceededError(AgentBuildError):
    """A run limit was reached (``limit_exceeded``)."""


class ThreadBusyError(AgentBuildError):
    """Another run holds the thread (``thread_busy``)."""


class ThreadSuspendedError(AgentBuildError):
    """The thread waits for approval (``thread_suspended``)."""


class StorageError(AgentBuildError):
    """Storage failed (``storage_*``, ``encryption_*``, ``audit_failed``)."""


class ValidationError(AgentBuildError):
    """Something given was not accepted (``invalid_*``, ``resume_invalid``)."""


class CancelledError(AgentBuildError):
    """The run was cancelled (``cancelled``)."""


_BY_CODE = {
    "model_failed": ModelError,
    "tool_failed": ToolExecutionError,
    "limit_exceeded": LimitExceededError,
    "thread_busy": ThreadBusyError,
    "thread_suspended": ThreadSuspendedError,
    "cancelled": CancelledError,
    "audit_failed": StorageError,
    "resume_invalid": ValidationError,
}


def error_for(code: str, message: str, details: Optional[Dict[str, Any]] = None) -> AgentBuildError:
    """The exception for a Rust error code."""
    cls = _BY_CODE.get(code)
    if cls is None:
        if code.startswith(("storage_", "encryption_", "checkpoint_")):
            cls = StorageError
        elif code.startswith("invalid_"):
            cls = ValidationError
        else:
            cls = AgentBuildError
    return cls(code, message, details)
