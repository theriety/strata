"""Telemetry sink: shares nothing with billing."""

_events: list[str] = []


def emit(event: str) -> int:
    _events.append(event)
    return len(_events)


def flush() -> int:
    count = len(_events)
    _events.clear()
    return count
