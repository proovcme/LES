//! Choose saved, addressable fields for search; leave native records and geometry in the original.
use super::{Result, FAILED};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

fn fields(value: &Value, keys: &[&str]) -> Map<String, Value> {
    keys.iter()
        .filter_map(|&key| value.get(key).map(|field| (key.to_owned(), field.clone())))
        .collect()
}

fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.trim().is_empty())
}

fn rvt_parameters(metadata: &Value, definitions: &Value) -> Vec<Value> {
    let parameters = &metadata["parameter_sets"];
    let from_sets = parameters
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|set| set["parameters"].as_array().into_iter().flatten());
    let direct = ["resolved_family_parameters", "raw_field_parameters"]
        .iter()
        .flat_map(|&key| metadata[key].as_array().into_iter().flatten());
    let mut seen = BTreeSet::new();
    from_sets
        .chain(direct)
        .filter_map(|row| {
            let value = row.get("raw_value").or_else(|| row.get("value"))?;
            let id = [
                "parameter_id",
                "definition_parameter_id",
                "serialized_parameter_id",
            ]
            .into_iter()
            .map(|key| &row[key])
            .find(|id| id.is_number() || text(id).is_some());
            let definition = id.and_then(|id| {
                let key = id.as_str().map(str::to_owned).unwrap_or_else(|| id.to_string());
                definitions.get(key).filter(|definition| definition["parameter_id"] == *id)
            });
            let name = definition.and_then(|definition| text(&definition["caption"]))
                .or_else(|| ["name", "definition_name", "source_field"].into_iter().find_map(|key| text(&row[key])));
            if name.is_none() && id.is_none() {
                return None;
            }
            Some(json!({
                "name": name, "parameter_id": id, "value": value,
                "storage_type": text(&row["storage_type"]),
                "unit": text(&row["unit"]), "unit_id": &row["unit_id"],
                "display_unit_id": definition.and_then(|definition| text(&definition["unit_type_id"])),
                "spec_type_id": definition.and_then(|definition| text(&definition["spec_type_id"]))
            }))
        })
        .filter(|row| seen.insert(row.to_string()))
        .collect()
}

struct DwgSheet<'a> {
    handle: &'a str,
    name: &'a str,
    tallest: Option<(f64, &'a str, &'a str)>,
    conflicting_text: bool,
}

impl<'a> DwgSheet<'a> {
    fn consider_text(&mut self, entry: &'a Value) {
        if !matches!(entry["type"].as_str(), Some("TEXT" | "MTEXT")) || entry["isVisible"] == false
        {
            return;
        }
        let (Some(handle), Some(label), Some(height)) = (
            text(&entry["handle"]),
            dwg_text(entry),
            entry["textHeight"]
                .as_f64()
                .filter(|h| h.is_finite() && *h > 0.0),
        ) else {
            return;
        };
        match self.tallest {
            Some((best, _, best_label)) if height == best => {
                self.conflicting_text |= label != best_label;
            }
            Some((best, ..)) if height < best => {}
            _ => {
                self.tallest = Some((height, handle, label));
                self.conflicting_text = false;
            }
        }
    }

    fn context(&self) -> Value {
        let mut sheet = json!({"handle":self.handle,"name":self.name});
        if let Some((_, handle, label)) = self.tallest.filter(|(_, _, label)| {
            !self.conflicting_text && label.chars().take(241).count() <= 240
        }) {
            sheet["prominent_text"] = json!({"handle":handle,"text":label});
        }
        sheet
    }
}

pub(super) fn dwg_text(entry: &Value) -> Option<&str> {
    text(&entry["text"]).or_else(|| text(&entry["text"]["text"]))
}

fn dwg_sheets<'a>(
    layouts: &'a [Value],
    elements: &'a [Value],
) -> BTreeMap<&'a str, Option<DwgSheet<'a>>> {
    let mut sheets = BTreeMap::new();
    for layout in layouts {
        let Some(owner) = text(&layout["paperSpaceTableId"]) else {
            continue;
        };
        let sheet = match (text(&layout["handle"]), text(&layout["layoutName"])) {
            (Some(handle), Some(name)) if layout["tabOrder"].as_u64().is_some_and(|n| n > 0) => {
                Some(DwgSheet {
                    handle,
                    name,
                    tallest: None,
                    conflicting_text: false,
                })
            }
            _ => None,
        };
        sheets
            .entry(owner)
            .and_modify(|value| *value = None)
            .or_insert(sheet);
    }
    for entry in elements {
        if let Some(sheet) = text(&entry["ownerBlockRecordSoftId"])
            .and_then(|owner| sheets.get_mut(owner))
            .and_then(Option::as_mut)
        {
            sheet.consider_text(entry);
        }
    }
    sheets
}

