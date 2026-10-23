//! Nexus Mesh (去中心化共享网络) 通道后端与门禁。

use crate::channel::{Capability, Channel, ChannelGate, MESH};
use crate::error::UpstreamError;
use crate::lane::{BoxFuture, Credential, Lane};
use crate::normalized::{ChatRequest, Completion, Delta};
use crate::upstream::{DeltaSink, Upstream};
use nexus_mesh::protocol::TunnelFrame;
use nexus_mesh::MeshService;
use std::sync::Arc;

pub const ROUTE_PREFIXES: &[&str] = &["mesh/", "p2p/"];

pub const DEFAULT_MESH_MODELS: &[&str] = &["auto/smart"];

pub struct MeshGate {
    service: Arc<MeshService>,
}

impl MeshGate {
    pub fn new(service: Arc<MeshService>) -> Self {
        Self { service }
    }
}

impl ChannelGate for MeshGate {
    fn ready(&self) -> bool {
        true
    }

    fn owns(&self, cap: Capability, base_model: &str) -> bool {
        if cap != Capability::Chat {
            return false;
        }
        let m = base_model.trim().to_ascii_lowercase();
        let stripped = m.strip_prefix("mesh/").unwrap_or(&m);
        if stripped == "auto/smart" {
            return true;
        }
        for peer in self.service.blocking_peers() {
            let prefix = format!("{}/", peer.device_name.trim().to_ascii_lowercase());
            if stripped.starts_with(&prefix) {
                return true;
            }
            let id_prefix = format!("{}/", peer.node_id.trim().to_ascii_lowercase());
            if stripped.starts_with(&id_prefix) {
                return true;
            }
            for rm in &peer.remote_models {
                let rm_clean = rm.trim().to_ascii_lowercase();
                if stripped == rm_clean || stripped.ends_with(&format!("/{rm_clean}")) {
                    return true;
                }
            }
        }
        false
    }

    fn claims_bare(&self, base_model: &str) -> bool {
        let m = base_model.trim().to_ascii_lowercase();
        let stripped = m.strip_prefix("mesh/").unwrap_or(&m);
        for peer in self.service.blocking_peers() {
            let prefix = format!("{}/", peer.device_name.trim().to_ascii_lowercase());
            if stripped.starts_with(&prefix) {
                return true;
            }
            let id_prefix = format!("{}/", peer.node_id.trim().to_ascii_lowercase());
            if stripped.starts_with(&id_prefix) {
                return true;
            }
        }
        false
    }

    fn models(&self, cap: Capability) -> Vec<String> {
        if cap != Capability::Chat {
            return Vec::new();
        }
        let peers = self.service.blocking_peers();
        let mut list: Vec<String> = Vec::new();
        for peer in &peers {
            let peer_clean = peer.device_name.trim().to_ascii_lowercase();
            for rm in &peer.remote_models {
                let scoped = if rm.to_ascii_lowercase().starts_with(&format!("{peer_clean}/")) {
                    rm.clone()
                } else {
                    format!("{}/{}", peer_clean, rm)
                };
                if !list.contains(&scoped) {
                    list.push(scoped);
                }
            }
        }
        if list.is_empty() {
            list.push("auto/smart".to_string());
        }
        list
    }
}

pub fn serialize_chat_request_to_mesh_payload(request: &ChatRequest, model: &str) -> serde_json::Value {
    let mut messages_json = Vec::new();
    for m in &request.messages {
        match m.role {
            crate::normalized::Role::System => {
                messages_json.push(serde_json::json!({
                    "role": "system",
                    "content": m.text,
                }));
            }
            crate::normalized::Role::User => {
                messages_json.push(serde_json::json!({
                    "role": "user",
                    "content": m.text,
                }));
            }
            crate::normalized::Role::Assistant => {
                let mut asst = serde_json::json!({
                    "role": "assistant",
                    "content": m.text,
                });
                if !m.tool_calls.is_empty() {
                    asst["tool_calls"] = serde_json::json!(m.tool_calls.iter().map(|tc| {
                        serde_json::json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": tc.arguments,
                            }
                        })
                    }).collect::<Vec<_>>());
                }
                messages_json.push(asst);
            }
            crate::normalized::Role::Tool => {
                if !m.tool_results.is_empty() {
                    for r in &m.tool_results {
                        messages_json.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": r.tool_call_id,
                            "name": r.tool_name,
                            "content": r.text,
                        }));
                    }
                } else {
                    messages_json.push(serde_json::json!({
                        "role": "tool",
                        "content": m.text,
                    }));
                }
            }
        }
    }

    let mut body = serde_json::json!({
        "model": model,
        "messages": messages_json,
        "stream": true,
    });

    if !request.tools.is_empty() {
        body["tools"] = serde_json::json!(request.tools.iter().map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        }).collect::<Vec<_>>());
    }

    body
}

