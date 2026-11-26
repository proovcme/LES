use super::{envelope, skill_python, Context, Prepared, Result};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc},
    time::Instant,
};
pub(super) async fn run(
    context: Arc<Context>,
    turn: &Prepared,
    name: &str,
    args: Value,
    end: Instant,
) -> Result<Value> {
    let name = name.to_owned();
    let pending = (name == "run_skill_calculation")
        .then(|| skill_python::Pending(Arc::new(AtomicBool::new(false))));
    let cancelled = pending.as_ref().map(|guard| guard.0.clone());
    let session = turn.request["session_id"].clone();
    let project = turn.session["project_id"].as_i64().unwrap_or(0);
    tokio::task::spawn_blocking(move||->Result<Value>{
        if let Some(action)=name.strip_prefix("filesystem_"){
            let application=std::env::current_dir().map_err(|_|"FILESYSTEM_SCOPE_DENIED")?;
            let mut roots=json!({"docs":application.join("docs"),"storage_datasets":context.state_root.join("storage/datasets"),
                "rag_content":application.join("RAG_Content"),"artifacts":context.state_root.join("storage/artifacts")});
            for item in std::env::var("LES_TOOL_FS_EXTRA_ROOTS").unwrap_or_default().split(',').map(str::trim).filter(|s|!s.is_empty()){
                let (key,path)=item.split_once('=').unwrap_or(("extra",item));let path=PathBuf::from(path);
                if !path.is_absolute(){return Err("FILESYSTEM_SCOPE_DENIED".into());}
                let key:String=key.trim().chars().map(|c|if c.is_ascii_alphanumeric()||c=='_'||c=='-'{c}else{'_'}).take(40).collect();
                roots[if key.is_empty(){"extra"}else{&key}]=json!(path);
            }
            let result=crate::chat::unwrap(les_core::filesystem::execute(&json!({"schema":"les.filesystem.v1","action":action,"roots":roots,"arguments":args})))?;
            let mut payload=envelope(&name,action,result["result"].clone(),result["sources"].as_array().cloned().unwrap_or_default());
            for field in ["status","missing","warnings"] {if let Some(value)=result.get(field){payload[field]=value.clone();}}return Ok(payload);
        }
        if name=="agent_workspace"{
            let result=crate::chat::unwrap(les_core::agent_files::execute(&json!({"schema":"les.agent-files.v1","database":context.database,"state_root":context.state_root,
                "action":"tool_execute","arguments":{"session_id":session,"bound_project_id":project,"tool_arguments":args}})))?;
            return Ok(envelope(&name,args["operation"].as_str().ok_or("TOOL_SCHEMA_INVALID")?,result,vec![]));
        }
        let action=match name.as_str(){
            "list_installed_skills"=>"catalog","read_installed_skill"=>"read",
            "run_skill_calculation"=>"calculate",
            "read_skill_calculation"=>if args.get("job_id").and_then(Value::as_str).is_some_and(|s|!s.is_empty()) {"read_job"}else{"approvals"},
            _=>return Err("TOOL_NOT_REGISTERED".into())
        };
        let mut input=json!({"schema":"les.skills.v1","library":context.state_root.join("storage/skills"),"artifacts":context.state_root.join("artifacts/skill-compute"),"action":action});
        input.as_object_mut().unwrap().extend(args.as_object().ok_or("TOOL_SCHEMA_INVALID")?.clone());
        input["schema"]=json!("les.skills.v1");input["library"]=json!(context.state_root.join("storage/skills"));
        input["artifacts"]=json!(context.state_root.join("artifacts/skill-compute"));input["action"]=json!(action);
        let response=if let Some(cancelled)=cancelled {
            les_core::skills::execute_with_runtime(&input, || skill_python::resolve(&context.state_root, end, &cancelled))
        } else {
            les_core::skills::execute(&input)
        };
        if response["ok"]!=true{return Err(response["error"]["code"].as_str().unwrap_or("SKILL_EXECUTION_FAILED").into());}
        let mut payload=envelope(&name,"installed_skill",response["value"].clone(),vec![]);
        if response["value"]["interrupted"]==true{payload["status"]=json!("error");}Ok(payload)
    }).await.map_err(|_|"TOOL_HANDLER_ERROR")?
}
