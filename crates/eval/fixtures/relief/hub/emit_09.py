"""emit stage 09: one link of the emit chain."""

from .emit_10 import send_10

def send_09(value: int) -> int:
    return send_10(value + 9)
