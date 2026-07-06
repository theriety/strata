"""Blob primitives."""


class Blob:
    """Base blob contract."""

    def area(self) -> int:
        raise NotImplementedError


def describe_blob(blob: Blob) -> str:
    return f"area={blob.area()}"
