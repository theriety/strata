"""Entry point bridging the two relief chains."""

from hub.emit_00 import send_00
from hub.ingest_00 import stage_00


def run(seed: int) -> int:
    return stage_00(seed) + send_00(seed)
