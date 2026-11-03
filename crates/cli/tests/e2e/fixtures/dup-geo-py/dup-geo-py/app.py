"""Application entry point."""

import importlib

from geometry import Rectangle, describe


def run(dimensions) -> str:
    rectangle = Rectangle(dimensions)
    return describe(rectangle)


def load():
    return importlib.import_module("geometry.shape")
