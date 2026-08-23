"""Line wrapping primitives."""


def wrap_line(text: str, width: int) -> list:
    return [text[i : i + width] for i in range(0, len(text), width)]
