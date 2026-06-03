//! OpenAI Responses API 流式转换模块
//!
//! 实现 Responses API SSE → Anthropic SSE 格式转换。
//!
//! Responses API 使用命名事件 (named events) 的生命周期模型：
//! response.created → output_item.added → content_part.added →
//! output_text.delta → content_part.done → output_item.done → response.completed
//!
//! 与 Chat Completions 的 delta chunk 模型完全不同，需要独立的状态机处理。

use super::transform_responses::{
    build_anthropic_usage_from_responses, map_responses_stop_reason,
    sanitize_anthropic_tool_use_input_json,
};
use crate::proxy::sse::{strip_sse_field, take_sse_block};
use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[inline]
fn response_object_from_event(data: &Value) -> &Value {
    data.get("response").unwrap_or(data)
}

#[inline]
fn content_part_key(data: &Value) -> Option<String> {
    if let (Some(item_id), Some(content_index)) = (
        data.get("item_id").and_then(|v| v.as_str()),
        data.get("content_index").and_then(|v| v.as_u64()),
    ) {
        return Some(format!("part:{item_id}:{content_index}"));
    }
    if let (Some(output_index), Some(content_index)) = (
        data.get("output_index").and_then(|v| v.as_u64()),
        data.get("content_index").and_then(|v| v.as_u64()),
    ) {
        return Some(format!("part:out:{output_index}:{content_index}"));
    }
    None
}

#[inline]
fn tool_item_key_from_added(data: &Value, item: &Value) -> Option<String> {
    if let Some(item_id) = item.get("id").and_then(|v| v.as_str()) {
        return Some(format!("tool:{item_id}"));
    }
    if let Some(item_id) = data.get("item_id").and_then(|v| v.as_str()) {
        return Some(format!("tool:{item_id}"));
    }
    if let Some(output_index) = data.get("output_index").and_then(|v| v.as_u64()) {
        return Some(format!("tool:out:{output_index}"));
    }
    None
}

#[inline]
fn tool_item_key_from_event(data: &Value) -> Option<String> {
    if let Some(item_id) = data.get("item_id").and_then(|v| v.as_str()) {
        return Some(format!("tool:{item_id}"));
    }
    if let Some(output_index) = data.get("output_index").and_then(|v| v.as_u64()) {
        return Some(format!("tool:out:{output_index}"));
    }
    None
}

#[inline]
fn reasoning_item_key(data: &Value) -> Option<String> {
    if let Some(item_id) = data
        .get("item_id")
        .and_then(|v| v.as_str())
        .or_else(|| data.pointer("/item/id").and_then(|v| v.as_str()))
    {
        return Some(format!("reasoning:{item_id}"));
    }
    if let Some(output_index) = data.get("output_index").and_then(|v| v.as_u64()) {
        return Some(format!("reasoning:out:{output_index}"));
    }
    None
}

#[inline]
fn reasoning_summary_key(data: &Value) -> Option<String> {
    let summary_index = data
        .get("summary_index")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    reasoning_item_key(data).map(|key| format!("{key}:{summary_index}"))
}

#[inline]
fn resolve_reasoning_index(
    data: &Value,
    next_content_index: &mut u32,
    index_by_key: &mut HashMap<String, u32>,
    fallback_open_index: &mut Option<u32>,
) -> u32 {
    if let Some(k) = reasoning_item_key(data) {
        if let Some(existing) = index_by_key.get(&k).copied() {
            existing
        } else {
            let assigned = *next_content_index;
            *next_content_index += 1;
            index_by_key.insert(k, assigned);
            assigned
        }
    } else if let Some(existing) = *fallback_open_index {
        existing
    } else {
        let assigned = *next_content_index;
        *next_content_index += 1;
        *fallback_open_index = Some(assigned);
        assigned
    }
}

fn reasoning_summary_part_text(data: &Value) -> Option<&str> {
    data.get("part")
        .and_then(|part| {
            part.get("text")
                .and_then(|v| v.as_str())
                .or_else(|| part.get("content").and_then(|v| v.as_str()))
        })
        .filter(|text| !text.is_empty())
}

fn reasoning_summary_event_text(data: &Value) -> Option<&str> {
    data.get("delta")
        .or_else(|| data.get("text"))
        .and_then(|v| v.as_str())
        .filter(|text| !text.is_empty())
}

fn reasoning_summary_item_parts(item: &Value) -> Vec<(u64, String)> {
    let Some(summary) = item.get("summary") else {
        return Vec::new();
    };
    if let Some(text) = summary.as_str().filter(|text| !text.is_empty()) {
        return vec![(0, text.to_string())];
    }

    let Some(parts) = summary.as_array() else {
        return Vec::new();
    };
    parts
        .iter()
        .enumerate()
        .filter_map(|(index, part)| {
            let summary_index = part
                .get("summary_index")
                .and_then(|v| v.as_u64())
                .unwrap_or(index as u64);
            let text = part
                .get("text")
                .and_then(|v| v.as_str())
                .or_else(|| part.get("content").and_then(|v| v.as_str()))
                .or_else(|| part.as_str())
                .filter(|text| !text.is_empty())?;
            Some((summary_index, text.to_string()))
        })
        .collect()
}

/// Resolve content index for a text/refusal content part event.
///
/// Uses `content_part_key` to look up or assign a stable index, falling back to
/// `fallback_open_index` when no key is available.
#[inline]
fn resolve_content_index(
    data: &Value,
    next_content_index: &mut u32,
    index_by_key: &mut HashMap<String, u32>,
    fallback_open_index: &mut Option<u32>,
) -> u32 {
    if let Some(k) = content_part_key(data) {
        if let Some(existing) = index_by_key.get(&k).copied() {
            existing
        } else {
            let assigned = *next_content_index;
            *next_content_index += 1;
            index_by_key.insert(k, assigned);
            assigned
        }
    } else if let Some(existing) = *fallback_open_index {
        existing
    } else {
        let assigned = *next_content_index;
        *next_content_index += 1;
        *fallback_open_index = Some(assigned);
        assigned
    }
}

#[derive(Debug, Clone)]
struct PendingToolBlock {
    key: String,
    call_id: String,
    name: String,
    arguments: String,
}

fn pending_tool_block_events(
    pending_tools: &mut Vec<PendingToolBlock>,
    pending_tool_deltas: &mut Vec<(String, String)>,
    next_content_index: &mut u32,
) -> Vec<Bytes> {
    let mut events = Vec::new();
    let mut order: Vec<(String, u32)> = Vec::new();
    let mut meta_by_key: HashMap<String, (u32, bool, String)> = HashMap::new();

    for tool in pending_tools.drain(..) {
        let index = *next_content_index;
        *next_content_index += 1;
        let is_read = tool.name == "Read";
        let key = tool.key;

        let start_event = json!({
            "type": "content_block_start",
            "index": index,
            "content_block": {
                "type": "tool_use",
                "id": tool.call_id,
                "name": tool.name
            }
        });
        events.push(Bytes::from(format!(
            "event: content_block_start\ndata: {}\n\n",
            serde_json::to_string(&start_event).unwrap_or_default()
        )));

        order.push((key.clone(), index));
        meta_by_key.insert(key, (index, is_read, tool.arguments));
    }

    let mut emitted_non_read_keys: HashSet<String> = HashSet::new();
    for (key, delta) in pending_tool_deltas.drain(..) {
        let Some((index, is_read, _arguments)) = meta_by_key.get(&key) else {
            continue;
        };
        if *is_read {
            continue;
        }
        let delta_event = json!({
            "type": "content_block_delta",
            "index": index,
            "delta": {
                "type": "input_json_delta",
                "partial_json": delta
            }
        });
        events.push(Bytes::from(format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::to_string(&delta_event).unwrap_or_default()
        )));
        emitted_non_read_keys.insert(key);
    }

    for (key, index) in &order {
        let Some((_index, is_read, arguments)) = meta_by_key.get(key) else {
            continue;
        };
        if !*is_read && emitted_non_read_keys.contains(key) {
            continue;
        }
        let arguments = if *is_read {
            sanitize_anthropic_tool_use_input_json("Read", arguments)
        } else {
            arguments.clone()
        };
        if arguments.is_empty() {
            continue;
        }
        let delta_event = json!({
            "type": "content_block_delta",
            "index": index,
            "delta": {
                "type": "input_json_delta",
                "partial_json": arguments
            }
        });
        events.push(Bytes::from(format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::to_string(&delta_event).unwrap_or_default()
        )));
    }

    for (_key, index) in order {
        let stop_event = json!({
            "type": "content_block_stop",
            "index": index
        });
        events.push(Bytes::from(format!(
            "event: content_block_stop\ndata: {}\n\n",
            serde_json::to_string(&stop_event).unwrap_or_default()
        )));
    }

    events
}

