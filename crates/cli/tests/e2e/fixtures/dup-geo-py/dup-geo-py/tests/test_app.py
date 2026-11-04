"""Tests for the app module."""

from app import run

from .conftest import make_dimensions


def test_run_describes_a_rectangle():
    dimensions = make_dimensions()
    assert run(dimensions) == "shape"
