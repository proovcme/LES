"""Product boundary of the standalone LES Light repository."""
import os


def is_light() -> bool:
    value = os.getenv("LES_PRODUCT_EDITION", "light").strip().lower()
    if value != "light":
        raise ValueError("This repository contains LES Light only")
    return True


def profile_modes() -> tuple[str, ...]:
    is_light()
    return ("search", "agent")
