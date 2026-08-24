"""Ignition timing: consumes the motor without joining its cycle."""

from .motor import torque_curve


def spark_advance(rpm: int) -> int:
    return max(0, 30 - rpm // 100) + torque_curve(1) // 8
