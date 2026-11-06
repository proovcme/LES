"""Compiled, same-origin desktop UI. No Node runtime or model access in the renderer."""
from __future__ import annotations

import os
from pathlib import Path
from urllib.parse import urlsplit

from fastapi import Depends
from starlette.staticfiles import StaticFiles

from proxy.config import CORS_ALLOWED_ORIGIN_REGEX, CORS_ALLOWED_ORIGINS, DEFAULT_CORS_ALLOWED_ORIGIN_REGEX, TRUSTED_PROXY_HEADER
from proxy.security import require_user, require_admin, _trusted_network_role

FRONTEND_ROOT = Path(__file__).resolve().parents[1] / 'desktop/light/frontend/dist'
CSP = ("default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; "
       "connect-src 'self' ipc: http://ipc.localhost; frame-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'self'")


class FrontendFiles(StaticFiles):
    async def get_response(self, path, scope):
        response = await super().get_response(path, scope)
        response.headers.update({
            'Content-Security-Policy': CSP, 'X-Content-Type-Options': 'nosniff',
            'Referrer-Policy': 'no-referrer', 'Cache-Control': 'no-store',
        })
        return response


def mount_frontend(app):
    @app.get('/api/light/native-auth-policy', include_in_schema=False)
    async def native_auth_policy():
        return {
            'schema': 'les.native-auth-policy.v1',
            'trusted_role': _trusted_network_role('127.0.0.1'),
            'proxy_header': TRUSTED_PROXY_HEADER,
            'cors_origins': CORS_ALLOWED_ORIGINS,
            'cors_loopback': CORS_ALLOWED_ORIGIN_REGEX == DEFAULT_CORS_ALLOWED_ORIGIN_REGEX,
            'compatible': not CORS_ALLOWED_ORIGIN_REGEX or CORS_ALLOWED_ORIGIN_REGEX == DEFAULT_CORS_ALLOWED_ORIGIN_REGEX,
        }

    @app.get('/api/light/native-admin-authorization', include_in_schema=False)
    async def native_admin_authorization(_user=Depends(require_admin)):
        return {'allowed': True}

    @app.get('/api/light/native-authorization', include_in_schema=False)
    async def native_authorization(_user=Depends(require_user)):
        return {'allowed': True}

    @app.get('/api/light/frontend', include_in_schema=False)
    async def frontend_config(_user=Depends(require_user)):
        value = os.getenv('LES_LIGHT_LEGACY_UI_URL', '')
        try:
            url = urlsplit(value)
            valid = (url.scheme == 'http' and url.hostname == '127.0.0.1' and bool(url.port)
                     and url.path == '/classic' and not (url.username or url.password or url.query or url.fragment))
        except ValueError:
            valid = False
        return {'legacy_url': value if valid else None}

    if os.getenv('LES_NATIVE_FRONTEND') != 'true' and FRONTEND_ROOT.is_dir():
        app.mount('/app', FrontendFiles(directory=FRONTEND_ROOT, html=True), name='les-frontend')
