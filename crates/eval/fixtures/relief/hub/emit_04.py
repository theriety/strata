"""emit stage 04: one link of the emit chain."""

from .emit_05 import send_05

def send_04(value: int) -> int:
    return send_05(value + 4)
