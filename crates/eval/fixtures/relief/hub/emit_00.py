"""emit stage 00: one link of the emit chain."""

from .emit_01 import send_01

def send_00(value: int) -> int:
    return send_01(value + 0)
