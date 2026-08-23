"""emit stage 03: one link of the emit chain."""

from .emit_04 import send_04

def send_03(value: int) -> int:
    return send_04(value + 3)
