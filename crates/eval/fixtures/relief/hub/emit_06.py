"""emit stage 06: one link of the emit chain."""

from .emit_07 import send_07

def send_06(value: int) -> int:
    return send_07(value + 6)
