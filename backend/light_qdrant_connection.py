"""Pass Light's ephemeral key only to the exact Qdrant owned by its launcher."""
import os
from urllib.parse import urlsplit

from backend.product_edition import is_light


def qdrant_client_options(url: str) -> dict:
    if not is_light():
        return {}
    owned = os.getenv("LES_LIGHT_QDRANT_URL", "").rstrip("/")
    key = os.getenv("LES_LIGHT_QDRANT_API_KEY", "")
    parsed = urlsplit(owned)
    if not key or not owned or parsed.scheme != "http" or parsed.hostname != "127.0.0.1" or parsed.port is None:
        raise RuntimeError("Собственное хранилище LES RAG не запущено. Перезапустите приложение.")
    if parsed.username or parsed.password or parsed.path or parsed.query or parsed.fragment or str(url).rstrip("/") != owned:
        raise RuntimeError("Адрес хранилища не принадлежит этому экземпляру LES RAG. Перезапустите приложение.")
    return {"api_key": key}


def qdrant_http_headers(url: str) -> dict:
    options = qdrant_client_options(url)
    return {"api-key": options["api_key"]} if options else {}
