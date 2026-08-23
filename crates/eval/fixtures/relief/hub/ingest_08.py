"""ingest stage 08: one link of the ingest chain."""

from .ingest_09 import stage_09

def stage_08(value: int) -> int:
    return stage_09(value + 8)