pub(super) fn dwg_objects(value: &Value) -> Result<Vec<Value>> {
    if value["contract"] != "UvyazkaCadMetadata" || value["version"] != 1 {
        return Err(FAILED);
    }
    let sections = &value["sections"];
    let layouts = sections["layouts"].as_array().ok_or(FAILED)?;
    let blocks = sections["blocks"].as_array().ok_or(FAILED)?;
    let elements = sections["elements"].as_array().ok_or(FAILED)?;
    let layers = sections["layers"].as_array().ok_or(FAILED)?;
    let sheets = dwg_sheets(layouts, elements);
    let mut layer_handles = BTreeMap::new();
    for layer in layers {
        if let (Some(name), Some(handle)) = (text(&layer["name"]), text(&layer["handle"])) {
            layer_handles
                .entry(name)
                .and_modify(|old| *old = "")
                .or_insert(handle);
        }
    }
    layer_handles.retain(|_, handle| !handle.is_empty());
    let mut model_owners = blocks.iter().filter_map(|block| {
        let handle = text(&block["handle"])?;
        let layout = text(&block["layout"])?;
        (text(&block["name"])?.eq_ignore_ascii_case("*Model_Space")
            && layouts.iter().any(|entry| {
                entry["handle"] == layout
                    && entry["tabOrder"] == 0
                    && entry["paperSpaceTableId"] == handle
            }))
        .then_some(handle)
    });
    let model_owner = model_owners
        .next()
        .filter(|_| model_owners.next().is_none());
    let mut objects = Vec::new();
    for section in ["elements", "layers", "blocks", "layouts"] {
        for entry in sections[section].as_array().ok_or(FAILED)? {
            let Some(handle) = text(&entry["handle"]) else {
                continue;
            };
            let label = dwg_text(entry);
            let name = text(&entry["name"])
                .or(label)
                .or_else(|| text(&entry["layoutName"]));
            let mut properties = fields(
                entry,
                &[
                    "layer",
                    "tag",
                    "ownerBlockRecordSoftId",
                    "blockHeader",
                    "layoutName",
                    "name",
                    "viewScale",
                ],
            );
            if let Some(label) = label {
                properties.insert("text".into(), json!(label));
            }
            if section == "layers" {
                properties.extend(fields(
                    entry,
                    &[
                        "colorIndex",
                        "colorName",
                        "lineType",
                        "lineweight",
                        "frozen",
                        "off",
                        "locked",
                        "plotFlag",
                        "transparency",
                    ],
                ));
            }
            if let Some(attributes) = entry["attribs"].as_array() {
                let attributes: Vec<_> = attributes
                    .iter()
                    .map(|attr| {
                        json!({"tag":attr["tag"],"handle":attr["handle"],
                        "value":text(&attr["text"]).or_else(|| text(&attr["text"]["text"]))})
                    })
                    .collect();
                properties.insert("attributes".into(), json!(attributes));
            }
            if section == "elements"
                && name.is_none()
                && !properties.contains_key("attributes")
                && !matches!(
                    entry["type"].as_str(),
                    Some("INSERT" | "MINSERT" | "ATTRIB" | "ATTDEF" | "DIMENSION")
                )
            {
                continue;
            }
            let mut object = json!({"object_id":format!("{section}:{handle}"),"section":section,
                "native_id":handle,"name":name,"category":entry["type"],"properties":properties,
                "identifiers":{"handle":handle},"value_semantics":"saved_dwg"});
            if let Some(layer) = text(&entry["layer"]) {
                object["dwg_layer"] = json!({"name":layer,"handle":layer_handles.get(layer)});
            }
            if matches!(entry["type"].as_str(), Some("TEXT" | "MTEXT")) {
                if let Some(position) = super::dwg_tables::text_position(entry) {
                    object["dwg_position"] =
                        json!({"point":position,"owner_handle":entry["ownerBlockRecordSoftId"]});
                }
            }
            if section == "elements"
                && matches!(entry["type"].as_str(), Some("INSERT" | "MINSERT"))
                && model_owner.is_some_and(|owner| entry["ownerBlockRecordSoftId"] == owner)
            {
                let count = |key: &str| {
                    let value = &entry[key];
                    if value.is_null() {
                        Some(1)
                    } else {
                        value.as_u64().map(|n| n.max(1))
                    }
                };
                let quantity = count("rowCount")
                    .zip(count("columnCount"))
                    .and_then(|(r, c)| r.checked_mul(c));
                let block = text(&entry["name"]).or_else(|| {
                    let header = text(&entry["blockHeader"])?;
                    blocks
                        .iter()
                        .find(|block| block["handle"] == header)
                        .and_then(|block| text(&block["name"]))
                });
                if let Some((name, quantity)) = block.zip(quantity) {
                    object["dwg_insert"] = json!({"name":name,"quantity":quantity,"space":"model",
                        "owner_handle":model_owner,"layer":entry["layer"],
                        "attributes":object["properties"]["attributes"].as_array().cloned().unwrap_or_default()});
                }
            }
            let owner = match section {
                "elements" => text(&entry["ownerBlockRecordSoftId"]),
                "layouts" => text(&entry["paperSpaceTableId"]),
                _ => None,
            };
            if let Some(sheet) = owner
                .and_then(|owner| sheets.get(owner))
                .and_then(Option::as_ref)
            {
                object["drawing_sheet"] = sheet.context();
            }
            objects.push(object);
        }
    }
    let sheet_context = sheets
        .into_iter()
        .filter_map(|(owner, sheet)| sheet.map(|sheet| (owner, sheet.context())))
        .collect();
    let remaining = super::MAX_OBJECTS.saturating_sub(objects.len());
    objects.extend(super::dwg_tables::objects(
        elements,
        layouts,
        blocks,
        &sheet_context,
        &layer_handles,
        remaining,
    ));
    Ok(objects)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwg_inventory_projects_only_inserts_owned_by_model_space() {
        let drawing = json!({"contract":"UvyazkaCadMetadata","version":1,"sections":{
            "layers":[],"blocks":[
                {"handle":"17","name":"*Model_Space","layout":"1A"},
                {"handle":"33","name":"Luminaire"}
            ],"layouts":[
                {"handle":"1A","layoutName":"Model","paperSpaceTableId":"17","tabOrder":0},
                {"handle":"41","layoutName":"Sheet","paperSpaceTableId":"40","tabOrder":1}
            ],"elements":[
                {"type":"INSERT","handle":"50","ownerBlockRecordSoftId":"17","name":"Luminaire","layer":"LIGHT",
                    "attribs":[{"tag":"POWER_W","text":{"text":"18"}}]},
                {"type":"MINSERT","handle":"51","ownerBlockRecordSoftId":"17","blockHeader":"33","rowCount":2,"columnCount":3},
                {"type":"INSERT","handle":"52","ownerBlockRecordSoftId":"40","name":"Luminaire"},
                {"type":"INSERT","handle":"53","ownerBlockRecordSoftId":"33","name":"Luminaire"},
                {"type":"VIEWPORT","handle":"54","ownerBlockRecordSoftId":"40","name":"View"}
            ]
        }});
        let objects = dwg_objects(&drawing).unwrap();
        let inserts = objects
            .iter()
            .filter(|o| o["dwg_insert"].is_object())
            .collect::<Vec<_>>();
        assert_eq!(inserts.len(), 2);
        assert_eq!(inserts[0]["native_id"], "50");
        assert_eq!(inserts[0]["dwg_insert"]["attributes"][0]["value"], "18");
        assert_eq!(inserts[1]["dwg_insert"]["name"], "Luminaire");
        assert_eq!(inserts[1]["dwg_insert"]["quantity"], 6);
    }

    #[test]
    fn parameter_definition_labels_the_stored_value_without_changing_its_units() {
        let metadata = json!({"resolved_family_parameters":[
            {"parameter_id":17,"storage_type":"Double","raw_value":0.25}
        ]});
        let definitions = json!({"17":{"parameter_id":17,"caption":"Глубина вхождения",
            "unit_type_id":"autodesk.unit.unit:millimeters-1.0.1"}});
        let params = rvt_parameters(&metadata, &definitions);
        assert_eq!(params[0]["name"], "Глубина вхождения");
        assert_eq!(params[0]["value"], 0.25);
        assert!(params[0]["unit"].is_null());
        let rendered = les_core::engineering::rvt_parameter_line(&params[0]).unwrap();
        assert!(rendered.contains("0.25 (исходное значение Revit без пересчёта."));
        assert!(rendered.contains("Формат отображения проекта"));
    }

    #[test]
    fn rvt_context_uses_saved_member_and_element_references() {
        let classes = BTreeMap::from([
            ("FamilyInstance".to_owned(), "elements"),
            ("Level".to_owned(), "levels"),
            ("RbsPipingSystem".to_owned(), "systems"),
        ]);
        let saved = [
            json!({"class_name":"FamilyInstance","identity":{"element_id":8},
                "workset":{"name":"Рабочий набор 1"},
                "saved_metadata":{"root_references":{"m_assocLevelId":17,"m_masterSymbolId":23}}}),
            json!({"class_name":"FamilyInstance","identity":{"element_id":9},"root_fields":{"m_name":"Клапан"}}),
            json!({"class_name":"Level","identity":{"element_id":17},"root_fields":{"m_name":"Этаж 2"}}),
            json!({"class_name":"RbsPipingSystem","identity":{"element_id":42},
                "mep_system":{"name":"В1","members":[8,8,17]}}),
        ];
        let mut objects = saved
            .iter()
            .map(|row| rvt_object(row, &classes, &Value::Null).unwrap().unwrap())
            .collect::<Vec<_>>();
        rvt_context(&mut objects);
        assert_eq!(
            objects[0]["model_context"],
            json!({
                "levels":[{"native_id":"17","name":"Этаж 2","via":["m_assocLevelId"]}],
                "types":[{"native_id":"23","via":["m_masterSymbolId"]}],
                "systems":[{"native_id":"42","name":"В1","via":["mep_system.members"]}]
            })
        );
        assert_eq!(objects[0]["properties"]["workset"], "Рабочий набор 1");
        assert!(objects[1]["model_context"]["systems"].is_null());
        assert!(objects[2]["model_context"]["systems"].is_null());
    }

    #[test]
    fn dwg_text_uses_its_paper_space_owner_and_preserves_caption_origin() {
        let drawing = json!({"contract":"UvyazkaCadMetadata","version":1,"sections":{
            "layers":[],"blocks":[],"layouts":[
                {"handle":"10","layoutName":"Model","paperSpaceTableId":"F0","tabOrder":0},
                {"handle":"11","layoutName":"P01","paperSpaceTableId":"F1","tabOrder":1},
                {"handle":"12","layoutName":"P02","paperSpaceTableId":"F2","tabOrder":2}],
            "elements":[
                {"handle":"21","ownerBlockRecordSoftId":"F1","type":"MTEXT","text":"Контуры вентиляции","textHeight":5},
                {"handle":"22","ownerBlockRecordSoftId":"F1","type":"TEXT","text":"Расход 90 м³/ч","textHeight":3},
                {"handle":"23","ownerBlockRecordSoftId":"F2","type":"MTEXT","text":"Схема освещения","textHeight":6},
                {"handle":"24","ownerBlockRecordSoftId":"F2","type":"MTEXT","text":"Другой текст того же размера","textHeight":6},
                {"handle":"25","ownerBlockRecordSoftId":"F0","type":"TEXT","text":"Модель","textHeight":100},
                {"handle":"26","ownerBlockRecordSoftId":"ZZ","type":"TEXT","text":"Локальное определение блока","textHeight":100}]}});
        let rows = dwg_objects(&drawing).unwrap();
        let row = |id| rows.iter().find(|row| row["native_id"] == id).unwrap();
        assert_eq!(
            row("22")["drawing_sheet"],
            json!({"handle":"11","name":"P01",
            "prominent_text":{"handle":"21","text":"Контуры вентиляции"}})
        );
        assert_eq!(row("22")["properties"]["text"], "Расход 90 м³/ч");
        assert_eq!(
            row("23")["drawing_sheet"],
            json!({"handle":"12","name":"P02"})
        );
        assert!(row("25").get("drawing_sheet").is_none());
        assert!(row("26").get("drawing_sheet").is_none());
    }
}

