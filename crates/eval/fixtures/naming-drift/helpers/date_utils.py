"""Date helpers: genuinely a utility, unlike the payment files here."""


def parse_iso(text: str) -> tuple:
    year, month, day = text.split("-")
    return (int(year), int(month), int(day))


def format_iso(parts: tuple) -> str:
    return "{:04d}-{:02d}-{:02d}".format(*parts)
