"""A real, explicitly registered HTTP embedding service for shipping batch tests."""
from datetime import datetime,timezone
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
from threading import Thread
import json

from backend.embedding_client import EmbedClient
from proxy.services.model_connection_contracts import CapabilityName,CapabilityObservation,CapabilitySnapshot,CapabilityState,ConnectionLocality,ConnectionRole
from proxy.services.model_connection_registry_service import ModelConnectionRegistry
from proxy.services.model_connection_resolver_service import ModelConnectionResolver
from proxy.services.model_secret_service import EnvironmentSecretStore


class NativeBatchEmbeddings:
    def __init__(self,database,*,native=False):
        self.calls=[];self.requests=[];self.on_request=None
        owner=self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self,*args):pass
            def do_POST(self):
                value=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                owner.requests.append(value);owner.calls.extend(value['input'])
                if owner.on_request:owner.on_request(value)
                vectors=[[1.,0.] for _ in value['input']]
                response=({'model':'fixture','embeddings':vectors} if native else
                    {'model':'fixture','data':[{'index':i,'embedding':v} for i,v in enumerate(vectors)]})
                body=json.dumps(response).encode()
                self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)))
                self.end_headers();self.wfile.write(body)
        self.server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
        self.thread=Thread(target=self.server.serve_forever,daemon=True);self.thread.start()
        try:
            registry=ModelConnectionRegistry(str(database));url=f'http://127.0.0.1:{self.server.server_port}'
            revision=registry.create_connection(display_name='Batch HTTP fixture',base_url=url+'/v1',model_id='fixture',
                locality=ConnectionLocality.LOOPBACK,requested_context_tokens=4096,secret_ref=None,extension_type=None,actor='admin:test')
            observed=datetime(2026,1,1,tzinfo=timezone.utc)
            options={'observed_context_tokens':'4096',**({'embedding_protocol':'native_embedding_v1'} if native else {})}
            registry.save_capability_snapshot(CapabilitySnapshot('cap:batch-http',revision.revision_id,
                (CapabilityObservation(CapabilityName.EMBEDDINGS,CapabilityState.SUPPORTED,'probe',observed),),
                observed,datetime(2099,1,1,tzinfo=timezone.utc),options),actor='admin:test')
            registry.bind_role(ConnectionRole.EMBEDDINGS,revision.revision_id,expected_binding_revision=None,actor='admin:test')
            resolver=ModelConnectionResolver(registry=registry,secret_store=EnvironmentSecretStore(Path(database).parent/'.env'),allow_private_http=True)
            self.embedder=EmbedClient(url,model='fixture',backend='openai',connection_mode='active',connection_resolver=resolver)
        except BaseException:
            self.close();raise

    def close(self):
        self.server.shutdown();self.server.server_close();self.thread.join(timeout=5)
