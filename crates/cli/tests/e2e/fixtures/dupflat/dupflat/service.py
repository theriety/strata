"""Production code that wrongly reaches into test-support code."""

from dupflat.conftest import sample_fixture


def serve() -> int:
    return sample_fixture() + 1
