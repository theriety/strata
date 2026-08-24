"""Application entry point: the only outside consumer of the drive."""

from engine.motor import run_motor


def start(rpm_target: int) -> str:
    return run_motor(rpm_target)