pub(super) fn rvt_classes(inventory: &Value) -> Result<BTreeMap<String, &'static str>> {
    if inventory["schema_version"] != 2 || !inventory["schema_sha256"].is_string() {
        return Err(FAILED);
    }
    let mut parents = BTreeMap::new();
    for class in inventory["classes"].as_array().ok_or(FAILED)? {
        if let Some(name) = text(&class["name"]) {
            parents.insert(name, text(&class["parent"]));
        }
    }
    let mut selected = BTreeMap::new();
    for name in parents.keys().copied() {
        let mut ancestors = BTreeSet::new();
        let mut current = Some(name);
        while let Some(class) = current.filter(|class| ancestors.insert(*class)) {
            current = parents.get(class).copied().flatten();
        }
        let has = |roots: &[&str]| roots.iter().any(|root| ancestors.contains(root));
        let section = if has(&[
            "RbsHvacSystem",
            "RbsPipingSystem",
            "RbsElectricalSystem",
            "RbsSystem",
        ]) {
            "systems"
        } else if has(&["TableView"]) {
            "schedules"
        } else if has(&["DBView", "Viewport", "ScheduleInstanceBase"]) {
            "views"
        } else if has(&["Level"]) {
            "levels"
        } else if has(&["Family"]) {
            "families"
        } else if has(&[
            "FamilySymbol",
            "RbsPipeType",
            "RbsFlexPipeType",
            "RbsPipeInsulationType",
            "AbsDuctType",
            "AbsFlexDuctType",
            "RbsDuctInsulationType",
            "RbsDuctLiningType",
        ]) {
            "types"
        } else if has(&[
            "FamilyInstance",
            "RbsCurve",
            "RoomElem",
            "Room",
            "HostObj",
            "Opening",
            "Grid",
            "DirectShape",
            "ImportInstance",
            "GenericForm",
        ]) {
            "elements"
        } else {
            continue;
        };
        selected.insert(name.to_owned(), section);
    }
    if selected.is_empty() {
        return Err("NO_SEARCHABLE_TEXT");
    }
    Ok(selected)
}

