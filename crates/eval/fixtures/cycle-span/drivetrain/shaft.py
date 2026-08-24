"""Shaft rotation: closes the import cycle back onto the motor."""

from engine.motor import torque_curve


def spin(rpm: int) -> int:
    return torque_curve(rpm) // 4
