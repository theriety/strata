"""Column layout over wrapped lines."""

from .wrap import wrap_line


def columns(text: str, width: int, count: int) -> list:
    lines = wrap_line(text, width)
    return [lines[i::count] for i in range(count)]