pub(super) fn rvt_object(
    row: &Value,
    classes: &BTreeMap<String, &'static str>,
    definitions: &Value,
) -> Result<Option<Value>> {
    let class = row["class_name"].as_str().ok_or(FAILED)?;
    let section = classes.get(class).ok_or(FAILED)?;
    let Some(id) = row["identity"]["element_id"].as_u64() else {
        return Ok(None);
    };
    let metadata = &row["saved_metadata"];
    let system = &row["mep_system"];
    let root = row
        .get("root_fields")
        .unwrap_or(&row["graph"]["objects"][0]["fields"]);
    let name = text(&system["name"])
        .or_else(|| text(&root["m_viewName"]))
        .or_else(|| text(&root["m_name"]))
        .or_else(|| {
            if *section == "levels" {
                text(&root["m_text"])
            } else {
                None
            }
        })
        .or_else(|| text(&root["m_typeName"]));
    let mut properties = fields(metadata, &["category", "family", "type", "workset"]);
    let categories: BTreeSet<_> = metadata["raw_field_parameters"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|param| {
            param["source_field"] == "m_categoryId"
                && param["storage_type"] == "ElementId"
                && matches!(param["parameter_id"].as_i64(), Some(-1140362 | -1140363))
        })
        .filter_map(|param| param["raw_value"].as_i64())
        .filter(|id| !matches!(id, -1 | 0))
        .collect();
    if categories.len() == 1 {
        properties.insert("rvt_category_id".into(), json!(categories.first()));
    }
    if let Some(workset) = text(&row["workset"]["name"]) {
        properties.entry("workset").or_insert(json!(workset));
    }
    let parameters = rvt_parameters(metadata, definitions);
    properties.extend(fields(
        root,
        &[
            "m_name",
            "m_viewName",
            "m_viewDescription",
            "m_scale",
            "m_levelId",
            "m_familyName",
            "m_typeName",
            "m_sheetNumber",
            "m_mark",
        ],
    ));
    if let Some(system_name) = text(&system["name"]) {
        properties.insert(
            "system".into(),
            json!({"name":system_name,"class":system["class"],
            "members":system["members"]}),
        );
    }
    let references = &metadata["root_references"];
    let mut model_context = Map::new();
    for (kind, keys) in [
        ("levels", &["m_assocLevelId", "m_levelId"][..]),
        ("types", &["m_masterSymbolId", "m_typeId", "m_idType"][..]),
        ("families", &["m_familyId"][..]),
    ] {
        let mut items = BTreeMap::new();
        for &key in keys {
            if let Some(target) = references[key].as_u64().filter(|id| *id > 0) {
                items.entry(target).or_insert(key);
            }
        }
        if !items.is_empty() {
            model_context.insert(
                kind.into(),
                json!(items
                    .into_iter()
                    .map(|(id, key)| json!({"native_id":id.to_string(),"via":[key]}))
                    .collect::<Vec<_>>()),
            );
        }
    }
    if properties.is_empty()
        && parameters.is_empty()
        && model_context.is_empty()
        && !matches!(*section, "levels" | "families")
    {
        return Ok(None);
    }
    Ok(Some(
        json!({"object_id":format!("{section}:{id}:{class}"),"section":section,"native_id":id.to_string(),
        "name":name,"category":class,"properties":properties,"model_context":model_context,
        "saved_parameters":parameters,
        "identifiers":{"element_id":id.to_string(),"unique_id":row["identity"]["unique_id"]},
        "value_semantics":"saved_rvt"}),
    ))
}

