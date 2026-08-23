"""emit stage 05: one link of the emit chain."""

from .emit_06 import send_06

def send_05(value: int) -> int:
    return send_06(value + 5)
