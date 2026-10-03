"""Shared, pure memory admission policy for the parser and its UI preview."""
from __future__ import annotations


def parse_memory_block_reason(ram_free_gb: float | None, swap_pct: float | None,
                              min_free_gb: float) -> str | None:
    if ram_free_gb is None or swap_pct is None:
        return "Состояние памяти неизвестно. Обновите состояние перед запуском обработки."
    ram_low = ram_free_gb < min_free_gb
    swap_thrash = swap_pct > 90.0 and ram_free_gb < min_free_gb * 1.5
    if not ram_low and not swap_thrash:
        return None
    requirement = (f"для запуска требуется не менее {min_free_gb:.1f} ГБ. " if ram_low
                   else "система интенсивно использует файл подкачки. ")
    return (f"Обработка пока не запущена: свободно {ram_free_gb:.1f} ГБ памяти, "
            + requirement + "Закройте ненужные приложения или дождитесь завершения других задач и повторите.")
