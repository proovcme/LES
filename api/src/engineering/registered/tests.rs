use super::*;
use axum::{routing::any, Router};
use std::sync::atomic::{AtomicUsize, Ordering};

async fn fixture() -> (Companion, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let writes = Arc::new(AtomicUsize::new(0));
    let observed = writes.clone();
    let app=Router::new().fallback(any(move |request:Request| {
        let writes=observed.clone();
        async move {
            let path=request.uri().path();
            let value=match path {
                "/api/models/status"=>json!({"write_token":"b".repeat(32),"readers":{"rvt":{"available":true},"dwg":{"available":false}}}),
                "/api/documents"=>json!({"project_id":"example","documents":[
                    {"doc_id":"doc-1","relative_path":"Model.rvt","sha256":"a".repeat(64)},
                    {"doc_id":"doc-2","relative_path":"Drawing.DWG","sha256":"c".repeat(64)},
                    {"doc_id":"doc-3","relative_path":"Notes.pdf"}],"next_offset":30}),
                "/api/models/read"=>{
                    assert_eq!(request.method(),"POST");
                    assert_eq!(request.headers()["X-Uvyazka-Token"],"b".repeat(32));
                    assert_eq!(request.headers()["Origin"],format!("http://{}",request.headers()["Host"].to_str().unwrap()));
                    let bytes=to_bytes(request.into_body(),16384).await.unwrap();let body:Value=serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(body,json!({"doc_id":"doc-1","request_id":"read-once"}));
                    writes.fetch_add(1,Ordering::SeqCst);
                    json!({"project_id":"example","dataset_id":"snapshot","source":{"doc_id":"doc-1","sha256":"a".repeat(64)},"freshness":{"status":"content_hash_matches"}})
                },
                _=>panic!("Unexpected companion route")
            };
            json_response(StatusCode::OK,value,"synthetic.uvyazka")
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        Companion::new(&format!("http://127.0.0.1:{port}/")).unwrap(),
        writes,
        server,
    )
}
#[test]
fn inventory_keeps_unavailable_formats_and_continuation_without_exposing_credentials() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (client, writes, server) = fixture().await;
            let page = client
                .registered_sources(&json!({"project_id":"example"}))
                .await
                .unwrap();
            assert_eq!(page["items"].as_array().unwrap().len(), 2);
            assert_eq!(page["items"][0]["available"], true);
            assert_eq!(page["items"][1]["available"], false);
            assert_eq!(page["next_offset"], 30);
            assert!(!page.to_string().contains(&"b".repeat(32)));
            assert_eq!(writes.load(Ordering::SeqCst), 0);
            server.abort();
        });
}
#[test]
fn explicit_read_uses_registered_identity_and_checks_the_selected_revision() {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async{
        let(client,writes,server)=fixture().await;
        let mut input=json!({"project_id":"example","doc_id":"doc-1","request_id":"read-once","expected_source_sha256":"a".repeat(64),"path":"ignored-untrusted-path"});
        assert_eq!(client.read_registered(&input).await.unwrap(),"snapshot");
        input["expected_source_sha256"]=json!("c".repeat(64));
        assert_eq!(client.read_registered(&input).await.unwrap_err(),"ENGINEERING_SOURCE_CHANGED");
        assert_eq!(writes.load(Ordering::SeqCst),2);server.abort();
    });
}
