"""emit stage 07: one link of the emit chain."""

from .emit_08 import send_08

def send_07(value: int) -> int:
    return send_08(value + 7)
