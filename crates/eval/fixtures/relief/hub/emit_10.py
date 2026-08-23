"""emit stage 10: one link of the emit chain."""

from .emit_11 import send_11

def send_10(value: int) -> int:
    return send_11(value + 10)
