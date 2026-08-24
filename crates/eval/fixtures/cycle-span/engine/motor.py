"""Motor control: opens the cross-directory import cycle with the shaft."""

from drivetrain.shaft import spin


def torque_curve(rpm: int) -> int:
    return rpm * 12


def run_motor(rpm_target: int) -> str:
    return "motor:{}:{}".format(rpm_target, spin(rpm_target))
