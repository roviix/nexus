//! Cursor 模型能力广播与动态参数注入（AvailableModels / GetUsableModels）。
//!
//! 核心职责：
//! 1. 拦截 Cursor 的 `/aiserver.v1.AiService/AvailableModels` 与 `/agent.v1.AgentService/GetUsableModels`；
//! 2. 动态注入 Nexus 接入模型（如 `gpt-5.6-luna (Nexus)`，以及网关配置的自选模型），并打上：
//!    - `supports_agent: true`（解锁 Cursor Composer 的 ∞ Agent 模式）
//!    - `supports_thinking: true`（解锁 Extended Thinking 思考过程展示）
//!    - `supports_max_mode: true` / `supports_plan_mode: true`
//!    - `parameter_definitions`: 注入 `effort`（low/medium/high/max）与 `context`（200k/1m）参数；
//! 3. 与 Cursor 官方模型列表无缝融合（通过 Protobuf 消息连结），消除 `Model name is not valid`。

use super::proto::agent::v1 as agent;
use crate::server::Gateway;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use bytes::{BufMut, BytesMut};
use prost::Message;
use std::sync::Arc;

#[derive(Clone, PartialEq, Message)]
pub struct AvailableModelsAddition {
    #[prost(string, repeated, tag = "1")]
    pub model_names: Vec<String>,
    #[prost(message, repeated, tag = "2")]
    pub models: Vec<AvailableModel>,
}

