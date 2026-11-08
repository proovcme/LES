//! Provider payload construction shared by native inference and compatibility callers.
use serde_json::{json, Value};

pub fn native_message(message: &Value) -> Result<Value, &'static str> {
    let mut result = message
        .as_object()
        .cloned()
        .ok_or("INVALID_MODEL_MESSAGE")?;
    if let Some(value) = result.get("tool_calls").filter(|v| !v.is_null()) {
        let mut calls = Vec::new();
        for call in value.as_array().ok_or("INVALID_NATIVE_TOOL_ARGUMENTS")? {
            let mut call = call
                .as_object()
                .cloned()
                .ok_or("INVALID_NATIVE_TOOL_ARGUMENTS")?;
            let function = call
                .get_mut("function")
                .and_then(Value::as_object_mut)
                .ok_or("INVALID_NATIVE_TOOL_ARGUMENTS")?;
            let arguments = function
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let arguments = if let Some(raw) = arguments.as_str() {
                serde_json::from_str(raw).map_err(|_| "INVALID_NATIVE_TOOL_ARGUMENTS")?
            } else {
                arguments
            };
            if !arguments.is_object() {
                return Err("INVALID_NATIVE_TOOL_ARGUMENTS");
            }
            function.insert("arguments".into(), arguments);
            calls.push(json!(call));
        }
        result.insert("tool_calls".into(), json!(calls));
    }
    if result.get("role").is_some_and(|v| v == "tool") {
        if let Some(name) = result
            .get("name")
            .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
            .cloned()
        {
            result.insert("tool_name".into(), name);
        }
    }
    Ok(json!(result))
}
pub fn prepare(
    connection: &Value,
    request: &Value,
    native: bool,
    streaming: bool,
) -> Result<Value, &'static str> {
    let model = connection["model_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or("MODEL_ID_REQUIRED")?;
    let output = request["max_output_tokens"]
        .as_u64()
        .filter(|v| *v > 0)
        .ok_or("MODEL_OUTPUT_LIMIT_INVALID")?;
    let reasoning = request["reasoning_enabled"]
        .as_bool()
        .ok_or("MODEL_REASONING_INVALID")?;
    let messages = request["messages"]
        .as_array()
        .ok_or("INVALID_MODEL_MESSAGE")?;
    let tools = request["tools"].as_array().ok_or("INVALID_MODEL_TOOLS")?;
    let format = &request["response_format"];
    let mut body;
    if native {
        let mut messages = messages
            .iter()
            .map(native_message)
            .collect::<Result<Vec<_>, _>>()?;
        for message in &mut messages {
            if let Some(parts) = message["content"].as_array() {
                let text = parts
                    .iter()
                    .filter(|v| v["type"] == "text")
                    .map(|v| v["text"].as_str().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join("\n");
                let mut images = Vec::new();
                for part in parts.iter().filter(|v| v["type"] == "image_url") {
                    let url = part["image_url"]["url"].as_str().unwrap_or("");
                    if !url.starts_with("data:image/") {
                        return Err("INVALID_NATIVE_IMAGE");
                    }
                    images.push(
                        url.split_once(";base64,")
                            .ok_or("INVALID_NATIVE_IMAGE")?
                            .1
                            .to_string(),
                    );
                }
                message["content"] = json!(text);
                if !images.is_empty() {
                    message["images"] = json!(images);
                }
            }
        }
        let context = connection["input_token_limit"]
            .as_u64()
            .filter(|v| *v > 0)
            .ok_or("MODEL_CONTEXT_INVALID")?;
        let mut options = json!({"num_predict":output,"num_ctx":context});
        if !request["temperature"].is_null() {
            options["temperature"] = request["temperature"].clone();
        }
        body = json!({"model":model,"messages":messages,"think":reasoning,"stream":true,"options":options});
        if format["type"] == "json_schema" {
            let schema = format["json_schema"]
                .get("schema")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !schema.is_object() {
                return Err("INVALID_RESPONSE_FORMAT");
            }
            body["format"] = schema;
        } else if format["type"] == "json_object" {
            body["format"] = json!("json");
        }
    } else {
        let mut system = Vec::new();
        let mut conversation = Vec::new();
        for message in messages {
            if !message.is_object() {
                return Err("INVALID_MODEL_MESSAGE");
            }
            if message["role"] == "system" {
                system.push(message.clone());
            } else {
                conversation.push(message.clone());
            }
        }
        if !system.is_empty() && system.iter().all(|v| v["content"].is_string()) {
            let combined = system
                .iter()
                .map(|v| v["content"].as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n\n");
            system.truncate(1);
            system[0]["content"] = json!(combined);
        }
        system.extend(conversation);
        let field = connection["max_output_field"]
            .as_str()
            .unwrap_or("max_tokens");
        if !matches!(field, "max_tokens" | "max_completion_tokens") {
            return Err("INVALID_OUTPUT_FIELD");
        }
        body = json!({"model":model,"messages":system});
        body[field] = json!(output);
        let local = connection["locality"] != "remote";
        if reasoning && !local {
            return Err("REASONING_MODE_UNSUPPORTED");
        }
        if local {
            body["chat_template_kwargs"] = json!({"enable_thinking":reasoning});
        }
        if !request["temperature"].is_null() {
            body["temperature"] = request["temperature"].clone();
        }
        if !format.is_null() {
            body["response_format"] = format.clone();
        }
        if streaming {
            body["stream"] = json!(true);
        }
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    Ok(body)
}
pub(crate) fn execute(request: &Value) -> Result<Value, &'static str> {
    if request["schema"] != "les.model-request.v1" {
        return Err("MODEL_REQUEST_SCHEMA_INVALID");
    }
    let value = if request["action"] == "native_message" {
        native_message(&request["message"])?
    } else {
        prepare(
            &request["connection"],
            &request["request"],
            request["native"] == true,
            request["streaming"] == true,
        )?
    };
    Ok(json!({"value":value}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_arguments_remain_exact_and_the_callers_message_is_not_changed() {
        let source = json!({"role":"assistant","tool_calls":[{"id":"a","function":{"name":"read","arguments":"{\"ref\":123456789012345678901234567890}"}}]});
        let result = native_message(&source).unwrap();
        assert!(source["tool_calls"][0]["function"]["arguments"].is_string());
        assert_eq!(
            result["tool_calls"][0]["function"]["arguments"]["ref"].to_string(),
            "123456789012345678901234567890"
        );
    }
}