pub(super) fn rvt_saved_label(row: &Value, section: &str) -> Option<String> {
    let graph = &row["graph"];
    let root = &graph["objects"][0]["fields"];
    if section == "levels" && row["class_name"] == "Level" {
        return text(&root["m_text"]).map(str::to_owned);
    }
    if section != "types" {
        return None;
    }
    if let Some(name) = text(&root["m_name"]).or_else(|| text(&root["m_typeName"])) {
        return Some(name.to_owned());
    }
    let offset = root["m_symbolInfo"]["offset"].as_u64()?;
    let target = graph["edges"]
        .as_array()?
        .iter()
        .find(|edge| edge["source_object_index"] == 0 && edge["pointer_offset"] == offset)?
        ["target_object_index"]
        .as_u64()?;
    let symbol = &graph["objects"][usize::try_from(target).ok()?];
    (symbol["class_name"] == "SymbolInfo")
        .then(|| text(&symbol["fields"]["m_name"]))
        .flatten()
        .map(str::to_owned)
}

pub(super) fn rvt_context(objects: &mut [Value]) {
    let mut names = BTreeMap::new();
    let mut memberships = BTreeMap::<u64, Vec<Value>>::new();
    for object in objects.iter() {
        let section = object["section"].as_str().unwrap();
        let id = object["native_id"].as_str().unwrap();
        if let Some(name) = text(&object["name"]) {
            names
                .entry((section.to_owned(), id.to_owned()))
                .and_modify(|entry| *entry = None)
                .or_insert(Some(name.to_owned()));
            if section == "systems" {
                let saved = &object["properties"]["system"]["members"];
                let members: BTreeSet<_> = saved
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_u64)
                    .collect();
                for member in members {
                    memberships
                        .entry(member)
                        .or_default()
                        .push(json!({"native_id":id,"name":name,"via":["mep_system.members"]}));
                }
            }
        }
    }
    for object in objects.iter() {
        if object["section"] != "types" {
            continue;
        }
        let id = object["native_id"].as_str().unwrap();
        let family = object["model_context"]["families"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|reference| reference["native_id"].as_str())
            .find_map(|id| {
                names
                    .get(&("families".into(), id.to_owned()))
                    .and_then(Option::as_deref)
            });
        if let (Some(family), Some(symbol)) = (family, text(&object["name"])) {
            if family != symbol {
                let label = format!("{family} / {symbol}");
                let key = ("types".into(), id.to_owned());
                if let Some(Some(name)) = names.get_mut(&key) {
                    *name = label;
                }
            }
        }
    }
    for object in objects {
        let context = object["model_context"].as_object_mut().unwrap();
        for (kind, items) in context.iter_mut() {
            for item in items.as_array_mut().unwrap() {
                let target = item["native_id"].as_str().unwrap();
                if let Some(Some(name)) = names.get(&(kind.to_owned(), target.to_owned())) {
                    item["name"] = json!(name);
                }
            }
        }
        if object["section"] != "elements" {
            continue;
        }
        if let Some(systems) = object["native_id"]
            .as_str()
            .and_then(|id| id.parse().ok())
            .and_then(|id| memberships.get(&id))
        {
            object["model_context"]["systems"] = json!(systems);
        }
    }
}