#[derive(Clone, PartialEq, Message)]
pub struct AvailableModel {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(bool, tag = "2")]
    pub default_on: bool,
    #[prost(bool, optional, tag = "5")]
    pub supports_agent: Option<bool>,
    #[prost(int32, optional, tag = "6")]
    pub degradation_status: Option<i32>,
    #[prost(message, optional, tag = "8")]
    pub tooltip_data: Option<TooltipData>,
    #[prost(bool, optional, tag = "9")]
    pub supports_thinking: Option<bool>,
    #[prost(bool, optional, tag = "10")]
    pub supports_images: Option<bool>,
    #[prost(bool, optional, tag = "14")]
    pub supports_max_mode: Option<bool>,
    #[prost(string, optional, tag = "17")]
    pub client_display_name: Option<String>,
    #[prost(string, optional, tag = "18")]
    pub server_model_name: Option<String>,
    #[prost(bool, optional, tag = "19")]
    pub supports_non_max_mode: Option<bool>,
    #[prost(message, optional, tag = "20")]
    pub tooltip_data_for_max_mode: Option<TooltipData>,
    #[prost(bool, optional, tag = "21")]
    pub is_recommended_for_background_composer: Option<bool>,
    #[prost(bool, optional, tag = "22")]
    pub supports_plan_mode: Option<bool>,
    #[prost(string, optional, tag = "24")]
    pub inputbox_short_model_name: Option<String>,
    #[prost(bool, optional, tag = "25")]
    pub supports_sandboxing: Option<bool>,
    #[prost(bool, optional, tag = "26")]
    pub supports_cmd_k: Option<bool>,
    #[prost(message, repeated, tag = "29")]
    pub parameter_definitions: Vec<ModelParameterDefinition>,
    #[prost(message, repeated, tag = "30")]
    pub variants: Vec<ModelVariant>,
    #[prost(string, repeated, tag = "36")]
    pub legacy_slugs: Vec<String>,
    #[prost(int32, optional, tag = "38")]
    pub named_model_section_index: Option<i32>,
    #[prost(string, optional, tag = "41")]
    pub vendor_name: Option<String>,
    #[prost(message, optional, tag = "42")]
    pub vendor: Option<AvailableModelVendor>,
    #[prost(message, repeated, tag = "48")]
    pub model_picker_badges: Vec<ModelPickerBadge>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TooltipData {
    #[prost(string, optional, tag = "7")]
    pub markdown_content: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ModelParameterDefinition {
    #[prost(string, tag = "1")]
    pub id: String,
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(string, optional, tag = "3")]
    pub markdown_tooltip: Option<String>,
    #[prost(message, optional, tag = "4")]
    pub parameter_type: Option<ModelParameterType>,
    #[prost(bool, optional, tag = "5")]
    pub is_cycleable_by_hotkey: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ModelParameterType {
    #[prost(message, optional, tag = "1")]
    pub boolean_parameter: Option<BooleanParameter>,
    #[prost(message, optional, tag = "2")]
    pub enum_parameter: Option<EnumParameter>,
}

#[derive(Clone, PartialEq, Message)]
pub struct BooleanParameter {
    #[prost(message, repeated, tag = "1")]
    pub values: Vec<BooleanParameterValue>,
}

#[derive(Clone, PartialEq, Message)]
pub struct BooleanParameterValue {
    #[prost(string, tag = "1")]
    pub value: String,
    #[prost(string, optional, tag = "2")]
    pub display_name: Option<String>,
    #[prost(bool, optional, tag = "3")]
    pub increases_model_cost: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
pub struct EnumParameter {
    #[prost(message, repeated, tag = "1")]
    pub values: Vec<EnumParameterValue>,
}

#[derive(Clone, PartialEq, Message)]
pub struct EnumParameterValue {
    #[prost(string, tag = "1")]
    pub value: String,
    #[prost(string, optional, tag = "2")]
    pub display_name: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ModelVariant {
    #[prost(message, repeated, tag = "1")]
    pub parameter_values: Vec<ModelParameterValue>,
    #[prost(string, tag = "2")]
    pub display_name: String,
    #[prost(bool, tag = "3")]
    pub is_max_mode: bool,
    #[prost(bool, optional, tag = "4")]
    pub is_default_max_config: Option<bool>,
    #[prost(bool, optional, tag = "5")]
    pub is_default_non_max_config: Option<bool>,
    #[prost(message, optional, tag = "6")]
    pub tooltip_data: Option<TooltipData>,
    #[prost(string, optional, tag = "8")]
    pub display_name_outside_picker: Option<String>,
    #[prost(string, optional, tag = "9")]
    pub variant_string_representation: Option<String>,
    #[prost(string, optional, tag = "11")]
    pub legacy_slug: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ModelParameterValue {
    #[prost(string, tag = "1")]
    pub id: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct ModelPickerBadge {
    #[prost(string, tag = "1")]
    pub label: String,
    #[prost(int32, tag = "2")]
    pub variant: i32,
    #[prost(bool, tag = "3")]
    pub dismiss_on_selection: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct AvailableModelVendor {
    #[prost(int32, tag = "1")]
    pub id: i32,
    #[prost(string, tag = "2")]
    pub display_name: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct UsableModelsAddition {
    #[prost(message, repeated, tag = "1")]
    pub models: Vec<agent::ModelDetails>,
}

const CONTEXTS: [(&str, &str); 3] = [("200k", "200K"), ("500k", "500K"), ("1m", "1M")];

const EFFORTS: [(&str, &str); 4] = [
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("max", "Max"),
];

fn default_model_parameters() -> Vec<ModelParameterDefinition> {
    vec![
        ModelParameterDefinition {
            id: "context".into(),
            name: "Context".into(),
            markdown_tooltip: Some("上下文长度：达到阈值时自动触发上下文压缩与多轮会话管理".into()),
            parameter_type: Some(ModelParameterType {
                boolean_parameter: None,
                enum_parameter: Some(EnumParameter {
                    values: CONTEXTS
                        .iter()
                        .map(|(val, label)| EnumParameterValue {
                            value: (*val).into(),
                            display_name: Some((*label).into()),
                        })
                        .collect(),
                }),
            }),
            is_cycleable_by_hotkey: Some(false),
        },
        ModelParameterDefinition {
            id: "effort".into(),
            name: "Effort".into(),
            markdown_tooltip: Some("思考强度：控制 Extended Thinking 与推理模型的思考深度".into()),
            parameter_type: Some(ModelParameterType {
                boolean_parameter: None,
                enum_parameter: Some(EnumParameter {
                    values: EFFORTS
                        .iter()
                        .map(|(val, label)| EnumParameterValue {
                            value: (*val).into(),
                            display_name: Some((*label).into()),
                        })
                        .collect(),
                }),
            }),
            is_cycleable_by_hotkey: Some(true),
        },
        ModelParameterDefinition {
            id: "fast".into(),
            name: "Fast".into(),
            markdown_tooltip: Some("快速推理加速开关".into()),
            parameter_type: Some(ModelParameterType {
                boolean_parameter: Some(BooleanParameter {
                    values: vec![
                        BooleanParameterValue {
                            value: "false".into(),
                            display_name: None,
                            increases_model_cost: None,
                        },
                        BooleanParameterValue {
                            value: "true".into(),
                            display_name: Some("Fast".into()),
                            increases_model_cost: Some(true),
                        },
                    ],
                }),
                enum_parameter: None,
            }),
            is_cycleable_by_hotkey: Some(false),
        },
    ]
}

fn build_nexus_available_model(model_name: &str, display_name: &str) -> AvailableModel {
    let tooltip = TooltipData {
        markdown_content: Some(format!(
            "**{display_name}**<br /><br />由 Nexus 网关智能调度 · 原生支持 Agent 模式与 Extended Thinking 思考流"
        )),
    };

    let mut variants = Vec::new();
    for (ctx_val, _ctx_label) in CONTEXTS {
        for (eff_val, _) in EFFORTS {
            variants.push(ModelVariant {
                parameter_values: vec![
                    ModelParameterValue {
                        id: "context".into(),
                        value: ctx_val.into(),
                    },
                    ModelParameterValue {
                        id: "effort".into(),
                        value: eff_val.into(),
                    },
                    ModelParameterValue {
                        id: "fast".into(),
                        value: "false".into(),
                    },
                ],
                display_name: display_name.into(),
                is_max_mode: eff_val == "max",
                is_default_max_config: (eff_val == "max" && ctx_val == "200k").then_some(true),
                is_default_non_max_config: (eff_val == "high" && ctx_val == "200k").then_some(true),
                tooltip_data: Some(tooltip.clone()),
                display_name_outside_picker: Some(display_name.into()),
                variant_string_representation: Some(format!(
                    "{model_name}[context={ctx_val},effort={eff_val},fast=false]"
                )),
                legacy_slug: Some(format!("{model_name}-{ctx_val}-{eff_val}")),
            });
        }
    }

    AvailableModel {
        name: model_name.into(),
        default_on: true,
        supports_agent: Some(true),
        degradation_status: Some(0),
        tooltip_data: Some(tooltip.clone()),
        supports_thinking: Some(true),
        supports_images: Some(true),
        supports_max_mode: Some(true),
        client_display_name: Some(display_name.into()),
        server_model_name: Some(model_name.into()),
        supports_non_max_mode: Some(true),
        tooltip_data_for_max_mode: Some(tooltip),
        is_recommended_for_background_composer: Some(true),
        supports_plan_mode: Some(true),
        inputbox_short_model_name: Some(display_name.into()),
        supports_sandboxing: Some(true),
        supports_cmd_k: Some(true),
        parameter_definitions: default_model_parameters(),
        variants,
        legacy_slugs: vec![model_name.into()],
        named_model_section_index: Some(1),
        vendor_name: Some("Nexus".into()),
        vendor: Some(AvailableModelVendor {
            id: 99,
            display_name: "Nexus".into(),
        }),
        model_picker_badges: vec![ModelPickerBadge {
            label: "Nexus".into(),
            variant: 1,
            dismiss_on_selection: false,
        }],
    }
}

fn collect_nexus_models(gw: &Gateway) -> Vec<(String, String)> {
    let mut list = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut add_model = |model_id: String, display_name: String| {
        let key = model_id.to_ascii_lowercase();
        if seen.insert(key) {
            list.push((model_id, display_name));
        }
    };

    // 严格跟随 Cursor 接入配置：接入什么模型，Cursor 里才看到什么模型
    if let Ok(routes) = gw.routes.read() {
        if let Some(cursor_route) = routes.get("cursor") {
            let configured = cursor_route.model.trim();
            if !configured.is_empty() {
                // 清洗通道与对等节点前缀 (例如 "kiro/kiro-claude-opus-5.5-thinking" 或 "kiro-share/kiro-claude-opus-5.5-thinking")
                let bare = configured
                    .split_once('/')
                    .map(|(_, r)| r)
                    .unwrap_or(configured)
                    .trim();
                let bare_clean = nexus_store::key_providers::bare_model(bare);

                // 1. Cursor 下拉列表中的专属接入模型名：如 "kiro-claude-opus-5.5-thinking (Nexus)"
                add_model(
                    format!("{bare_clean} (Nexus)"),
                    format!("{bare_clean} (Nexus)"),
                );
                // 2. 裸模型名兼容项：如 "kiro-claude-opus-5.5-thinking"
                add_model(bare_clean.to_string(), format!("{bare_clean} (Nexus)"));
            }

            // 用户配置的别名
            for alias in &cursor_route.aliases {
                let alias_clean = alias.trim();
                if alias_clean.is_empty() {
                    continue;
                }
                let bare = alias_clean
                    .split_once('/')
                    .map(|(_, r)| r)
                    .unwrap_or(alias_clean)
                    .trim();
                let bare_clean = nexus_store::key_providers::bare_model(bare);
                add_model(
                    format!("{bare_clean} (Nexus)"),
                    format!("{bare_clean} (Nexus)"),
                );
                add_model(bare_clean.to_string(), format!("{bare_clean} (Nexus)"));
            }
        }
    }

    list
}

/// 处理 AvailableModels 请求（合并官方模型与 Nexus 模型，打上 supports_agent: true）
pub async fn handle_available_models(
    State(gw): State<Arc<Gateway>>,
    headers: HeaderMap,
    body: Bytes,
    path_and_query: &str,
) -> Response {
    let nexus_models = collect_nexus_models(&gw);
    let mut model_names = Vec::new();
    let mut available_models = Vec::new();

    for (model_id, display_name) in &nexus_models {
        model_names.push(model_id.clone());
        available_models.push(build_nexus_available_model(model_id, display_name));
    }

    let addition = AvailableModelsAddition {
        model_names,
        models: available_models,
    };
    let extra_bytes = addition.encode_to_vec();

    // 尝试透传获取官方底座模型并合并
    let upstream_res =
        super::native_relay::handle_native_relay(State(gw), headers, body, path_and_query).await;
    if upstream_res.status().is_success() {
        let (parts, upstream_body) = upstream_res.into_parts();
        if let Ok(bytes) = axum::body::to_bytes(upstream_body, crate::server::MAX_BODY_BYTES).await
        {
            let merged_bytes = merge_protobuf_payload(&bytes, &extra_bytes);
            return Response::from_parts(parts, axum::body::Body::from(merged_bytes));
        }
    }

    // 官方不可用时，直接返回本地生成的 Nexus 模型定义
    local_protobuf_response(extra_bytes)
}

/// 处理 GetUsableModels 请求（合并官方可用模型与 Nexus 模型）
pub async fn handle_usable_models(
    State(gw): State<Arc<Gateway>>,
    headers: HeaderMap,
    body: Bytes,
    path_and_query: &str,
) -> Response {
    let nexus_models = collect_nexus_models(&gw);
    let usable_models = nexus_models
        .into_iter()
        .map(|(model_id, display_name)| agent::ModelDetails {
            model_id: model_id.clone(),
            display_model_id: model_id.clone(),
            display_name: display_name.clone(),
            display_name_short: display_name,
            thinking_details: Some(agent::ThinkingDetails::default()),
            ..Default::default()
        })
        .collect();

    let addition = UsableModelsAddition {
        models: usable_models,
    };
    let extra_bytes = addition.encode_to_vec();

    let upstream_res =
        super::native_relay::handle_native_relay(State(gw), headers, body, path_and_query).await;
    if upstream_res.status().is_success() {
        let (parts, upstream_body) = upstream_res.into_parts();
        if let Ok(bytes) = axum::body::to_bytes(upstream_body, crate::server::MAX_BODY_BYTES).await
        {
            let merged_bytes = merge_protobuf_payload(&bytes, &extra_bytes);
            return Response::from_parts(parts, axum::body::Body::from(merged_bytes));
        }
    }

    local_protobuf_response(extra_bytes)
}

fn merge_protobuf_payload(upstream: &[u8], extra: &[u8]) -> Bytes {
    if upstream.len() < 5 {
        let mut buf = BytesMut::with_capacity(upstream.len() + extra.len());
        buf.extend_from_slice(upstream);
        buf.extend_from_slice(extra);
        return buf.freeze();
    }

    let flags = upstream[0];
    let length = u32::from_be_bytes([upstream[1], upstream[2], upstream[3], upstream[4]]) as usize;
    if length == upstream.len() - 5 && flags == 0 {
        // Connect-RPC unary envelope 格式：flag(1) + len(4) + data
        let payload = &upstream[5..];
        let mut merged = BytesMut::with_capacity(5 + payload.len() + extra.len());
        merged.put_u8(0);
        merged.put_u32((payload.len() + extra.len()) as u32);
        merged.extend_from_slice(payload);
        merged.extend_from_slice(extra);
        merged.freeze()
    } else {
        let mut merged = BytesMut::with_capacity(upstream.len() + extra.len());
        merged.extend_from_slice(upstream);
        merged.extend_from_slice(extra);
        merged.freeze()
    }
}

fn local_protobuf_response(payload: Vec<u8>) -> Response {
    let mut buf = BytesMut::with_capacity(5 + payload.len());
    buf.put_u8(0);
    buf.put_u32(payload.len() as u32);
    buf.extend_from_slice(&payload);

    let mut res = Response::new(axum::body::Body::from(buf.freeze()));
    *res.status_mut() = StatusCode::OK;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/proto"),
    );
    res.headers_mut().insert(
        header::HeaderName::from_static("connect-protocol-version"),
        HeaderValue::from_static("1"),
    );
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_nexus_available_model_capabilities() {
        let model = build_nexus_available_model("gpt-5.6-luna (Nexus)", "Luna (Nexus)");
        assert_eq!(model.name, "gpt-5.6-luna (Nexus)");
        assert_eq!(model.client_display_name.as_deref(), Some("Luna (Nexus)"));
        assert_eq!(model.supports_agent, Some(true));
        assert_eq!(model.supports_thinking, Some(true));
        assert_eq!(model.supports_max_mode, Some(true));
        assert_eq!(model.supports_plan_mode, Some(true));

        assert_eq!(model.parameter_definitions.len(), 3);
        assert_eq!(model.parameter_definitions[0].id, "context");
        assert_eq!(model.parameter_definitions[1].id, "effort");
        assert_eq!(model.parameter_definitions[2].id, "fast");

        let param_type = model.parameter_definitions[1]
            .parameter_type
            .as_ref()
            .unwrap();
        let enum_param = param_type.enum_parameter.as_ref().unwrap();
        assert_eq!(enum_param.values.len(), 4);
        assert_eq!(enum_param.values[0].value, "low");
        assert_eq!(enum_param.values[3].value, "max");
    }

    #[test]
    fn test_merge_protobuf_payload_connect_envelope() {
        let msg1 = b"hello";
        let mut envelope1 = Vec::new();
        envelope1.push(0u8);
        envelope1.extend_from_slice(&(msg1.len() as u32).to_be_bytes());
        envelope1.extend_from_slice(msg1);

        let msg2 = b"world";
        let merged = merge_protobuf_payload(&envelope1, msg2);

        assert_eq!(merged[0], 0);
        let len = u32::from_be_bytes([merged[1], merged[2], merged[3], merged[4]]) as usize;
        assert_eq!(len, msg1.len() + msg2.len());
        assert_eq!(&merged[5..5 + msg1.len()], msg1);
        assert_eq!(&merged[5 + msg1.len()..], msg2);
    }

    #[test]
    fn test_collect_nexus_models_strictly_matches_cursor_route() {
        struct DummyUpstream;
        impl crate::upstream::Upstream for DummyUpstream {
            fn stream<'a>(
                &'a self,
                _credential: &'a crate::lane::Credential,
                _request: &'a crate::normalized::ChatRequest,
                _on_delta: crate::upstream::DeltaSink<'a>,
            ) -> crate::lane::BoxFuture<
                'a,
                Result<crate::normalized::Completion, crate::error::UpstreamError>,
            > {
                Box::pin(async { unimplemented!() })
            }
        }

        let lane = std::sync::Arc::new(crate::lane::StaticLane::new(crate::lane::Credential {
            label: "test".into(),
            access_token: "".into(),
            identity: crate::identity::DeviceIdentity::derived("tok"),
        }));
        let gw = Gateway::single(lane, std::sync::Arc::new(DummyUpstream));

        // 1. 无 cursor 路由配置时，不注入任何额外模型（保持 Cursor 原生干净）
        let empty_list = collect_nexus_models(&gw);
        assert!(empty_list.is_empty());

        // 2. 配置了 Mesh / Kiro 模型后，只严格注入该模型及其 Nexus 别名
        gw.routes.write().unwrap().insert(
            "cursor".to_string(),
            crate::routes::ClientRoute {
                model: "kiro-share/kiro-claude-opus-5.5-thinking".to_string(),
                opus: None,
                haiku: None,
                fable: None,
                context1m: false,
                aliases: vec!["kiro/custom-opus".to_string()],
            },
        );

        let models = collect_nexus_models(&gw);
        let model_names: Vec<String> = models.into_iter().map(|(id, _)| id).collect();

        assert_eq!(
            model_names,
            vec![
                "kiro-claude-opus-5.5-thinking (Nexus)".to_string(),
                "kiro-claude-opus-5.5-thinking".to_string(),
                "custom-opus (Nexus)".to_string(),
                "custom-opus".to_string(),
            ]
        );
    }
}
