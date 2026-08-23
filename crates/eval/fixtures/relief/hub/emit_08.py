"""emit stage 08: one link of the emit chain."""

from .emit_09 import send_09

def send_08(value: int) -> int:
    return send_09(value + 8)
