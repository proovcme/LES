//! Scoped, paginated filename discovery. Catalog metadata never becomes factual evidence.
use super::*;
pub(super) async fn search(
    context: Arc<Context>,
    turn: &Prepared,
    args: &Value,
    datasets: &[Value],
) -> Result<Value> {
    let limit = args["limit"].as_u64().unwrap_or(12).clamp(1, 50);
    let result = page(context, turn, args, datasets, limit).await?;
    Ok(envelope(
        "search_sources",
        "document_catalog",
        result,
        vec![],
    ))
}
pub(super) async fn map(context: Arc<Context>, turn: &Prepared, args: Value) -> Result<Value> {
    let dataset = args["dataset_id"].clone();
    let mut result = page(context, turn, &json!({}), &[dataset.clone()], 100).await?;
    result["dataset_id"] = dataset;
    result["limit"] = json!(100);
    Ok(envelope("dataset_map", "dataset_map", result, vec![]))
}
async fn page(
    context: Arc<Context>,
    turn: &Prepared,
    args: &Value,
    datasets: &[Value],
    limit: u64,
) -> Result<Value> {
    for dataset in datasets {
        scoped(turn, dataset)?;
    }
    let database = context.database.clone();
    let datasets = datasets.to_vec();
    let q = args["q"].clone();
    let offset = args["offset"].as_u64().unwrap_or(0);
    if offset > 100000 {
        return Err("SOURCE_OFFSET_INVALID".into());
    }
    let selected = if turn.request["selected_sources_only"] == true {
        selected_files(turn)
    } else {
        vec![]
    };
    let result=tokio::task::spawn_blocking(move||->Result<Value>{
        let mut documents=Vec::new(); let mut total=0; let mut skip=offset;
        for dataset in datasets {
            let mut input=json!({"dataset_id":dataset,"q":q,"limit":1,"offset":0,"doc_filter":selected});
            let count=super::super::super::unwrap(crate::library::catalog(&database,"library_documents",input.clone()))?["total"].as_u64().ok_or("SOURCE_CATALOG_UNAVAILABLE")?;
            total+=count;
            if skip>=count {skip-=count;continue;}
            if documents.len()>=limit as usize {continue;}
            input["offset"]=json!(skip); input["limit"]=json!(limit-documents.len() as u64);skip=0;
            let page=super::super::super::unwrap(crate::library::catalog(&database,"library_documents",input))?;
            documents.extend(page["documents"].as_array().ok_or("SOURCE_CATALOG_UNAVAILABLE")?.iter().cloned());
        }
        let next=offset+documents.len() as u64;
        Ok(json!({"documents":documents,"total":total,"offset":offset,"next_offset":if next<total{Some(next)}else{None},
            "evidence_admissible":false,"note":"Это имена и статусы файлов, а не их содержимое. Найденный документ ещё нужно прочитать. Дата файла и порядок выдачи не определяют актуальность редакции."}))
    }).await.map_err(|_|"SOURCE_CATALOG_UNAVAILABLE")??;
    Ok(result)
}
