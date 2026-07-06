"""A concrete oblong."""

from pkg.blob import Blob, describe_blob


class Oblong(Blob):
    def __init__(self, width: int, height: int) -> None:
        self.width = width
        self.height = height

    def area(self) -> int:
        return self.width * self.height


def summarize(oblong: Oblong) -> str:
    return describe_blob(oblong)
