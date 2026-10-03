"""Memory of the machine running LES, independent of model endpoints."""
import psutil


def system_memory_snapshot() -> dict:
    vm = psutil.virtual_memory()
    swap = psutil.swap_memory()
    return {
        "ram_total": vm.total / 1e9,
        "ram_used": vm.used / 1e9,
        "ram_free_gb": vm.available / 1e9,
        "swap_used_gb": swap.used / 1e9,
        "swap_total_gb": swap.total / 1e9,
        "swap_pct": swap.percent,
        "memory_source": "local_os",
    }
