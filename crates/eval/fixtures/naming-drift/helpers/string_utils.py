"""String helpers: genuinely a utility, unlike the payment files here."""


def slugify(text: str) -> str:
    return "-".join(text.lower().split())


def truncate(text: str, n: int) -> str:
    return text[:n]
