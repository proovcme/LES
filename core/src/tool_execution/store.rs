//! Atomic approval binding, idempotency claims and actor-bound result references.
use super::{canonical, now, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};
const ERROR: &str = "TOOL_EXECUTION_STATE_ERROR";
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().ok_or(ERROR)
}
fn open(database: &str) -> Result<Connection> {
    if !Path::new(database).is_absolute() {
        return Err(ERROR);
    }
    let conn = Connection::open(database).map_err(|_| ERROR)?;
    conn.busy_timeout(Duration::from_secs(10))
        .map_err(|_| ERROR)?;
    conn.execute_batch(include_str!("schema.sql"))
        .map_err(|_| ERROR)?;
    Ok(conn)
}
pub fn execute(database: &str, action: &str, args: &Value) -> Result<Value> {
    let mut conn = open(database)?;
    match action{
        "ensure"=>Ok(Value::Null),
        "issue"=>{let v=&args["receipt"];conn.execute("INSERT INTO tool_approval_receipts VALUES(?1,?2,?3,?4,?5,?6,?7)",params![text(v,"receipt_id")?,text(v,"status")?,text(v,"proposal_revision")?,text(v,"tool_name")?,text(v,"argument_sha256")?,text(v,"actor_id")?,v["expires_at_epoch"].as_f64().ok_or(ERROR)?]).map_err(|_|ERROR)?;Ok(Value::Null)},
        "approval"=>{conn.query_row("SELECT receipt_id,status,proposal_revision,tool_name,argument_sha256,actor_id,expires_at_epoch FROM tool_approval_receipts WHERE receipt_id=?1",[text(args,"receipt_id")?],|r|Ok(json!({"receipt_id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"proposal_revision":r.get::<_,String>(2)?,"tool_name":r.get::<_,String>(3)?,"argument_sha256":r.get::<_,String>(4)?,"actor_id":r.get::<_,String>(5)?,"expires_at_epoch":r.get::<_,f64>(6)?}))).optional().map(|v|v.unwrap_or(Value::Null)).map_err(|_|ERROR)},
        "revoke"=>{let count=conn.execute("UPDATE tool_approval_receipts SET status='revoked' WHERE receipt_id=?1",[text(args,"receipt_id")?]).map_err(|_|ERROR)?;Ok(json!(count!=0))},
        "put_cursor"=>{
            let tx=conn.transaction().map_err(|_|ERROR)?;
            tx.execute("INSERT INTO tool_result_cursors VALUES(?1,?2,?3,?4)",params![text(args,"cursor")?,text(args,"actor_id")?,args["expires_at_epoch"].as_f64().ok_or(ERROR)?,canonical::encode(&args["result"])?]).map_err(|_|ERROR)?;
            tx.execute("DELETE FROM tool_result_cursors WHERE expires_at_epoch<=?1",[now()]).map_err(|_|ERROR)?;tx.commit().map_err(|_|ERROR)?;Ok(Value::Null)
        },
        "resolve_cursor"=>{let raw=conn.query_row("SELECT result_json FROM tool_result_cursors WHERE cursor=?1 AND actor_id=?2 AND expires_at_epoch>?3",params![text(args,"cursor")?,text(args,"actor_id")?,now()],|r|r.get::<_,String>(0)).optional().map_err(|_|ERROR)?;match raw{Some(raw)=>serde_json::from_str(&raw).map_err(|_|ERROR),None=>Ok(Value::Null)}},
        "claim"=>{
            let actor=text(args,"actor_id")?;let tool=text(args,"tool_name")?;let key=text(args,"idempotency_key")?;let fingerprint=text(args,"fingerprint")?;let receipt=text(args,"approval_receipt_id")?;
            let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_|ERROR)?;
            if args["verify_approval"]==true {
                let receipt_value=tx.query_row("SELECT status,proposal_revision,tool_name,argument_sha256,actor_id,expires_at_epoch FROM tool_approval_receipts WHERE receipt_id=?1",[receipt],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,f64>(5)?))).optional().map_err(|_|ERROR)?;
                if !receipt_value.is_some_and(|(status,revision,name,hash,owner,expires)|status=="approved"&&expires>now()&&name==tool&&owner==actor&&args["proposal_revision"]==revision&&args["argument_sha256"]==hash){
                    tx.commit().map_err(|_|ERROR)?;return Ok(json!({"state":"approval_invalid","envelope":null}));
                }
            }
            let bound=tx.query_row("SELECT actor_id,tool_name,idempotency_key,fingerprint FROM tool_approval_uses WHERE receipt_id=?1",[receipt],|r|Ok([r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?])).optional().map_err(|_|ERROR)?;
            if let Some(bound)=bound{if bound!=[actor,tool,key,fingerprint]{tx.commit().map_err(|_|ERROR)?;return Ok(json!({"state":"approval_used","envelope":null}));}}
            else{tx.execute("INSERT INTO tool_approval_uses VALUES(?1,?2,?3,?4,?5,?6)",params![receipt,actor,tool,key,fingerprint,now()]).map_err(|_|ERROR)?;}
            let previous=tx.query_row("SELECT fingerprint,state,envelope_json FROM tool_idempotency_ledger WHERE actor_id=?1 AND tool_name=?2 AND idempotency_key=?3",params![actor,tool,key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional().map_err(|_|ERROR)?;
            let value=match previous{
                None=>{tx.execute("INSERT INTO tool_idempotency_ledger VALUES(?1,?2,?3,?4,'in_progress','',?5)",params![actor,tool,key,fingerprint,now()]).map_err(|_|ERROR)?;json!({"state":"claimed","envelope":null})},
                Some((saved,_,_))if saved!=fingerprint=>json!({"state":"conflict","envelope":null}),
                Some((_,state,raw))if state=="completed"=>json!({"state":state,"envelope":serde_json::from_str::<Value>(&raw).map_err(|_|ERROR)?}),
                Some((_,state,_))=>json!({"state":state,"envelope":null})
            };tx.commit().map_err(|_|ERROR)?;Ok(value)
        },
        "complete"|"ambiguous"=>{
            let raw=if action=="complete"{canonical::encode(&args["envelope"])?}else{String::new()};
            let changed=conn.execute("UPDATE tool_idempotency_ledger SET state=?1,envelope_json=?2,updated_at_epoch=?3 WHERE actor_id=?4 AND tool_name=?5 AND idempotency_key=?6 AND fingerprint=?7",params![if action=="complete"{"completed"}else{"ambiguous"},raw,now(),text(args,"actor_id")?,text(args,"tool_name")?,text(args,"idempotency_key")?,text(args,"fingerprint")?]).map_err(|_|ERROR)?;
            if changed!=1{return Err(ERROR);}Ok(Value::Null)
        },_=>Err(ERROR)
    }
}
