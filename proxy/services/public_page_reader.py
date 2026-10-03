"""Bounded public HTTP reader with DNS pinning; no browser or extra service."""
from __future__ import annotations

import http.client
import ipaddress
import socket
import ssl
from html.parser import HTMLParser
from urllib.parse import urljoin, urlsplit


class PageText(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.hidden = 0
        self.in_title = False
        self.parts = []
        self.title = []

    def handle_starttag(self, tag, attrs):
        if tag in {"script", "style", "noscript"}:
            self.hidden += 1
        if tag == "title":
            self.in_title = True

    def handle_endtag(self, tag):
        if tag in {"script", "style", "noscript"}:
            self.hidden = max(0, self.hidden - 1)
        if tag == "title":
            self.in_title = False

    def handle_data(self, data):
        if not self.hidden and data.strip():
            self.parts.append(data.strip())
            if self.in_title:
                self.title.append(data.strip())


def _connection(host, port, ip, secure):
    connection = http.client.HTTPConnection(host, port, timeout=12)

    def connect():
        sock = socket.create_connection((ip, port), timeout=12)
        try:
            connection.sock = ssl.create_default_context().wrap_socket(sock, server_hostname=host) if secure else sock
        except BaseException:
            sock.close()
            raise

    connection.connect = connect
    return connection


def read_public_page(url, *, resolver=socket.getaddrinfo, connection_factory=_connection):
    """Validate every redirect and connect to the validated address, avoiding DNS rebinding."""
    current = str(url)
    for _ in range(6):
        parsed = urlsplit(current)
        if parsed.scheme not in {"http", "https"} or not parsed.hostname or parsed.username is not None or parsed.password is not None:
            raise ValueError("Адрес не является публичной HTTP-страницей")
        port = parsed.port or (443 if parsed.scheme == "https" else 80)
        addresses = resolver(parsed.hostname, port, type=socket.SOCK_STREAM)
        ips = [str(item[4][0]) for item in addresses]
        if not ips or any(not ipaddress.ip_address(ip).is_global for ip in ips):
            raise ValueError("Доступ к локальным и служебным адресам запрещён")
        connection = connection_factory(parsed.hostname, port, ips[0], parsed.scheme == "https")
        try:
            path = parsed.path or "/"
            if parsed.query:
                path += "?" + parsed.query
            connection.request("GET", path, headers={"User-Agent": "LES-Light/0.1", "Accept": "text/html,text/plain", "Accept-Encoding": "identity"})
            response = connection.getresponse()
            if response.status in {301, 302, 303, 307, 308}:
                location = response.getheader("Location")
                if not location:
                    raise ValueError("Страница перенаправляет без адреса назначения")
                current = urljoin(current, location)
                continue
            if response.status != 200:
                raise ValueError(f"Сайт вернул HTTP {response.status}")
            content_type = response.getheader("Content-Type", "").lower()
            if not content_type.startswith(("text/html", "text/plain", "application/xhtml+xml")):
                raise ValueError("По адресу находится не текстовая веб-страница")
            raw = response.read(1_000_001)
            if len(raw) > 1_000_000:
                raise ValueError("Страница превышает допустимый размер 1 МБ")
            charset = response.headers.get_content_charset() or "utf-8"
            text = raw.decode(charset, errors="replace")
            title = ""
            if "html" in content_type:
                parser = PageText()
                parser.feed(text)
                text = "\n".join(parser.parts)
                title = " ".join(parser.title)
            return {"final_url": current, "title": title, "text": text}
        finally:
            connection.close()
    raise ValueError("Слишком много перенаправлений страницы")
