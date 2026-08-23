"""emit stage 02: one link of the emit chain."""

from .emit_03 import send_03

def send_02(value: int) -> int:
    return send_03(value + 2)
