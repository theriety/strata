"""Production service that wrongly reaches into test-support code.

Importing `sample_fixture` from `conftest` is a polarity leak: production code
must never depend on test/support code. This edge is the polarity violation the
fixture exists to surface.
"""

from conftest import sample_fixture


def serve() -> int:
    return sample_fixture() + 1
