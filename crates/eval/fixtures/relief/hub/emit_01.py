"""emit stage 01: one link of the emit chain."""

from .emit_02 import send_02

def send_01(value: int) -> int:
    return send_02(value + 1)
