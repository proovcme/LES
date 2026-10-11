"""Loopback browser boundary; loopback IP alone is not browser authorization."""
from urllib.parse import urlsplit

from starlette.middleware.base import BaseHTTPMiddleware
from starlette.responses import JSONResponse


class LightBrowserBoundary(BaseHTTPMiddleware):
    async def dispatch(self, request, call_next):
        try:
            host = urlsplit('http://' + request.headers.get('host', '')).hostname
            origin = request.headers.get('origin')
            parsed = urlsplit(origin) if origin else None
            valid = host in {'localhost', '127.0.0.1', '::1'}
            if parsed:
                valid = valid and parsed.scheme in {'http', 'https'} and parsed.hostname in {'localhost', '127.0.0.1', '::1'}
                valid = valid and not (parsed.username or parsed.password or parsed.path or parsed.query or parsed.fragment)
                parsed.port
        except ValueError:
            valid = False
        if not valid:
            return JSONResponse({'detail': 'Local LES browser access required'}, status_code=403)
        return await call_next(request)
