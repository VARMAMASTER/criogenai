"""Called by the native module to complete asyncio futures (design 0011 §4).

Runs on the event loop's thread (``call_soon_threadsafe``). A future that
was cancelled meanwhile is left alone.
"""

import json

from .errors import error_for


def resolve(future, payload):
    if not future.done():
        future.set_result(json.loads(payload))


def reject(future, code, message, details, cause):
    if future.done():
        return
    future.set_exception(make_error(code, message, details, cause))


def make_error(code, message, details=None, cause=None):
    error = error_for(code, message, json.loads(details) if details else None)
    if cause is not None:
        error.__cause__ = cause
    return error