pub struct MeshUpstream {
    service: Arc<MeshService>,
}

impl MeshUpstream {
    pub fn new(service: Arc<MeshService>) -> Self {
        Self { service }
    }
}

impl Upstream for MeshUpstream {
    fn stream<'a>(
        &'a self,
        _credential: &'a Credential,
        request: &'a ChatRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
        Box::pin(async move {
            self.service.record_local_activity();

            let raw_m = request.model.trim();
            let stripped = raw_m.strip_prefix("mesh/").unwrap_or(raw_m);
            let m_lower = stripped.to_ascii_lowercase();

            let peers = self.service.blocking_peers();
            let mut target_peer: Option<nexus_mesh::MeshPeer> = None;
            let mut sub_model = stripped.to_string();

            for peer in &peers {
                let dev_prefix = format!("{}/", peer.device_name.trim().to_ascii_lowercase());
                let node_prefix = format!("{}/", peer.node_id.trim().to_ascii_lowercase());
                if m_lower.starts_with(&dev_prefix) {
                    sub_model = stripped[dev_prefix.len()..].to_string();
                    target_peer = Some(peer.clone());
                    break;
                } else if m_lower.starts_with(&node_prefix) {
                    sub_model = stripped[node_prefix.len()..].to_string();
                    target_peer = Some(peer.clone());
                    break;
                }
            }

            let is_self = target_peer.as_ref().map(|p| p.is_self).unwrap_or(false);
            let is_preempting = !is_self && self.service.is_preempted();

            let preemption_notice = if is_preempting {
                "⚡ [提示] 节点当前处于本地所有者使用高峰期，外部共享请求正处于排队让路保护中。\n\n"
            } else {
                ""
            };
            if !preemption_notice.is_empty() {
                on_delta(Delta::Text(preemption_notice.to_string()));
            }

            // 1. 如果匹配到本机共享回环节点 (is_self)，直接在本地调用 Invoke Hook 执行，免去云端跳数与离线隐患
            if let Some(ref peer) = target_peer {
                if peer.is_self {
                    if let Some(hook) = self.service.get_invoke_hook() {
                        let m_sub_lower = sub_model.to_ascii_lowercase();
                        let channel_id: String = if peer.shared_channels.len() == 1 {
                            peer.shared_channels[0].clone()
                        } else if m_sub_lower.contains("kiro") {
                            "kiro".to_string()
                        } else if m_sub_lower.contains("claude") {
                            "claude".to_string()
                        } else if m_sub_lower.contains("gpt")
                            || m_sub_lower.starts_with("o1")
                            || m_sub_lower.starts_with("o3")
                            || m_sub_lower.starts_with("o4")
                        {
                            "chatgpt".to_string()
                        } else if m_sub_lower.contains("gemini") {
                            "antigravity".to_string()
                        } else if m_sub_lower.contains("grok") {
                            "grok".to_string()
                        } else if m_sub_lower.contains("glm") {
                            "zcode".to_string()
                        } else if m_sub_lower.contains("qwen") || m_sub_lower.contains("deepseek") {
                            "qoder".to_string()
                        } else {
                            peer.shared_channels.first().cloned().unwrap_or_else(|| "kiro".to_string())
                        };

                        let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel(1024);
                        let stream_id = 1;

                        let payload_str = if let Some(raw) = &request.raw_inbound {
                            let mut v = (*raw.body).clone();
                            if let Some(obj) = v.as_object_mut() {
                                obj.insert("model".to_string(), serde_json::Value::String(sub_model.clone()));
                            }
                            v.to_string()
                        } else {
                            serialize_chat_request_to_mesh_payload(request, &sub_model).to_string()
                        };

                        let hook_task = tokio::spawn(async move {
                            hook(
                                channel_id.to_string(),
                                sub_model.clone(),
                                payload_str,
                                frame_tx,
                                stream_id,
                            )
                            .await
                        });

                        let mut full_text = String::new();
                        let mut tool_calls = Vec::new();
                        let mut finish_reason = crate::normalized::FinishReason::Stop;
                        let mut thinking_signature: Option<String> = None;
                        let mut output_tokens = 150u32;
                        if !preemption_notice.is_empty() {
                            full_text.push_str(preemption_notice);
                        }

                        while let Some(frame) = frame_rx.recv().await {
                            match frame {
                                TunnelFrame::Delta { text, is_thinking, .. } => {
                                    if is_thinking {
                                        on_delta(Delta::Thinking(text.clone()));
                                    } else {
                                        on_delta(Delta::Text(text.clone()));
                                        full_text.push_str(&text);
                                    }
                                }
                                TunnelFrame::Finish { total_tokens, finish_reason: fr, tool_calls: tc, thinking_signature: ts, .. } => {
                                    if let Some(t) = total_tokens {
                                        output_tokens = t;
                                    }
                                    if let Some(ref sig) = ts {
                                        on_delta(Delta::ThinkingSignature(sig.clone()));
                                        thinking_signature = Some(sig.clone());
                                    }
                                    if let Some(tcs) = tc {
                                        tool_calls = tcs.into_iter().map(|w| crate::normalized::ToolCall {
                                            id: w.id,
                                            name: w.name,
                                            arguments: w.arguments,
                                        }).collect();
                                    }
                                    if !tool_calls.is_empty() {
                                        finish_reason = crate::normalized::FinishReason::ToolCalls;
                                    } else if let Some(r) = fr {
                                        finish_reason = match r.as_str() {
                                            "tool_calls" | "tool_use" => crate::normalized::FinishReason::ToolCalls,
                                            "length" | "max_tokens" => crate::normalized::FinishReason::Length,
                                            "content_filter" => crate::normalized::FinishReason::ContentFilter,
                                            _ => crate::normalized::FinishReason::Stop,
                                        };
                                    }
                                    break;
                                }
                                TunnelFrame::Error { message, .. } => {
                                    return Err(UpstreamError::new(
                                        crate::error::UpstreamKind::Upstream,
                                        500,
                                        format!("Mesh 本机回环调用错误: {message}"),
                                    ));
                                }
                                _ => {}
                            }
                        }

                        match hook_task.await {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => {
                                return Err(UpstreamError::new(
                                    crate::error::UpstreamKind::Upstream,
                                    500,
                                    format!("Mesh 本机回环调用错误: {e}"),
                                ));
                            }
                            Err(e) => {
                                return Err(UpstreamError::new(
                                    crate::error::UpstreamKind::Upstream,
                                    500,
                                    format!("Mesh 本机回环任务异常: {e}"),
                                ));
                            }
                        }

                        return Ok(Completion {
                            text: full_text,
                            thinking: String::new(),
                            tool_calls,
                            finish_reason,
                            usage: crate::normalized::Usage {
                                input_tokens: 50,
                                output_tokens,
                                cache_read_tokens: 0,
                                cache_write_tokens: 0,
                                reasoning_tokens: 0,
                                input_basis: crate::normalized::InputBasis::IncludesCache,
                            },
                            usage_measured: true,
                            routed_model: Some(raw_m.to_string()),
                            ttft_ms: Some(20),
                            turn_ms: 100,
                            raw_response: None,
                            thinking_signature,
                            redacted_thinking: None,
                        });
                    }
                }
            }

            // 2. 远端对等节点调用：将用户自定义别名前缀 (如 kiro-share/) 精准转换为 Broker 可精准寻址的对端真实 node_id
            let outbound_model = if let Some(ref peer) = target_peer {
                format!("{}/{}", peer.node_id, sub_model)
            } else {
                raw_m.to_string()
            };

            let body = serialize_chat_request_to_mesh_payload(request, &outbound_model);

            let client = {
                let consumer = self.service.consumer_client();
                let c = consumer.read().await;
                c.clone()
            };

            let mut resp = match client.chat_stream(&body).await {
                Ok(r) => r,
                Err(e) => {
                    return Err(UpstreamError::new(
                        crate::error::UpstreamKind::Upstream,
                        503,
                        format!("无法连接 Nexus Mesh 云端/对等节点: {e}"),
                    ));
                }
            };

            if !resp.status().is_success() {
                let status_code = resp.status().as_u16();
                let err_text = resp.text().await.unwrap_or_else(|_| "未知错误".to_string());
                return Err(UpstreamError::new(
                    crate::error::UpstreamKind::Upstream,
                    status_code,
                    format!("Mesh 对端节点返回错误: {err_text}"),
                ));
            }

            let mut full_text = String::new();
            let mut full_thinking = String::new();
            let mut tool_calls = Vec::new();
            let mut finish_reason = crate::normalized::FinishReason::Stop;
            let mut thinking_signature: Option<String> = None;
            if !preemption_notice.is_empty() {
                full_text.push_str(preemption_notice);
            }

            let mut line_buf = String::new();

            while let Some(chunk) = resp.chunk().await.map_err(|e| {
                UpstreamError::new(
                    crate::error::UpstreamKind::Upstream,
                    502,
                    format!("Mesh 流传输中断: {e}"),
                )
            })? {
                let text = String::from_utf8_lossy(&chunk);
                line_buf.push_str(&text);

                while let Some(pos) = line_buf.find('\n') {
                    let line = line_buf[..pos].trim().to_string();
                    line_buf = line_buf[pos + 1..].to_string();

                    if line.starts_with("data: ") {
                        let data = line.trim_start_matches("data: ").trim();
                        if data == "[DONE]" {
                            break;
                        }
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                            if let Some(content) = v
                                .pointer("/choices/0/delta/content")
                                .and_then(|c| c.as_str())
                            {
                                if !content.is_empty() {
                                    on_delta(Delta::Text(content.to_string()));
                                    full_text.push_str(content);
                                }
                            }
                            if let Some(th) = v
                                .pointer("/choices/0/delta/reasoning_content")
                                .or_else(|| v.pointer("/choices/0/delta/thinking"))
                                .and_then(|c| c.as_str())
                            {
                                if !th.is_empty() {
                                    on_delta(Delta::Thinking(th.to_string()));
                                    full_thinking.push_str(th);
                                }
                            }
                            if let Some(sig) = v
                                .pointer("/choices/0/delta/thinking_signature")
                                .and_then(|s| s.as_str())
                            {
                                on_delta(Delta::ThinkingSignature(sig.to_string()));
                                thinking_signature = Some(sig.to_string());
                            }
                            if let Some(delta_calls) = v
                                .pointer("/choices/0/delta/tool_calls")
                                .and_then(|c| c.as_array())
                            {
                                for call_val in delta_calls {
                                    let idx = call_val
                                        .get("index")
                                        .and_then(|i| i.as_u64())
                                        .unwrap_or(0) as usize;
                                    while tool_calls.len() <= idx {
                                        tool_calls.push(crate::normalized::ToolCall {
                                            id: String::new(),
                                            name: String::new(),
                                            arguments: String::new(),
                                        });
                                    }
                                    if let Some(id) = call_val.get("id").and_then(|s| s.as_str()) {
                                        tool_calls[idx].id.push_str(id);
                                    }
                                    if let Some(name) = call_val
                                        .pointer("/function/name")
                                        .and_then(|s| s.as_str())
                                    {
                                        tool_calls[idx].name.push_str(name);
                                    }
                                    if let Some(args) = call_val
                                        .pointer("/function/arguments")
                                        .and_then(|s| s.as_str())
                                    {
                                        tool_calls[idx].arguments.push_str(args);
                                    }
                                }
                            }
                            if let Some(fr) = v
                                .pointer("/choices/0/finish_reason")
                                .and_then(|s| s.as_str())
                            {
                                finish_reason = match fr {
                                    "tool_calls" | "tool_use" => crate::normalized::FinishReason::ToolCalls,
                                    "length" | "max_tokens" => crate::normalized::FinishReason::Length,
                                    "content_filter" => crate::normalized::FinishReason::ContentFilter,
                                    _ => crate::normalized::FinishReason::Stop,
                                };
                            }
                        }
                    }
                }
            }

            if !tool_calls.is_empty() {
                finish_reason = crate::normalized::FinishReason::ToolCalls;
            }

            Ok(Completion {
                text: full_text,
                thinking: full_thinking,
                tool_calls,
                finish_reason,
                usage: crate::normalized::Usage {
                    input_tokens: 50,
                    output_tokens: 150,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                    input_basis: crate::normalized::InputBasis::IncludesCache,
                },
                usage_measured: true,
                routed_model: Some(raw_m.to_string()),
                ttft_ms: Some(150),
                turn_ms: 400,
                raw_response: None,
                thinking_signature,
                redacted_thinking: None,
            })
        })
    }

    fn count_tokens<'a>(
        &'a self,
        _credential: &'a Credential,
        _request: &'a ChatRequest,
    ) -> BoxFuture<'a, Result<u64, UpstreamError>> {
        Box::pin(async move { Ok(42) })
    }
}

