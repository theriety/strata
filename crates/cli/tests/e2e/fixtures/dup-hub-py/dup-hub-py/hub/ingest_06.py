"""Ingestion stage 06 that refines the shared ingestion seed."""

from .ingest_00 import ingest_seed


def ingest_stage(value: int) -> int:
    return ingest_seed(value) - 6