/// 创建从 Responses API SSE 到 Anthropic SSE 的转换流
///
/// 状态机跟踪: message_id, current_model, has_sent_message_start, item/content index map
/// SSE 解析支持 named events (event: + data: 行)
pub fn create_anthropic_sse_stream_from_responses<E: std::error::Error + Send + 'static>(
    stream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut buffer = String::new();
        let mut utf8_remainder: Vec<u8> = Vec::new();
        let mut message_id: Option<String> = None;
        let mut current_model: Option<String> = None;
        let mut has_sent_message_start = false;
        let mut has_tool_use = false;
        let mut next_content_index: u32 = 0;
        let mut index_by_key: HashMap<String, u32> = HashMap::new();
        let mut open_indices: HashSet<u32> = HashSet::new();
        let mut fallback_open_index: Option<u32> = None;
        let mut current_text_index: Option<u32> = None;
        let mut current_reasoning_index: Option<u32> = None;
        let mut pending_tools: Vec<PendingToolBlock> = Vec::new();
        let mut pending_tool_by_key: HashMap<String, usize> = HashMap::new();
        let mut pending_tool_deltas: Vec<(String, String)> = Vec::new();
        let mut has_seen_text_content = false;
        let mut tool_index_by_item_id: HashMap<String, u32> = HashMap::new();
        let mut tool_name_by_index: HashMap<u32, String> = HashMap::new();
        let mut tool_args_by_index: HashMap<u32, String> = HashMap::new();
        let mut last_tool_index: Option<u32> = None;
        let mut reasoning_emitted_keys: HashSet<String> = HashSet::new();

        tokio::pin!(stream);

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    crate::proxy::sse::append_utf8_safe(&mut buffer, &mut utf8_remainder, &bytes);

                    // SSE 事件由 \n\n 分隔
                    while let Some(block) = take_sse_block(&mut buffer) {
                        if block.trim().is_empty() {
                            continue;
                        }

                        // 解析 SSE 块：提取 event: 和 data: 行
                        let mut event_type: Option<String> = None;
                        let mut data_parts: Vec<String> = Vec::new();

                        for line in block.lines() {
                            if let Some(evt) = strip_sse_field(line, "event") {
                                event_type = Some(evt.trim().to_string());
                            } else if let Some(d) = strip_sse_field(line, "data") {
                                data_parts.push(d.to_string());
                            }
                        }

                        if data_parts.is_empty() {
                            continue;
                        }

                        let data_str = data_parts.join("\n");
                        let event_name = event_type.as_deref().unwrap_or("");

                        // 解析 JSON 数据
                        let data: Value = match serde_json::from_str(&data_str) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };

                        log::debug!("[Claude/Responses] <<< SSE event: {event_name}");

                        match event_name {
                            // ================================================
                            // response.created → message_start
                            // ================================================
                            "response.created" => {
                                let response_obj = response_object_from_event(&data);
                                if let Some(id) = response_obj.get("id").and_then(|i| i.as_str()) {
                                    message_id = Some(id.to_string());
                                }
                                if let Some(model) =
                                    response_obj.get("model").and_then(|m| m.as_str())
                                {
                                    current_model = Some(model.to_string());
                                }

                                has_sent_message_start = true;
                                // Build usage with defensive null handling
                                // Some() wrapper ensures build function always receives valid input
                                // Fallback to empty object {} if usage field missing, ensuring message_start
                                // event always has valid usage structure for VSCode Extension compatibility
                                let start_usage = build_anthropic_usage_from_responses(
                                    Some(response_obj.get("usage").unwrap_or(&json!({}))),
                                );

                                let event = json!({
                                    "type": "message_start",
                                    "message": {
                                        "id": message_id.clone().unwrap_or_default(),
                                        "type": "message",
                                        "role": "assistant",
                                        "model": current_model.clone().unwrap_or_default(),
                                        "usage": start_usage
                                    }
                                });
                                let sse = format!("event: message_start\ndata: {}\n\n",
                                    serde_json::to_string(&event).unwrap_or_default());
                                log::debug!("[Claude/Responses] >>> Anthropic SSE: message_start");
                                yield Ok(Bytes::from(sse));
                            }

                            // ================================================
                            // response.content_part.added → content_block_start (text)
                            // ================================================
                            "response.content_part.added" => {
                                // 确保 message_start 已发送
                                if !has_sent_message_start {
                                    let start_event = json!({
                                        "type": "message_start",
                                        "message": {
                                            "id": message_id.clone().unwrap_or_default(),
                                            "type": "message",
                                            "role": "assistant",
                                            "model": current_model.clone().unwrap_or_default(),
                                            "usage": { "input_tokens": 0, "output_tokens": 0 }
                                        }
                                    });
                                    let sse = format!("event: message_start\ndata: {}\n\n",
                                        serde_json::to_string(&start_event).unwrap_or_default());
                                    yield Ok(Bytes::from(sse));
                                    has_sent_message_start = true;
                                }

                                if let Some(part) = data.get("part") {
                                    let part_type = part.get("type").and_then(|t| t.as_str());
                                    if matches!(part_type, Some("output_text") | Some("refusal")) {
                                        has_seen_text_content = true;
                                        if let Some(index) = current_reasoning_index.take() {
                                            if open_indices.remove(&index) {
                                                let stop_event = json!({
                                                    "type": "content_block_stop",
                                                    "index": index
                                                });
                                                let stop_sse = format!("event: content_block_stop\ndata: {}\n\n",
                                                    serde_json::to_string(&stop_event).unwrap_or_default());
                                                yield Ok(Bytes::from(stop_sse));
                                            }
                                            if fallback_open_index == Some(index) {
                                                fallback_open_index = None;
                                            }
                                        }
                                        let index = if let Some(index) = current_text_index {
                                            index
                                        } else {
                                            let index = resolve_content_index(
                                                &data,
                                                &mut next_content_index,
                                                &mut index_by_key,
                                                &mut fallback_open_index,
                                            );
                                            current_text_index = Some(index);
                                            index
                                        };

                                        if open_indices.contains(&index) {
                                            continue;
                                        }

                                        let event = json!({
                                            "type": "content_block_start",
                                            "index": index,
                                            "content_block": {
                                                "type": "text",
                                                "text": ""
                                            }
                                        });
                                        let sse = format!("event: content_block_start\ndata: {}\n\n",
                                            serde_json::to_string(&event).unwrap_or_default());
                                        yield Ok(Bytes::from(sse));
                                        open_indices.insert(index);
                                    }
                                }
                            }

                            // ================================================
                            // response.output_text.delta → content_block_delta (text_delta)
                            // ================================================
                            "response.output_text.delta" => {
                                if let Some(delta) = data.get("delta").and_then(|d| d.as_str()) {
                                    has_seen_text_content = true;
                                    if let Some(index) = current_reasoning_index.take() {
                                        if open_indices.remove(&index) {
                                            let stop_event = json!({
                                                "type": "content_block_stop",
                                                "index": index
                                            });
                                            let stop_sse = format!("event: content_block_stop\ndata: {}\n\n",
                                                serde_json::to_string(&stop_event).unwrap_or_default());
                                            yield Ok(Bytes::from(stop_sse));
                                        }
                                        if fallback_open_index == Some(index) {
                                            fallback_open_index = None;
                                        }
                                    }
                                    let index = if let Some(index) = current_text_index {
                                        index
                                    } else {
                                        let index = resolve_content_index(
                                            &data,
                                            &mut next_content_index,
                                            &mut index_by_key,
                                            &mut fallback_open_index,
                                        );
                                        current_text_index = Some(index);
                                        index
                                    };

                                    if !open_indices.contains(&index) {
                                        let start_event = json!({
                                            "type": "content_block_start",
                                            "index": index,
                                            "content_block": {
                                                "type": "text",
                                                "text": ""
                                            }
                                        });
                                        let start_sse = format!("event: content_block_start\ndata: {}\n\n",
                                            serde_json::to_string(&start_event).unwrap_or_default());
                                        yield Ok(Bytes::from(start_sse));
                                        open_indices.insert(index);
                                    }
                                    let event = json!({
                                        "type": "content_block_delta",
                                        "index": index,
                                        "delta": {
                                            "type": "text_delta",
                                            "text": delta
                                        }
                                    });
                                    let sse = format!("event: content_block_delta\ndata: {}\n\n",
                                        serde_json::to_string(&event).unwrap_or_default());
                                    yield Ok(Bytes::from(sse));
                                }
                            }

                            // ================================================
                            // response.refusal.delta → content_block_delta (text_delta)
                            // ================================================
                            "response.refusal.delta" => {
                                if let Some(delta) = data.get("delta").and_then(|d| d.as_str()) {
                                    has_seen_text_content = true;
                                    if let Some(index) = current_reasoning_index.take() {
                                        if open_indices.remove(&index) {
                                            let stop_event = json!({
                                                "type": "content_block_stop",
                                                "index": index
                                            });
                                            let stop_sse = format!("event: content_block_stop\ndata: {}\n\n",
                                                serde_json::to_string(&stop_event).unwrap_or_default());
                                            yield Ok(Bytes::from(stop_sse));
                                        }
                                        if fallback_open_index == Some(index) {
                                            fallback_open_index = None;
                                        }
                                    }
                                    let index = if let Some(index) = current_text_index {
                                        index
                                    } else {
                                        let index = resolve_content_index(
                                            &data,
                                            &mut next_content_index,
                                            &mut index_by_key,
                                            &mut fallback_open_index,
                                        );
                                        current_text_index = Some(index);
                                        index
                                    };

                                    if !open_indices.contains(&index) {
                                        let start_event = json!({
                                            "type": "content_block_start",
                                            "index": index,
                                            "content_block": {
                                                "type": "text",
                                                "text": ""
                                            }
                                        });
                                        let start_sse = format!("event: content_block_start\ndata: {}\n\n",
                                            serde_json::to_string(&start_event).unwrap_or_default());
                                        yield Ok(Bytes::from(start_sse));
                                        open_indices.insert(index);
                                    }

                                    let event = json!({
                                        "type": "content_block_delta",
                                        "index": index,
                                        "delta": {
                                            "type": "text_delta",
                                            "text": delta
                                        }
                                    });
                                    let sse = format!("event: content_block_delta\ndata: {}\n\n",
                                        serde_json::to_string(&event).unwrap_or_default());
                                    yield Ok(Bytes::from(sse));
                                }
                            }

                            // ================================================
                            // response.content_part.done → content_block_stop
                            // ================================================
                            "response.content_part.done" => {}

                            // ================================================
                            // response.output_item.added (function_call) → content_block_start (tool_use)
                            // ================================================
                            "response.output_item.added" => {
                                if let Some(item) = data.get("item") {
                                    let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                                    if item_type == "function_call" {
                                        has_tool_use = true;
                                        if let Some(index) = current_text_index.take() {
                                            if open_indices.remove(&index) {
                                                let stop_event = json!({
                                                    "type": "content_block_stop",
                                                    "index": index
                                                });
                                                let stop_sse = format!("event: content_block_stop\ndata: {}\n\n",
                                                    serde_json::to_string(&stop_event).unwrap_or_default());
                                                yield Ok(Bytes::from(stop_sse));
                                            }
                                            if fallback_open_index == Some(index) {
                                                fallback_open_index = None;
                                            }
                                        }
                                        // 确保 message_start 已发送
                                        if !has_sent_message_start {
                                            let start_event = json!({
                                                "type": "message_start",
                                                "message": {
                                                    "id": message_id.clone().unwrap_or_default(),
                                                    "type": "message",
                                                    "role": "assistant",
                                                    "model": current_model.clone().unwrap_or_default(),
                                                    "usage": { "input_tokens": 0, "output_tokens": 0 }
                                                }
                                            });
                                            let sse = format!("event: message_start\ndata: {}\n\n",
                                                serde_json::to_string(&start_event).unwrap_or_default());
                                            yield Ok(Bytes::from(sse));
                                            has_sent_message_start = true;
                                        }

                                        let call_id = item.get("call_id").and_then(|i| i.as_str()).unwrap_or("");
                                        let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");

                                        if !has_seen_text_content {
                                            let key = tool_item_key_from_added(&data, item)
                                                .unwrap_or_else(|| format!("tool:pending:{}", pending_tools.len()));
                                            if !pending_tool_by_key.contains_key(&key) {
                                                pending_tool_by_key.insert(key.clone(), pending_tools.len());
                                                pending_tools.push(PendingToolBlock {
                                                    key,
                                                    call_id: call_id.to_string(),
                                                    name: name.to_string(),
                                                    arguments: String::new(),
                                                });
                                            }
                                            continue;
                                        }

                                        let index = if let Some(k) = tool_item_key_from_added(&data, item) {
                                            if let Some(existing) = index_by_key.get(&k).copied() {
                                                existing
                                            } else {
                                                let assigned = next_content_index;
                                                next_content_index += 1;
                                                index_by_key.insert(k, assigned);
                                                assigned
                                            }
                                        } else {
                                            let assigned = next_content_index;
                                            next_content_index += 1;
                                            assigned
                                        };
                                        if let Some(item_id) = item
                                            .get("id")
                                            .and_then(|v| v.as_str())
                                            .or_else(|| data.get("item_id").and_then(|v| v.as_str()))
                                        {
                                            tool_index_by_item_id.insert(item_id.to_string(), index);
                                        }
                                        tool_name_by_index.insert(index, name.to_string());
                                        last_tool_index = Some(index);

                                        if open_indices.contains(&index) {
                                            continue;
                                        }

                                        tool_args_by_index.insert(index, String::new());

                                        let event = json!({
                                            "type": "content_block_start",
                                            "index": index,
                                            "content_block": {
                                                "type": "tool_use",
                                                "id": call_id,
                                                "name": name
                                            }
                                        });
                                        let sse = format!("event: content_block_start\ndata: {}\n\n",
                                            serde_json::to_string(&event).unwrap_or_default());
                                        yield Ok(Bytes::from(sse));
                                        open_indices.insert(index);
                                    }
                                    // message type output_item.added is handled via content_part.added
                                }
                            }

                            // ================================================
                            // response.function_call_arguments.delta → content_block_delta (input_json_delta)
                            // ================================================
                            "response.function_call_arguments.delta" => {
                                if let Some(delta) = data.get("delta").and_then(|d| d.as_str()) {
                                    if !has_seen_text_content {
                                        if let Some(key) = tool_item_key_from_event(&data) {
                                            if let Some(pending_index) = pending_tool_by_key.get(&key).copied() {
                                                if let Some(tool) = pending_tools.get_mut(pending_index) {
                                                    tool.arguments.push_str(delta);
                                                    pending_tool_deltas.push((key, delta.to_string()));
                                                    continue;
                                                }
                                            }
                                        }
                                    }

                                    let item_id = data.get("item_id").and_then(|v| v.as_str());
                                    let index = if let Some(id) = item_id {
                                        tool_index_by_item_id.get(id).copied()
                                    } else {
                                        None
                                    }
                                    .or_else(|| {
                                        tool_item_key_from_event(&data)
                                            .and_then(|k| index_by_key.get(&k).copied())
                                    })
                                    .or(last_tool_index)
                                    .unwrap_or_else(|| {
                                        let assigned = next_content_index;
                                        next_content_index += 1;
                                        assigned
                                    });

                                    if !open_indices.contains(&index) {
                                        let start_event = json!({
                                            "type": "content_block_start",
                                            "index": index,
                                            "content_block": {
                                                "type": "tool_use",
                                                "id": data
                                                    .get("call_id")
                                                    .and_then(|v| v.as_str())
                                                    .or(item_id)
                                                    .unwrap_or(""),
                                                "name": data
                                                    .get("name")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("")
                                            }
                                        });
                                        let start_sse = format!("event: content_block_start\ndata: {}\n\n",
                                            serde_json::to_string(&start_event).unwrap_or_default());
                                        yield Ok(Bytes::from(start_sse));
                                        open_indices.insert(index);
                                    }

                                    if tool_name_by_index.get(&index).map(String::as_str) == Some("Read") {
                                        tool_args_by_index
                                            .entry(index)
                                            .or_default()
                                            .push_str(delta);
                                        continue;
                                    }

                                    let event = json!({
                                        "type": "content_block_delta",
                                        "index": index,
                                        "delta": {
                                            "type": "input_json_delta",
                                            "partial_json": delta
                                        }
                                    });
                                    let sse = format!("event: content_block_delta\ndata: {}\n\n",
                                        serde_json::to_string(&event).unwrap_or_default());
                                    yield Ok(Bytes::from(sse));
                                }
                            }

                            // ================================================
                            // response.function_call_arguments.done → content_block_stop
                            // ================================================
                            "response.function_call_arguments.done" => {
                                if !has_seen_text_content {
                                    if let Some(key) = tool_item_key_from_event(&data) {
                                        if let Some(pending_index) = pending_tool_by_key.get(&key).copied() {
                                            if let Some(tool) = pending_tools.get_mut(pending_index) {
                                                if let Some(arguments) = data.get("arguments").and_then(|v| v.as_str()) {
                                                    tool.arguments = arguments.to_string();
                                                }
                                                continue;
                                            }
                                        }
                                    }
                                }

                                let item_id = data.get("item_id").and_then(|v| v.as_str());
                                let index = if let Some(id) = item_id {
                                    tool_index_by_item_id.get(id).copied()
                                } else {
                                    None
                                }
                                .or_else(|| {
                                    tool_item_key_from_event(&data)
                                        .and_then(|k| index_by_key.get(&k).copied())
                                })
                                .or(last_tool_index);
                                if let Some(index) = index {
                                    if !open_indices.remove(&index) {
                                        continue;
                                    }
                                    if tool_name_by_index.get(&index).map(String::as_str) == Some("Read") {
                                        let raw = data
                                            .get("arguments")
                                            .and_then(|v| v.as_str())
                                            .map(str::to_string)
                                            .unwrap_or_else(|| {
                                                tool_args_by_index
                                                    .get(&index)
                                                    .cloned()
                                                    .unwrap_or_default()
                                            });
                                        let sanitized = sanitize_anthropic_tool_use_input_json("Read", &raw);
                                        if !sanitized.is_empty() {
                                            let event = json!({
                                                "type": "content_block_delta",
                                                "index": index,
                                                "delta": {
                                                    "type": "input_json_delta",
                                                    "partial_json": sanitized
                                                }
                                            });
                                            let sse = format!("event: content_block_delta\ndata: {}\n\n",
                                                serde_json::to_string(&event).unwrap_or_default());
                                            yield Ok(Bytes::from(sse));
                                        }
                                    }
                                    let event = json!({
                                        "type": "content_block_stop",
                                        "index": index
                                    });
                                    let sse = format!("event: content_block_stop\ndata: {}\n\n",
                                        serde_json::to_string(&event).unwrap_or_default());
                                    yield Ok(Bytes::from(sse));
                                    if let Some(item_id) = item_id {
                                        tool_index_by_item_id.remove(item_id);
                                    }
                                    tool_name_by_index.remove(&index);
                                    tool_args_by_index.remove(&index);
                                }
                            }

                            // ================================================
                            // response.refusal.done → content_block_stop
                            // ================================================
                            "response.refusal.done" => {
                                let index = current_text_index.take().or_else(|| {
                                    let key = content_part_key(&data);
                                    if let Some(k) = key {
                                        index_by_key.get(&k).copied()
                                    } else {
                                        fallback_open_index
                                    }
                                });
                                if let Some(index) = index {
                                    if !open_indices.remove(&index) {
                                        continue;
                                    }
                                    let event = json!({
                                        "type": "content_block_stop",
                                        "index": index
                                    });
                                    let sse = format!("event: content_block_stop\ndata: {}\n\n",
                                        serde_json::to_string(&event).unwrap_or_default());
                                    yield Ok(Bytes::from(sse));
                                    if fallback_open_index == Some(index) {
                                        fallback_open_index = None;
                                    }
                                }
                            }

                            // ================================================
                            // response.reasoning_summary_part.added → content_block_start (thinking)
                            // ================================================
                            "response.reasoning_summary_part.added" => {
                                if let Some(part) = data.get("part") {
                                    if part.get("type").and_then(|t| t.as_str()) == Some("summary_text") {
                                        if !has_sent_message_start {
                                            let start_event = json!({
                                                "type": "message_start",
                                                "message": {
                                                    "id": message_id.clone().unwrap_or_default(),
                                                    "type": "message",
                                                    "role": "assistant",
                                                    "model": current_model.clone().unwrap_or_default(),
                                                    "usage": { "input_tokens": 0, "output_tokens": 0 }
                                                }
                                            });
                                            let sse = format!("event: message_start\ndata: {}\n\n",
                                                serde_json::to_string(&start_event).unwrap_or_default());
                                            yield Ok(Bytes::from(sse));
                                            has_sent_message_start = true;
                                        }

                                        if let Some(index) = current_text_index.take() {
                                            if open_indices.remove(&index) {
                                                let stop_event = json!({
                                                    "type": "content_block_stop",
                                                    "index": index
                                                });
                                                let stop_sse = format!("event: content_block_stop\ndata: {}\n\n",
                                                    serde_json::to_string(&stop_event).unwrap_or_default());
                                                yield Ok(Bytes::from(stop_sse));
                                            }
                                            if fallback_open_index == Some(index) {
                                                fallback_open_index = None;
                                            }
                                        }

                                        let index = resolve_reasoning_index(
                                            &data,
                                            &mut next_content_index,
                                            &mut index_by_key,
                                            &mut fallback_open_index,
                                        );

                                        if !open_indices.contains(&index) {
                                            let start_event = json!({
                                                "type": "content_block_start",
                                                "index": index,
                                                "content_block": {
                                                    "type": "thinking",
                                                    "thinking": ""
                                                }
                                            });
                                            let start_sse = format!("event: content_block_start\ndata: {}\n\n",
                                                serde_json::to_string(&start_event).unwrap_or_default());
                                            yield Ok(Bytes::from(start_sse));
                                            open_indices.insert(index);
                                        }

                                        if let Some(text) = reasoning_summary_part_text(&data) {
                                            let event = json!({
                                                "type": "content_block_delta",
                                                "index": index,
                                                "delta": {
                                                    "type": "thinking_delta",
                                                    "thinking": text
                                                }
                                            });
                                            let sse = format!("event: content_block_delta\ndata: {}\n\n",
                                                serde_json::to_string(&event).unwrap_or_default());
                                            yield Ok(Bytes::from(sse));
                                            if let Some(key) = reasoning_summary_key(&data) {
                                                reasoning_emitted_keys.insert(key);
                                            }
                                        }
                                    }
                                }
                            }

                            // ================================================
                            // response.reasoning_summary_text.delta → content_block_delta (thinking_delta)
                            // ================================================
                            "response.reasoning_summary_text.delta" => {
                                if let Some(delta) = reasoning_summary_event_text(&data) {
                                    if !has_sent_message_start {
                                        let start_event = json!({
                                            "type": "message_start",
                                            "message": {
                                                "id": message_id.clone().unwrap_or_default(),
                                                "type": "message",
                                                "role": "assistant",
                                                "model": current_model.clone().unwrap_or_default(),
                                                "usage": { "input_tokens": 0, "output_tokens": 0 }
                                            }
                                        });
                                        let sse = format!("event: message_start\ndata: {}\n\n",
                                            serde_json::to_string(&start_event).unwrap_or_default());
                                        yield Ok(Bytes::from(sse));
                                        has_sent_message_start = true;
                                    }

                                    if let Some(index) = current_text_index.take() {
                                        if open_indices.remove(&index) {
                                            let stop_event = json!({
                                                "type": "content_block_stop",
                                                "index": index
                                            });
                                            let stop_sse = format!("event: content_block_stop\ndata: {}\n\n",
                                                serde_json::to_string(&stop_event).unwrap_or_default());
                                            yield Ok(Bytes::from(stop_sse));
                                        }
                                        if fallback_open_index == Some(index) {
                                            fallback_open_index = None;
                                        }
                                    }

                                    let index = resolve_reasoning_index(
                                        &data,
                                        &mut next_content_index,
                                        &mut index_by_key,
                                        &mut fallback_open_index,
                                    );
                                    current_reasoning_index = Some(index);

                                    if !open_indices.contains(&index) {
                                        let start_event = json!({
                                            "type": "content_block_start",
                                            "index": index,
                                            "content_block": {
                                                "type": "thinking",
                                                "thinking": ""
                                            }
                                        });
                                        let start_sse = format!("event: content_block_start\ndata: {}\n\n",
                                            serde_json::to_string(&start_event).unwrap_or_default());
                                        yield Ok(Bytes::from(start_sse));
                                        open_indices.insert(index);
                                    }

                                    let event = json!({
                                        "type": "content_block_delta",
                                        "index": index,
                                        "delta": {
                                            "type": "thinking_delta",
                                            "thinking": delta
                                        }
                                    });
                                    let sse = format!("event: content_block_delta\ndata: {}\n\n",
                                        serde_json::to_string(&event).unwrap_or_default());
                                    yield Ok(Bytes::from(sse));
                                    if let Some(key) = reasoning_summary_key(&data) {
                                        reasoning_emitted_keys.insert(key);
                                    }
                                }
                            }

                            // ================================================
                            // response.reasoning_summary_text.done / part.done → content_block_stop
                            // ================================================
                            "response.reasoning_summary_text.done" | "response.reasoning_summary_part.done" => {
                                let text = reasoning_summary_event_text(&data)
                                    .or_else(|| reasoning_summary_part_text(&data));
                                let summary_key = reasoning_summary_key(&data);
                                let should_emit = text.is_some()
                                    && summary_key
                                        .as_ref()
                                        .is_none_or(|key| !reasoning_emitted_keys.contains(key));
                                if !should_emit {
                                    continue;
                                }

                                let index = resolve_reasoning_index(
                                    &data,
                                    &mut next_content_index,
                                    &mut index_by_key,
                                    &mut fallback_open_index,
                                );
                                current_reasoning_index = Some(index);
                                if let Some(text) = text {
                                    if !has_sent_message_start {
                                        let start_event = json!({
                                            "type": "message_start",
                                            "message": {
                                                "id": message_id.clone().unwrap_or_default(),
                                                "type": "message",
                                                "role": "assistant",
                                                "model": current_model.clone().unwrap_or_default(),
                                                "usage": { "input_tokens": 0, "output_tokens": 0 }
                                            }
                                        });
                                        let sse = format!("event: message_start\ndata: {}\n\n",
                                            serde_json::to_string(&start_event).unwrap_or_default());
                                        yield Ok(Bytes::from(sse));
                                        has_sent_message_start = true;
                                    }

                                    if !open_indices.contains(&index) {
                                        let start_event = json!({
                                            "type": "content_block_start",
                                            "index": index,
                                            "content_block": {
                                                "type": "thinking",
                                                "thinking": ""
                                            }
                                        });
                                        let start_sse = format!("event: content_block_start\ndata: {}\n\n",
                                            serde_json::to_string(&start_event).unwrap_or_default());
                                        yield Ok(Bytes::from(start_sse));
                                        open_indices.insert(index);
                                    }
                                    let event = json!({
                                        "type": "content_block_delta",
                                        "index": index,
                                        "delta": {
                                            "type": "thinking_delta",
                                            "thinking": text
                                        }
                                    });
                                    let sse = format!("event: content_block_delta\ndata: {}\n\n",
                                        serde_json::to_string(&event).unwrap_or_default());
                                    yield Ok(Bytes::from(sse));
                                    if let Some(key) = summary_key {
                                        reasoning_emitted_keys.insert(key);
                                    }
                                }
                            }

                            // ================================================
                            // response.reasoning.delta → content_block_delta (thinking_delta)
                            // ================================================
                            "response.reasoning.delta" => {
                                if let Some(delta) = data
                                    .get("delta")
                                    .or_else(|| data.get("text"))
                                    .and_then(|d| d.as_str())
                                {
                                    if let Some(index) = current_text_index.take() {
                                        if open_indices.remove(&index) {
                                            let stop_event = json!({
                                                "type": "content_block_stop",
                                                "index": index
                                            });
                                            let stop_sse = format!("event: content_block_stop\ndata: {}\n\n",
                                                serde_json::to_string(&stop_event).unwrap_or_default());
                                            yield Ok(Bytes::from(stop_sse));
                                        }
                                        if fallback_open_index == Some(index) {
                                            fallback_open_index = None;
                                        }
                                    }
                                    let index = resolve_reasoning_index(
                                        &data,
                                        &mut next_content_index,
                                        &mut index_by_key,
                                        &mut fallback_open_index,
                                    );

                                    if !open_indices.contains(&index) {
                                        let start_event = json!({
                                            "type": "content_block_start",
                                            "index": index,
                                            "content_block": {
                                                "type": "thinking",
                                                "thinking": ""
                                            }
                                        });
                                        let start_sse = format!("event: content_block_start\ndata: {}\n\n",
                                            serde_json::to_string(&start_event).unwrap_or_default());
                                        yield Ok(Bytes::from(start_sse));
                                        open_indices.insert(index);
                                    }

                                    let event = json!({
                                        "type": "content_block_delta",
                                        "index": index,
                                        "delta": {
                                            "type": "thinking_delta",
                                            "thinking": delta
                                        }
                                    });
                                    let sse = format!("event: content_block_delta\ndata: {}\n\n",
                                        serde_json::to_string(&event).unwrap_or_default());
                                    yield Ok(Bytes::from(sse));
                                    if let Some(key) = reasoning_summary_key(&data) {
                                        reasoning_emitted_keys.insert(key);
                                    }
                                }
                            }

                            // ================================================
                            // response.reasoning.done → content_block_stop
                            // ================================================
                            "response.reasoning.done" => {
                                let index = reasoning_item_key(&data)
                                    .and_then(|k| index_by_key.get(&k).copied())
                                    .or(current_reasoning_index)
                                    .or(fallback_open_index);
                                if let Some(index) = index {
                                    if !open_indices.remove(&index) {
                                        continue;
                                    }
                                    let event = json!({
                                        "type": "content_block_stop",
                                        "index": index
                                    });
                                    let sse = format!("event: content_block_stop\ndata: {}\n\n",
                                        serde_json::to_string(&event).unwrap_or_default());
                                    yield Ok(Bytes::from(sse));
                                    if fallback_open_index == Some(index) {
                                        fallback_open_index = None;
                                    }
                                }
                            }

                            // ================================================
                            // response.completed → message_delta + message_stop
                            // ================================================
                            "response.completed" => {
                                let response_obj = response_object_from_event(&data);
                                let stop_reason = map_responses_stop_reason(
                                    response_obj.get("status").and_then(|s| s.as_str()),
                                    has_tool_use,
                                    response_obj
                                        .pointer("/incomplete_details/reason")
                                        .and_then(|r| r.as_str()),
                                );

                                // Best effort: close any dangling blocks before message_delta/message_stop.
                                if !open_indices.is_empty() {
                                    let mut remaining: Vec<u32> = open_indices.iter().copied().collect();
                                    remaining.sort_unstable();
                                    for index in remaining {
                                        let stop_event = json!({
                                            "type": "content_block_stop",
                                            "index": index
                                        });
                                        let stop_sse = format!("event: content_block_stop\ndata: {}\n\n",
                                            serde_json::to_string(&stop_event).unwrap_or_default());
                                        yield Ok(Bytes::from(stop_sse));
                                        open_indices.remove(&index);
                                    }
                                }
                                fallback_open_index = None;

                                if !pending_tools.is_empty() {
                                    for event in pending_tool_block_events(&mut pending_tools, &mut pending_tool_deltas, &mut next_content_index) {
                                        yield Ok(event);
                                    }
                                    pending_tool_by_key.clear();
                                }

                                // Defensive: Always build usage_json, even if usage field missing
                                // Some() wrapper with fallback to {} ensures build_anthropic_usage_from_responses
                                // always receives valid input, preventing null pointer errors in VSCode Extension
                                let usage_json = build_anthropic_usage_from_responses(
                                    Some(response_obj.get("usage").unwrap_or(&json!({})))
                                );

                                // Emit message_delta (with usage + stop_reason)
                                let delta_event = json!({
                                    "type": "message_delta",
                                    "delta": {
                                        "stop_reason": stop_reason,
                                        "stop_sequence": null
                                    },
                                    "usage": usage_json
                                });
                                let sse = format!("event: message_delta\ndata: {}\n\n",
                                    serde_json::to_string(&delta_event).unwrap_or_default());
                                log::debug!("[Claude/Responses] >>> Anthropic SSE: message_delta");
                                yield Ok(Bytes::from(sse));

                                // Emit message_stop
                                let stop_event = json!({"type": "message_stop"});
                                let stop_sse = format!("event: message_stop\ndata: {}\n\n",
                                    serde_json::to_string(&stop_event).unwrap_or_default());
                                log::debug!("[Claude/Responses] >>> Anthropic SSE: message_stop");
                                yield Ok(Bytes::from(stop_sse));
                            }

                            // Lifecycle events that don't need Anthropic counterparts.
                            // Listed explicitly so new events trigger a match-completeness review.
                            "response.output_text.done" => {
                                if let Some(index) = current_text_index.take() {
                                    if open_indices.remove(&index) {
                                        let stop_event = json!({
                                            "type": "content_block_stop",
                                            "index": index
                                        });
                                        let stop_sse = format!("event: content_block_stop\ndata: {}\n\n",
                                            serde_json::to_string(&stop_event).unwrap_or_default());
                                        yield Ok(Bytes::from(stop_sse));
                                    }
                                    if fallback_open_index == Some(index) {
                                        fallback_open_index = None;
                                    }
                                }
                                if !pending_tools.is_empty() {
                                    for event in pending_tool_block_events(&mut pending_tools, &mut pending_tool_deltas, &mut next_content_index) {
                                        yield Ok(event);
                                    }
                                    pending_tool_by_key.clear();
                                }
                            }
                            "response.output_item.done" => {
                                let Some(item) = data.get("item") else {
                                    continue;
                                };
                                if item.get("type").and_then(|v| v.as_str()) != Some("reasoning") {
                                    continue;
                                }
                                let summary_parts = reasoning_summary_item_parts(item);
                                if summary_parts.is_empty() {
                                    continue;
                                }
                                if !has_sent_message_start {
                                    let start_event = json!({
                                        "type": "message_start",
                                        "message": {
                                            "id": message_id.clone().unwrap_or_default(),
                                            "type": "message",
                                            "role": "assistant",
                                            "model": current_model.clone().unwrap_or_default(),
                                            "usage": { "input_tokens": 0, "output_tokens": 0 }
                                        }
                                    });
                                    let sse = format!("event: message_start\ndata: {}\n\n",
                                        serde_json::to_string(&start_event).unwrap_or_default());
                                    yield Ok(Bytes::from(sse));
                                    has_sent_message_start = true;
                                }
                                if let Some(text_index) = current_text_index.take() {
                                    if open_indices.remove(&text_index) {
                                        let stop_event = json!({
                                            "type": "content_block_stop",
                                            "index": text_index
                                        });
                                        let stop_sse = format!("event: content_block_stop\ndata: {}\n\n",
                                            serde_json::to_string(&stop_event).unwrap_or_default());
                                        yield Ok(Bytes::from(stop_sse));
                                    }
                                    if fallback_open_index == Some(text_index) {
                                        fallback_open_index = None;
                                    }
                                }

                                for (summary_index, text) in summary_parts {
                                    let mut item_data = data.clone();
                                    if let Some(obj) = item_data.as_object_mut() {
                                        obj.insert("summary_index".to_string(), json!(summary_index));
                                    }
                                    let Some(summary_key) = reasoning_summary_key(&item_data) else {
                                        continue;
                                    };
                                    if reasoning_emitted_keys.contains(&summary_key) {
                                        continue;
                                    }
                                    let index = resolve_reasoning_index(
                                        &item_data,
                                        &mut next_content_index,
                                        &mut index_by_key,
                                        &mut fallback_open_index,
                                    );
                                    current_reasoning_index = Some(index);
                                    if !open_indices.contains(&index) {
                                        let start_event = json!({
                                            "type": "content_block_start",
                                            "index": index,
                                            "content_block": {
                                                "type": "thinking",
                                                "thinking": ""
                                            }
                                        });
                                        let start_sse = format!("event: content_block_start\ndata: {}\n\n",
                                            serde_json::to_string(&start_event).unwrap_or_default());
                                        yield Ok(Bytes::from(start_sse));
                                        open_indices.insert(index);
                                    }
                                    let delta_event = json!({
                                        "type": "content_block_delta",
                                        "index": index,
                                        "delta": {
                                            "type": "thinking_delta",
                                            "thinking": text
                                        }
                                    });
                                    let delta_sse = format!("event: content_block_delta\ndata: {}\n\n",
                                        serde_json::to_string(&delta_event).unwrap_or_default());
                                    yield Ok(Bytes::from(delta_sse));
                                    reasoning_emitted_keys.insert(summary_key);
                                }
                            }
                            "response.in_progress" => {}

                            // Any other unknown/future events — silently skip.
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    log::error!("Responses stream error: {e}");
                    let error_event = json!({
                        "type": "error",
                        "error": {
                            "type": "stream_error",
                            "message": format!("Stream error: {e}")
                        }
                    });
                    let sse = format!("event: error\ndata: {}\n\n",
                        serde_json::to_string(&error_event).unwrap_or_default());
                    yield Ok(Bytes::from(sse));
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use futures::StreamExt;
    use std::collections::HashMap;

    async fn collect_events(input: &str) -> Vec<Value> {
        let upstream = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(
            input.as_bytes().to_vec(),
        ))]);
        let converted = create_anthropic_sse_stream_from_responses(upstream);
        let chunks: Vec<_> = converted.collect().await;

        chunks
            .into_iter()
            .flat_map(|chunk| {
                let bytes = chunk.unwrap();
                let text = String::from_utf8_lossy(bytes.as_ref()).to_string();
                text.split("\n\n")
                    .filter_map(|block| {
                        block.lines().find_map(|line| {
                            strip_sse_field(line, "data")
                                .and_then(|payload| serde_json::from_str::<Value>(payload).ok())
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn test_map_responses_stop_reason_tool_use() {
        assert_eq!(
            map_responses_stop_reason(Some("completed"), true, None),
            Some("tool_use")
        );
        assert_eq!(
            map_responses_stop_reason(Some("completed"), false, None),
            Some("end_turn")
        );
        assert_eq!(
            map_responses_stop_reason(Some("incomplete"), false, Some("max_output_tokens")),
            Some("max_tokens")
        );
        assert_eq!(
            map_responses_stop_reason(Some("incomplete"), false, Some("content_filter")),
            Some("end_turn")
        );
    }

    #[test]
    fn test_response_object_from_event_with_wrapper() {
        let data = json!({
            "type": "response.created",
            "response": {
                "id": "resp_1",
                "model": "gpt-4o"
            }
        });
        let obj = response_object_from_event(&data);
        assert_eq!(obj["id"], "resp_1");
        assert_eq!(obj["model"], "gpt-4o");
    }

    #[tokio::test]
    async fn test_streaming_conversion_with_wrapped_response_events() {
        let input = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-4o\",\"usage\":{\"input_tokens\":12,\"output_tokens\":0}}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"get_weather\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{\\\"city\\\":\\\"Tokyo\\\"}\"}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {\"type\":\"response.function_call_arguments.done\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":12,\"output_tokens\":3}}}\n\n"
        );

        let upstream = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(
            input.as_bytes().to_vec(),
        ))]);
        let converted = create_anthropic_sse_stream_from_responses(upstream);
        let chunks: Vec<_> = converted.collect().await;

        let merged = chunks
            .into_iter()
            .map(|c| String::from_utf8_lossy(c.unwrap().as_ref()).to_string())
            .collect::<String>();

        assert!(merged.contains("\"type\":\"message_start\""));
        assert!(merged.contains("\"id\":\"resp_1\""));
        assert!(merged.contains("\"model\":\"gpt-4o\""));
        assert!(merged.contains("\"type\":\"tool_use\""));
        assert!(merged.contains("\"name\":\"get_weather\""));
        assert!(merged.contains("\"type\":\"input_json_delta\""));
        assert!(merged.contains("\"stop_reason\":\"tool_use\""));
        assert!(merged.contains("\"input_tokens\":12"));
        assert!(merged.contains("\"output_tokens\":3"));
        assert!(merged.contains("\"type\":\"message_stop\""));
    }

    #[tokio::test]
    async fn test_streaming_read_tool_drops_empty_pages() {
        let input = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_read\",\"model\":\"gpt-5.5\"}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_read\",\"type\":\"function_call\",\"call_id\":\"call_read\",\"name\":\"Read\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_read\",\"delta\":\"{\\\"file_path\\\":\\\"/tmp/demo.py\\\",\\\"limit\\\":2000,\\\"offset\\\":0,\\\"pages\\\":\\\"\\\"}\"}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_read\",\"arguments\":\"{\\\"file_path\\\":\\\"/tmp/demo.py\\\",\\\"limit\\\":2000,\\\"offset\\\":0,\\\"pages\\\":\\\"\\\"}\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
        );

        let upstream = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(
            input.as_bytes().to_vec(),
        ))]);
        let converted = create_anthropic_sse_stream_from_responses(upstream);
        let chunks: Vec<_> = converted.collect().await;

        let merged = chunks
            .into_iter()
            .map(|c| String::from_utf8_lossy(c.unwrap().as_ref()).to_string())
            .collect::<String>();

        assert!(merged.contains("\"name\":\"Read\""));
        assert!(merged.contains("\"partial_json\":\"{\\\"file_path\\\":\\\"/tmp/demo.py\\\",\\\"limit\\\":2000,\\\"offset\\\":0}"));
        assert!(!merged.contains("\\\"pages\\\":\\\"\\\""));
    }

    #[tokio::test]
    async fn test_streaming_read_tool_duplicate_start_preserves_buffered_args() {
        let input = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_read\",\"model\":\"gpt-5.5\"}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_read\",\"type\":\"function_call\",\"call_id\":\"call_read\",\"name\":\"Read\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_read\",\"delta\":\"{\\\"file_path\\\":\\\"/tmp/demo.py\\\",\\\"limit\\\":2000,\\\"offset\\\":0,\\\"pages\\\":\\\"\\\"}\"}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_read\",\"type\":\"function_call\",\"call_id\":\"call_read\",\"name\":\"Read\"}}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_read\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
        );

        let upstream = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(
            input.as_bytes().to_vec(),
        ))]);
        let converted = create_anthropic_sse_stream_from_responses(upstream);
        let chunks: Vec<_> = converted.collect().await;

        let merged = chunks
            .into_iter()
            .map(|c| String::from_utf8_lossy(c.unwrap().as_ref()).to_string())
            .collect::<String>();

        assert_eq!(merged.matches("event: content_block_start").count(), 1);
        assert_eq!(merged.matches("event: content_block_stop").count(), 1);
        assert!(merged.contains("\"partial_json\":\"{\\\"file_path\\\":\\\"/tmp/demo.py\\\",\\\"limit\\\":2000,\\\"offset\\\":0}"));
        assert!(!merged.contains("\\\"pages\\\":\\\"\\\""));
    }

    #[tokio::test]
    async fn test_streaming_conversion_interleaved_tool_deltas_by_item_id() {
        let input = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_2\",\"model\":\"gpt-4o\"}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"first_tool\"}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"fc_2\",\"type\":\"function_call\",\"call_id\":\"call_2\",\"name\":\"second_tool\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_2\",\"delta\":\"{\\\"b\\\":2}\"}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"a\\\":1}\"}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_1\"}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_2\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":8,\"output_tokens\":4}}}\n\n"
        );

        let upstream = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(
            input.as_bytes().to_vec(),
        ))]);
        let converted = create_anthropic_sse_stream_from_responses(upstream);
        let chunks: Vec<_> = converted.collect().await;
        let merged = chunks
            .into_iter()
            .map(|c| String::from_utf8_lossy(c.unwrap().as_ref()).to_string())
            .collect::<String>();

        let events: Vec<Value> = merged
            .split("\n\n")
            .filter_map(|block| {
                let data = block
                    .lines()
                    .find_map(|line| strip_sse_field(line, "data"))?;
                serde_json::from_str::<Value>(data).ok()
            })
            .collect();

        let mut tool_index_by_call: HashMap<String, u64> = HashMap::new();
        for event in &events {
            if event.get("type").and_then(|v| v.as_str()) == Some("content_block_start") {
                let cb = event.get("content_block");
                if cb.and_then(|v| v.get("type")).and_then(|v| v.as_str()) == Some("tool_use") {
                    if let (Some(call_id), Some(index)) = (
                        cb.and_then(|v| v.get("id")).and_then(|v| v.as_str()),
                        event.get("index").and_then(|v| v.as_u64()),
                    ) {
                        tool_index_by_call.insert(call_id.to_string(), index);
                    }
                }
            }
        }

        let delta_indices: Vec<u64> = events
            .iter()
            .filter(|event| {
                event.get("type").and_then(|v| v.as_str()) == Some("content_block_delta")
                    && event.pointer("/delta/type").and_then(|v| v.as_str())
                        == Some("input_json_delta")
            })
            .filter_map(|event| event.get("index").and_then(|v| v.as_u64()))
            .collect();

        assert_eq!(delta_indices.len(), 2);
        assert_eq!(delta_indices[0], *tool_index_by_call.get("call_2").unwrap());
        assert_eq!(delta_indices[1], *tool_index_by_call.get("call_1").unwrap());
        assert_ne!(
            tool_index_by_call.get("call_1"),
            tool_index_by_call.get("call_2")
        );
    }

    #[tokio::test]
    async fn test_streaming_reasoning_summary_parts_are_merged_into_one_thinking_block() {
        let input = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_summary_merge\",\"model\":\"gpt-5.5\"}}\n\n",
            "event: response.reasoning_summary_text.delta\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_merge\",\"output_index\":0,\"summary_index\":0,\"delta\":\"First thought.\"}\n\n",
            "event: response.reasoning_summary_text.done\n",
            "data: {\"type\":\"response.reasoning_summary_text.done\",\"item_id\":\"rs_merge\",\"output_index\":0,\"summary_index\":0,\"text\":\"First thought.\"}\n\n",
            "event: response.reasoning_summary_text.delta\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_merge\",\"output_index\":0,\"summary_index\":1,\"delta\":\"Second thought.\"}\n\n",
            "event: response.reasoning_summary_text.done\n",
            "data: {\"type\":\"response.reasoning_summary_text.done\",\"item_id\":\"rs_merge\",\"output_index\":0,\"summary_index\":1,\"text\":\"Second thought.\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_merge\",\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"First thought.\"},{\"type\":\"summary_text\",\"text\":\"Second thought.\"}]}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
        );

        let events = collect_events(input).await;
        let thinking_starts: Vec<_> = events
            .iter()
            .filter(|event| {
                event.pointer("/content_block/type").and_then(Value::as_str) == Some("thinking")
            })
            .collect();
        let thinking_indices: Vec<_> = events
            .iter()
            .filter_map(|event| {
                if event.pointer("/delta/type").and_then(Value::as_str) == Some("thinking_delta") {
                    event.get("index").and_then(Value::as_u64)
                } else {
                    None
                }
            })
            .collect();
        let thinking_text: Vec<_> = events
            .iter()
            .filter_map(|event| event.pointer("/delta/thinking").and_then(Value::as_str))
            .collect();

        assert_eq!(thinking_starts.len(), 1);
        assert_eq!(thinking_indices, vec![0, 0]);
        assert_eq!(thinking_text, vec!["First thought.", "Second thought."]);
    }

    #[tokio::test]
    async fn test_streaming_tool_call_is_emitted_after_late_text() {
        let input = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_tool_text\",\"model\":\"gpt-5.5\"}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"fc_first\",\"type\":\"function_call\",\"call_id\":\"call_first\",\"name\":\"Read\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_first\",\"delta\":\"{\\\"file_path\\\":\\\"/tmp/demo.py\\\"}\"}\n\n",
            "event: response.function_call_arguments.done\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_first\"}\n\n",
            "event: response.content_part.added\n",
            "data: {\"type\":\"response.content_part.added\",\"part\":{\"type\":\"output_text\",\"text\":\"\"},\"output_index\":1,\"content_index\":0}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"I need to inspect the file.\",\"output_index\":1,\"content_index\":0}\n\n",
            "event: response.output_text.done\n",
            "data: {\"type\":\"response.output_text.done\",\"output_index\":1,\"content_index\":0}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
        );

        let events = collect_events(input).await;
        let text_start = events
            .iter()
            .position(|event| {
                event.pointer("/content_block/type").and_then(Value::as_str) == Some("text")
            })
            .unwrap();
        let tool_start = events
            .iter()
            .position(|event| {
                event.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use")
            })
            .unwrap();

        assert!(text_start < tool_start);
        assert_eq!(
            events[tool_start]
                .pointer("/content_block/name")
                .and_then(Value::as_str),
            Some("Read")
        );
        assert!(events.iter().any(|event| {
            event.pointer("/delta/type").and_then(Value::as_str) == Some("input_json_delta")
                && event.pointer("/delta/partial_json").and_then(Value::as_str)
                    == Some("{\"file_path\":\"/tmp/demo.py\"}")
        }));
    }

    #[tokio::test]
    async fn test_streaming_reasoning_summary_events_emit_thinking_blocks() {
        let input = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_summary\",\"model\":\"gpt-5.5\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"rs_resp_summary\",\"type\":\"reasoning\",\"status\":\"in_progress\",\"summary\":[]}}\n\n",
            "event: response.reasoning_summary_part.added\n",
            "data: {\"type\":\"response.reasoning_summary_part.added\",\"item_id\":\"rs_resp_summary\",\"output_index\":0,\"summary_index\":0,\"part\":{\"type\":\"summary_text\",\"text\":\"\"}}\n\n",
            "event: response.reasoning_summary_text.delta\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_resp_summary\",\"output_index\":0,\"summary_index\":0,\"delta\":\"Need to inspect context.\"}\n\n",
            "event: response.reasoning_summary_text.done\n",
            "data: {\"type\":\"response.reasoning_summary_text.done\",\"item_id\":\"rs_resp_summary\",\"output_index\":0,\"summary_index\":0,\"text\":\"Need to inspect context.\"}\n\n",
            "event: response.reasoning_summary_part.done\n",
            "data: {\"type\":\"response.reasoning_summary_part.done\",\"item_id\":\"rs_resp_summary\",\"output_index\":0,\"summary_index\":0,\"part\":{\"type\":\"summary_text\",\"text\":\"Need to inspect context.\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_resp_summary\",\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"Need to inspect context.\"}]}}\n\n",
            "event: response.content_part.added\n",
            "data: {\"type\":\"response.content_part.added\",\"part\":{\"type\":\"output_text\",\"text\":\"\"},\"output_index\":1,\"content_index\":0}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Done\",\"output_index\":1,\"content_index\":0}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":10}}}\n\n"
        );

        let upstream = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(
            input.as_bytes().to_vec(),
        ))]);
        let converted = create_anthropic_sse_stream_from_responses(upstream);
        let chunks: Vec<_> = converted.collect().await;
        let merged = chunks
            .into_iter()
            .map(|c| String::from_utf8_lossy(c.unwrap().as_ref()).to_string())
            .collect::<String>();

        assert!(
            merged.contains("\"type\":\"thinking\""),
            "should emit thinking content_block_start for reasoning summary events"
        );
        assert!(
            merged.contains("\"type\":\"thinking_delta\""),
            "should emit thinking_delta for reasoning summary text"
        );
        assert!(
            merged.contains("\"thinking\":\"Need to inspect context.\""),
            "should contain reasoning summary text as thinking"
        );
        assert!(merged.contains("\"text\":\"Done\""));
    }

    #[tokio::test]
    async fn test_streaming_reasoning_done_with_item_id_closes_thinking_before_text() {
        let input = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_r_done\",\"model\":\"o3\"}}\n\n",
            "event: response.reasoning.delta\n",
            "data: {\"type\":\"response.reasoning.delta\",\"item_id\":\"rs_1\",\"output_index\":0,\"delta\":\"Think first.\"}\n\n",
            "event: response.reasoning.done\n",
            "data: {\"type\":\"response.reasoning.done\",\"item_id\":\"rs_1\",\"output_index\":0}\n\n",
            "event: response.content_part.added\n",
            "data: {\"type\":\"response.content_part.added\",\"part\":{\"type\":\"output_text\",\"text\":\"\"},\"output_index\":1,\"content_index\":0}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Answer\",\"output_index\":1,\"content_index\":0}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
        );

        let events = collect_events(input).await;
        let thinking_start = events
            .iter()
            .position(|event| {
                event.pointer("/content_block/type").and_then(Value::as_str) == Some("thinking")
            })
            .unwrap();
        let first_stop_after_thinking = events[thinking_start + 1..]
            .iter()
            .position(|event| {
                event.get("type").and_then(Value::as_str) == Some("content_block_stop")
            })
            .map(|offset| thinking_start + 1 + offset)
            .unwrap();
        let text_start = events
            .iter()
            .position(|event| {
                event.pointer("/content_block/type").and_then(Value::as_str) == Some("text")
            })
            .unwrap();

        assert!(
            first_stop_after_thinking < text_start,
            "reasoning.done must close thinking before text starts"
        );
    }

    #[tokio::test]
    async fn test_streaming_reasoning_summary_done_only_emits_thinking() {
        let input = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_done_only\",\"model\":\"gpt-5.5\"}}\n\n",
            "event: response.reasoning_summary_text.done\n",
            "data: {\"type\":\"response.reasoning_summary_text.done\",\"item_id\":\"rs_done\",\"output_index\":0,\"summary_index\":0,\"text\":\"Done-only thinking.\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
        );

        let events = collect_events(input).await;
        assert!(events.iter().any(|event| event
            .pointer("/content_block/type")
            .and_then(Value::as_str)
            == Some("thinking")));
        assert!(events.iter().any(|event| event
            .pointer("/delta/thinking")
            .and_then(Value::as_str)
            == Some("Done-only thinking.")));
    }

    #[tokio::test]
    async fn test_streaming_reasoning_output_item_done_reuses_open_summary_block() {
        let input = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_reuse\",\"model\":\"gpt-5.5\"}}\n\n",
            "event: response.reasoning_summary_part.added\n",
            "data: {\"type\":\"response.reasoning_summary_part.added\",\"item_id\":\"rs_reuse\",\"output_index\":0,\"summary_index\":0,\"part\":{\"type\":\"summary_text\",\"text\":\"\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_reuse\",\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"Final thinking.\"}]}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
        );

        let events = collect_events(input).await;
        let thinking_starts = events
            .iter()
            .filter(|event| {
                event.pointer("/content_block/type").and_then(Value::as_str) == Some("thinking")
            })
            .count();
        let thinking_text = events
            .iter()
            .filter_map(|event| event.pointer("/delta/thinking").and_then(Value::as_str))
            .collect::<Vec<_>>();
        let stops = events
            .iter()
            .filter(|event| event.get("type").and_then(Value::as_str) == Some("content_block_stop"))
            .count();

        assert_eq!(thinking_starts, 1);
        assert_eq!(thinking_text, vec!["Final thinking."]);
        assert_eq!(stops, 1);
    }

    #[tokio::test]
    async fn test_streaming_reasoning_output_item_done_keeps_unstreamed_later_summary_parts() {
        let input = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_multi_summary\",\"model\":\"gpt-5.5\"}}\n\n",
            "event: response.reasoning_summary_text.delta\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"item_id\":\"rs_multi\",\"output_index\":0,\"summary_index\":0,\"delta\":\"First summary.\"}\n\n",
            "event: response.reasoning_summary_text.done\n",
            "data: {\"type\":\"response.reasoning_summary_text.done\",\"item_id\":\"rs_multi\",\"output_index\":0,\"summary_index\":0,\"text\":\"First summary.\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_multi\",\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"First summary.\"},{\"type\":\"summary_text\",\"text\":\"Second summary.\"}]}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
        );

        let events = collect_events(input).await;
        let thinking_text = events
            .iter()
            .filter_map(|event| event.pointer("/delta/thinking").and_then(Value::as_str))
            .collect::<Vec<_>>();

        assert!(thinking_text.contains(&"First summary."));
        assert!(thinking_text.contains(&"Second summary."));
    }

    #[tokio::test]
    async fn test_streaming_reasoning_delta_emits_thinking_blocks() {
        let input = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_r\",\"model\":\"o3\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            "event: response.reasoning.delta\n",
            "data: {\"type\":\"response.reasoning.delta\",\"delta\":\"Let me think...\"}\n\n",
            "event: response.reasoning.done\n",
            "data: {\"type\":\"response.reasoning.done\"}\n\n",
            "event: response.content_part.added\n",
            "data: {\"type\":\"response.content_part.added\",\"part\":{\"type\":\"output_text\",\"text\":\"\"},\"output_index\":0,\"content_index\":0}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"42\",\"output_index\":0,\"content_index\":0}\n\n",
            "event: response.content_part.done\n",
            "data: {\"type\":\"response.content_part.done\",\"output_index\":0,\"content_index\":0}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":10}}}\n\n"
        );

        let upstream = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(
            input.as_bytes().to_vec(),
        ))]);
        let converted = create_anthropic_sse_stream_from_responses(upstream);
        let chunks: Vec<_> = converted.collect().await;
        let merged = chunks
            .into_iter()
            .map(|c| String::from_utf8_lossy(c.unwrap().as_ref()).to_string())
            .collect::<String>();

        // Should contain thinking block start, thinking delta, and text content
        assert!(
            merged.contains("\"type\":\"thinking\""),
            "should emit thinking content_block_start"
        );
        assert!(
            merged.contains("\"type\":\"thinking_delta\""),
            "should emit thinking_delta"
        );
        assert!(
            merged.contains("\"thinking\":\"Let me think...\""),
            "should contain thinking text"
        );
        assert!(
            merged.contains("\"type\":\"text_delta\""),
            "should also emit text content"
        );
        assert!(
            merged.contains("\"text\":\"42\""),
            "should contain text delta"
        );
        assert!(merged.contains("\"stop_reason\":\"end_turn\""));
    }

    #[tokio::test]
    async fn test_streaming_text_parts_are_merged_into_one_text_block() {
        let input = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_merge\",\"model\":\"gpt-5.4\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            "event: response.content_part.added\n",
            "data: {\"type\":\"response.content_part.added\",\"part\":{\"type\":\"output_text\",\"text\":\"\"},\"output_index\":0,\"content_index\":0}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"你\",\"output_index\":0,\"content_index\":0}\n\n",
            "event: response.content_part.done\n",
            "data: {\"type\":\"response.content_part.done\",\"output_index\":0,\"content_index\":0}\n\n",
            "event: response.content_part.added\n",
            "data: {\"type\":\"response.content_part.added\",\"part\":{\"type\":\"output_text\",\"text\":\"\"},\"output_index\":0,\"content_index\":1}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"好\",\"output_index\":0,\"content_index\":1}\n\n",
            "event: response.content_part.done\n",
            "data: {\"type\":\"response.content_part.done\",\"output_index\":0,\"content_index\":1}\n\n",
            "event: response.output_text.done\n",
            "data: {\"type\":\"response.output_text.done\",\"output_index\":0,\"content_index\":1}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n\n"
        );

        let upstream = stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from(
            input.as_bytes().to_vec(),
        ))]);
        let converted = create_anthropic_sse_stream_from_responses(upstream);
        let chunks: Vec<_> = converted.collect().await;
        let events: Vec<Value> = chunks
            .into_iter()
            .flat_map(|chunk| {
                let bytes = chunk.unwrap();
                let text = String::from_utf8_lossy(bytes.as_ref()).to_string();
                text.split("\n\n")
                    .filter_map(|block| {
                        block.lines().find_map(|line| {
                            strip_sse_field(line, "data")
                                .and_then(|payload| serde_json::from_str::<Value>(payload).ok())
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        let text_starts = events
            .iter()
            .filter(|event| {
                event.get("type").and_then(|v| v.as_str()) == Some("content_block_start")
                    && event
                        .pointer("/content_block/type")
                        .and_then(|v| v.as_str())
                        == Some("text")
            })
            .count();
        let text_stops = events
            .iter()
            .filter(|event| {
                event.get("type").and_then(|v| v.as_str()) == Some("content_block_stop")
            })
            .count();
        let text_deltas: Vec<String> = events
            .iter()
            .filter(|event| {
                event.get("type").and_then(|v| v.as_str()) == Some("content_block_delta")
                    && event.pointer("/delta/type").and_then(|v| v.as_str()) == Some("text_delta")
            })
            .filter_map(|event| {
                event
                    .pointer("/delta/text")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string)
            })
            .collect();

        assert_eq!(text_starts, 1);
        assert_eq!(text_stops, 1);
        assert_eq!(text_deltas, vec!["你".to_string(), "好".to_string()]);
    }

    #[tokio::test]
    async fn test_streaming_responses_chinese_split_across_chunks_no_replacement_chars() {
        // Chinese text delta split across two TCP chunks.
        let full = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_cn\",\"model\":\"gpt-4o\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"你好世界\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":4}}}\n\n"
        );
        let bytes = full.as_bytes();

        // Find "你" and split inside it
        let ni_start = bytes.windows(3).position(|w| w == "你".as_bytes()).unwrap();
        let split_point = ni_start + 2; // split after second byte of "你"

        let chunk1 = Bytes::from(bytes[..split_point].to_vec());
        let chunk2 = Bytes::from(bytes[split_point..].to_vec());

        let upstream = stream::iter(vec![
            Ok::<_, std::io::Error>(chunk1),
            Ok::<_, std::io::Error>(chunk2),
        ]);
        let converted = create_anthropic_sse_stream_from_responses(upstream);
        let chunks: Vec<_> = converted.collect().await;
        let merged = chunks
            .into_iter()
            .map(|c| String::from_utf8_lossy(c.unwrap().as_ref()).to_string())
            .collect::<String>();

        assert!(
            merged.contains("你好世界"),
            "expected '你好世界' in output, got replacement chars (U+FFFD)"
        );
        assert!(
            !merged.contains('\u{FFFD}'),
            "output must not contain U+FFFD replacement characters"
        );
    }
}