pub fn mesh_channel(lane: Arc<dyn Lane>, service: Arc<MeshService>) -> Channel {
    Channel {
        id: MESH,
        label: "Mesh 共享网络",
        vendor: "nexus-mesh",
        prefixes: ROUTE_PREFIXES,
        lane,
        upstream: Arc::new(MeshUpstream::new(service.clone())),
        gate: Arc::new(MeshGate::new(service)),
        passthrough: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_mesh::types::{HostChannel, MeshConfig, MeshPeer, MeshRole};
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn test_mesh_gate_owns_and_claims_bare() {
        let mut config = MeshConfig::default();
        config.peers = vec![MeshPeer {
            node_id: "node_remote_999".to_string(),
            device_name: "Kiro-share".to_string(),
            status: "connected".to_string(),
            latency_ms: 12,
            is_self: true,
            shared_channels: vec!["kiro".to_string()],
            remote_models: vec!["kiro-claude-sonnet-5".to_string()],
            paired_at: 0,
            last_seen_at: 0,
        }];

        let service = Arc::new(MeshService::new(config));
        let gate = MeshGate::new(service);

        // 1. 带别名前缀
        assert!(gate.owns(Capability::Chat, "kiro-share/kiro-claude-sonnet-5"));
        assert!(gate.owns(Capability::Chat, "mesh/kiro-share/kiro-claude-sonnet-5"));
        assert!(gate.claims_bare("kiro-share/kiro-claude-sonnet-5"));

        // 2. 带 node_id 前缀
        assert!(gate.owns(Capability::Chat, "node_remote_999/kiro-claude-sonnet-5"));
        assert!(gate.claims_bare("node_remote_999/kiro-claude-sonnet-5"));

        // 3. 裸模型名
        assert!(gate.owns(Capability::Chat, "kiro-claude-sonnet-5"));

        // 4. models 导出清单包含正确别名且无双重前缀
        let models = gate.models(Capability::Chat);
        assert!(models.contains(&"kiro-share/kiro-claude-sonnet-5".to_string()));
    }

    #[tokio::test]
    async fn test_mesh_upstream_local_loopback_execution() {
        let mut config = MeshConfig::default();
        config.role = MeshRole::Provider;
        config.host_channels = vec![HostChannel {
            channel_id: "kiro".to_string(),
            label: "Kiro".to_string(),
            tier: "tier1_local".to_string(),
            enabled: true,
            max_concurrency: 2,
            reserve_quota_percent: 20,
            allowed_models: vec!["kiro-claude-sonnet-5".to_string()],
        }];
        config.peers = vec![MeshPeer {
            node_id: config.node_id.0.clone(),
            device_name: "Kiro-share".to_string(),
            status: "connected".to_string(),
            latency_ms: 2,
            is_self: true,
            shared_channels: vec!["kiro".to_string()],
            remote_models: vec!["kiro-claude-sonnet-5".to_string()],
            paired_at: 0,
            last_seen_at: 0,
        }];

        let service = Arc::new(MeshService::new(config));

        // 模拟本地 hook
        let hook_called = Arc::new(AtomicUsize::new(0));
        let hook_called_clone = hook_called.clone();
        let hook: nexus_mesh::tunnel::client::MeshInvokeHook = Arc::new(
            move |channel_id, model, _payload, outbound_tx, stream_id| {
                let called = hook_called_clone.clone();
                Box::pin(async move {
                    assert_eq!(channel_id, "kiro");
                    assert_eq!(model, "kiro-claude-sonnet-5");
                    called.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                    let _ = outbound_tx
                        .send(TunnelFrame::Delta {
                            stream_id,
                            text: "hello from local loopback".to_string(),
                            is_thinking: false,
                        })
                        .await;
                    let _ = outbound_tx
                        .send(TunnelFrame::Finish {
                            stream_id,
                            total_tokens: Some(10),
                            finish_reason: Some("stop".to_string()),
                            tool_calls: None,
                            thinking_signature: None,
                        })
                        .await;
                    Ok(())
                })
            },
        );

        service.set_invoke_hook(Some(hook));

        let upstream = MeshUpstream::new(service);
        let cred = Credential {
            label: "test".to_string(),
            access_token: "test".to_string(),
            identity: crate::identity::DeviceIdentity::derived("test"),
        };
        let req = ChatRequest {
            model: "kiro-share/kiro-claude-sonnet-5".to_string(),
            messages: vec![crate::normalized::Message::text(
                crate::normalized::Role::User,
                "hi",
            )],
            ..Default::default()
        };

        let mut received_deltas = Vec::new();
        let mut sink = |d: Delta| {
            if let Delta::Text(t) = d {
                received_deltas.push(t);
            }
        };

        let result = upstream.stream(&cred, &req, &mut sink).await.unwrap();
        assert_eq!(result.text, "hello from local loopback");
        assert_eq!(received_deltas, vec!["hello from local loopback".to_string()]);
        assert_eq!(hook_called.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_mesh_upstream_tool_calls_and_thinking_propagation() {
        let mut config = MeshConfig::default();
        config.role = MeshRole::Provider;
        config.host_channels = vec![HostChannel {
            channel_id: "kiro".to_string(),
            label: "Kiro".to_string(),
            tier: "tier1_local".to_string(),
            enabled: true,
            max_concurrency: 2,
            reserve_quota_percent: 20,
            allowed_models: vec!["kiro-claude-sonnet-5".to_string()],
        }];
        config.peers = vec![MeshPeer {
            node_id: config.node_id.0.clone(),
            device_name: "Kiro-share".to_string(),
            status: "connected".to_string(),
            latency_ms: 2,
            is_self: true,
            shared_channels: vec!["kiro".to_string()],
            remote_models: vec!["kiro-claude-sonnet-5".to_string()],
            paired_at: 0,
            last_seen_at: 0,
        }];

        let service = Arc::new(MeshService::new(config));

        // 模拟产生 thinking 和 tool_calls
        let hook: nexus_mesh::tunnel::client::MeshInvokeHook = Arc::new(
            move |_channel_id, _model, _payload, outbound_tx, stream_id| {
                Box::pin(async move {
                    let _ = outbound_tx
                        .send(TunnelFrame::Delta {
                            stream_id,
                            text: "thinking step 1...".to_string(),
                            is_thinking: true,
                        })
                        .await;
                    let _ = outbound_tx
                        .send(TunnelFrame::Finish {
                            stream_id,
                            total_tokens: Some(42),
                            finish_reason: Some("tool_use".to_string()),
                            tool_calls: Some(vec![nexus_mesh::protocol::ToolCallWire {
                                id: "call_tool_01".to_string(),
                                name: "bash".to_string(),
                                arguments: r#"{"command":"git status"}"#.to_string(),
                            }]),
                            thinking_signature: Some("sig_valid_123".to_string()),
                        })
                        .await;
                    Ok(())
                })
            },
        );

        service.set_invoke_hook(Some(hook));

        let upstream = MeshUpstream::new(service);
        let cred = Credential {
            label: "test".to_string(),
            access_token: "test".to_string(),
            identity: crate::identity::DeviceIdentity::derived("test"),
        };
        let req = ChatRequest {
            model: "kiro-share/kiro-claude-sonnet-5".to_string(),
            messages: vec![crate::normalized::Message::text(
                crate::normalized::Role::User,
                "查看本地改动",
            )],
            ..Default::default()
        };

        let mut received_thinking = Vec::new();
        let mut received_sigs = Vec::new();
        let mut sink = |d: Delta| match d {
            Delta::Thinking(t) => received_thinking.push(t),
            Delta::ThinkingSignature(s) => received_sigs.push(s),
            _ => {}
        };

        let result = upstream.stream(&cred, &req, &mut sink).await.unwrap();
        assert_eq!(received_thinking, vec!["thinking step 1...".to_string()]);
        assert_eq!(received_sigs, vec!["sig_valid_123".to_string()]);
        assert_eq!(result.finish_reason, crate::normalized::FinishReason::ToolCalls);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "bash");
        assert_eq!(result.tool_calls[0].arguments, r#"{"command":"git status"}"#);
        assert_eq!(result.thinking_signature.as_deref(), Some("sig_valid_123"));
    }
}
