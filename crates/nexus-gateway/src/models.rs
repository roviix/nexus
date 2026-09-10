//! 模型名映射。
//!
//! 客户端说的是**它自己世界**的模型名：Claude Code 发 `claude-sonnet-4-5-20250929`，
//! OpenAI SDK 发 `gpt-4o`。Cursor 只认自己那套（`claude-sonnet-5` / `gpt-5.6-sol` / …），
//! 名字对不上就是 `ERROR_BAD_MODEL_NAME`——整轮请求废掉，而客户端收到的是一句它无法理解
//! 的参数错误。这是「Claude Code 指过来就用不了」的**唯一根因**，不映射这条路就是断的。
//!
//! 三条规则，按顺序：
//! 1. **已是 Cursor 的名字就原样放行**（含 `-thinking` / `-max` / `-fast` 这类后缀变体）。
//!    reasoning 档位在 Cursor 这边编码在模型名里，不能擅自改写。
//! 2. **按家族前缀映射**：`claude-sonnet-*` → `claude-sonnet-5`。匹配的是家族而不是具体版本，
//!    因为客户端的版本号（日期戳）会一直变，而我们能给的就那几个。
//! 3. **认不出就回退 `auto`**，让上游自己挑，同时在响应里如实报出实际路由到的模型。
//!    宁可给一个能用的答案 + 一条日志，也不要一个 400——用户那边看到的会是「这个工具坏了」。

/// Cursor 侧已知的模型名。`/v1/models` 报的就是它，也是「原样放行」的判据。
pub const CURSOR_MODELS: &[&str] = &[
    "auto",
    "claude-sonnet-5",
    "claude-opus-5",
    "claude-opus-5-thinking-max-fast",
    "gpt-5.6-sol",
    "gpt-5.6-sol-max-fast",
    "gpt-5.6-terra",
    "grok-4.6",
    "grok-4.5",
    "gemini-3.7-flash",
];

/// 家族前缀 → Cursor 在这一族的**起步大版本**。
///
/// 用来放行清单里没列、但上游认识的变体（`-thinking-high`、`-max`、新出的小版本）：与其
/// 维护一张永远落后的白名单，不如认家族——发错了上游会给出准确的报错，那比我们擅自改名成
/// 另一个模型诚实。但**必须同时看版本号**：`claude-sonnet-4-5-20250929` 也带 `claude-sonnet-`
/// 前缀，却是 Anthropic 自己的名字，原样发出去换来的是 `ERROR_BAD_MODEL_NAME`。
/// `None` = 这一族不需要版本判断（名字本身就只属于 Cursor）。
const CURSOR_FAMILIES: &[(&str, Option<u32>)] = &[
    ("claude-sonnet-", Some(5)),
    ("claude-opus-", Some(5)),
    ("claude-haiku-", Some(5)),
    ("gpt-5.", None), // 小数点已经把它和 gpt-5 / gpt-5-codex 区分开
    ("grok-", Some(4)),
    ("gemini-", Some(3)),
    ("composer", None),
    ("cursor-", None),
];

/// 客户端名 → Cursor 名。**最长前缀优先**，所以更具体的规则要排在前面。
const ALIASES: &[(&str, &str)] = &[
    // ── Anthropic（Claude Code 的默认模型都在这一族）──
    ("claude-3-5-haiku", "claude-sonnet-5"),
    ("claude-3-haiku", "claude-sonnet-5"),
    ("claude-3-5-sonnet", "claude-sonnet-5"),
    ("claude-3-7-sonnet", "claude-sonnet-5"),
    ("claude-3-opus", "claude-opus-5"),
    ("claude-4-sonnet", "claude-sonnet-5"),
    ("claude-4-opus", "claude-opus-5"),
    ("claude-sonnet-4", "claude-sonnet-5"),
    ("claude-opus-4", "claude-opus-5"),
    // Claude Code 用 haiku 干标题生成这类小活。Cursor 没有 haiku 档，给 auto 更合适：
    // 上游会挑一个便宜快模型，而不是拿 opus 去写一个标题。
    ("claude-haiku-4", "auto"),
    ("haiku", "auto"),
    // ── OpenAI ──
    ("gpt-4o-mini", "auto"),
    ("gpt-4o", "gpt-5.6-sol"),
    ("gpt-4.1-mini", "auto"),
    ("gpt-4.1", "gpt-5.6-sol"),
    ("gpt-4-turbo", "gpt-5.6-sol"),
    ("gpt-4", "gpt-5.6-sol"),
    ("gpt-3.5", "auto"),
    ("gpt-5-codex", "gpt-5.6-terra"),
    ("gpt-5-mini", "auto"),
    ("gpt-5", "gpt-5.6-sol"),
    ("o1-mini", "auto"),
    ("o1", "gpt-5.6-terra"),
    // ── Google ──
    ("gemini-2.5-flash", "gemini-3.7-flash"),
    ("gemini-2.5-pro", "gemini-3.7-flash"),
    ("gemini-1.5", "gemini-3.7-flash"),
    ("gemini-", "gemini-3.7-flash"),
    // ── xAI ──
    ("grok-3", "grok-4.6"),
    ("grok-2", "grok-4.6"),
];

/// 一次映射的结果。`note` 有值时说明我们动了客户端要的东西，调用方要记一行日志。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub upstream: String,
    pub note: Option<String>,
}

impl Resolved {
    fn passthrough(model: &str) -> Self {
        Self {
            upstream: model.to_string(),
            note: None,
        }
    }
}

fn normalize(model: &str) -> String {
    model.trim().to_lowercase()
}

/// 剥掉 Anthropic 的日期戳后缀（`-20250929`）与 OpenAI 的 `-latest`。
///
/// 客户端每次升级都会换一个新日期，按原名匹配的表第二天就过期了。
fn strip_version_suffix(name: &str) -> &str {
    if let Some(rest) = name.strip_suffix("-latest") {
        return rest;
    }
    // 尾部是 8 位数字（日期）就砍掉。
    if let Some(idx) = name.rfind('-') {
        let tail = &name[idx + 1..];
        if tail.len() == 8 && tail.chars().all(|c| c.is_ascii_digit()) {
            return &name[..idx];
        }
    }
    name
}

/// 客户端要的模型名 → 发给 Cursor 的模型名。
pub fn resolve(requested: &str) -> Resolved {
    let raw = normalize(requested);
    if raw.is_empty() {
        return Resolved::passthrough("auto");
    }
    // 有些客户端会带 provider 前缀（`anthropic/claude-…`、`openai/gpt-…`）。
    let raw = raw.rsplit('/').next().unwrap_or(&raw).to_string();

    if CURSOR_MODELS.contains(&raw.as_str()) {
        return Resolved::passthrough(&raw);
    }
    if is_cursor_family(&raw) {
        return Resolved::passthrough(&raw);
    }

    let base = strip_version_suffix(&raw);
    // 最长前缀优先：`claude-3-5-sonnet` 要先于 `claude-3` 命中。
    let hit = ALIASES
        .iter()
        .filter(|(from, _)| base.starts_with(from) || raw.starts_with(from))
        .max_by_key(|(from, _)| from.len());
    if let Some((from, to)) = hit {
        return Resolved {
            upstream: (*to).to_string(),
            note: Some(format!("{requested} → {to}（按 {from} 家族映射）")),
        };
    }
    Resolved {
        upstream: "auto".to_string(),
        note: Some(format!("{requested} 不是上游认识的模型，已回退 auto")),
    }
}

/// 模型广场用的一条目录项：一个 Cursor 模型名，加上界面需要的归类信息。
///
/// `series` / `variant` 让同底座的档位（`-thinking-max-fast` 这类）在列表里聚到一起；
/// `aliases` 是别名表里会映射到它的客户端叫法——用户在 Claude Code 里看到的名字和这里
/// 的名字对不上时，这一栏就是答案。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    /// 模型名。Cursor 那份是静态表；ChatGPT 那份可能来自上游拉到的目录，所以是 `String`。
    pub id: String,
    /// `anthropic` / `openai` / `google` / `xai` / `cursor`，与 shop 的 `ModelVendor` 同一套值。
    pub vendor: &'static str,
    pub vendor_label: &'static str,
    /// `chat` | `image`，与 shop 目录的 `modality` 同一套值。前端按它把生图模型分到图片会话。
    pub modality: &'static str,
    /// 同底座的档位共用一个系列名（就是不带档位后缀的那个模型名）。
    pub series: String,
    /// 档位：`standard`，或 id 去掉系列名后剩下的后缀（`thinking-max-fast`）。
    pub variant: String,
    pub aliases: Vec<&'static str>,
    pub note: Option<&'static str>,
    /// 出图模型固定出多大（`1536x1024`）；`None` = 认请求里的 `size`，界面该摆规格菜单。
    /// 对话模型恒为 `None`。Cursor 那条出图链路没有尺寸字段；ChatGPT 的 gpt-image 认。
    /// **一律序列化**（null 也发）：前端靠「键在不在」区分「新目录说不固定」和「老网关没这个字段」。
    pub fixed_size: Option<&'static str>,
}

/// 本地网关能出图的模型。`RunGenerateImage` 服务端其实忽略 `model_id`（背后固定是
/// Nano Banana 2 一档），所以只有一条；对外挂的名字与云端目录一致（`nano-banana-2`），
/// 别名是 Google 自己的型号名——两边用户看到的是同一个东西。
pub const IMAGE_MODELS: &[(&str, &[&str])] = &[("nano-banana-2", &["gemini-3.1-flash-image"])];

/// 这个名字是不是本地目录里的生图模型（含别名）。server 用它决定要不要把
/// `/v1/images/generations` 的请求放行到 lane。
pub fn is_image_model(name: &str) -> bool {
    let n = name.trim();
    IMAGE_MODELS
        .iter()
        .any(|(id, aliases)| *id == n || aliases.contains(&n))
}

fn vendor_of(id: &str) -> (&'static str, &'static str) {
    if id.starts_with("claude-") {
        ("anthropic", "Anthropic")
    } else if id.starts_with("gpt-") || id.starts_with("o1") || id.starts_with("o3") {
        ("openai", "OpenAI")
    } else if id.starts_with("gemini-") {
        ("google", "Google")
    } else if id.starts_with("grok-") {
        ("xai", "xAI")
    } else {
        ("cursor", "Cursor")
    }
}

/// 模型广场的本地目录：`CURSOR_MODELS` 逐条归类，别名反查自 `ALIASES`。
///
/// 系列的判定只认清单里**已有**的名字：`claude-opus-5-thinking-max-fast` 的系列是清单里
/// 也有的 `claude-opus-5`；一个名字若不是别的清单项的前缀延伸，它自己就是一个系列。
pub fn catalog() -> Vec<CatalogEntry> {
    let images = IMAGE_MODELS.iter().map(|(id, aliases)| {
        let (vendor, vendor_label) = vendor_of(aliases.first().copied().unwrap_or(id));
        CatalogEntry {
            id: id.to_string(),
            vendor,
            vendor_label,
            modality: "image",
            series: id.to_string(),
            variant: "standard".to_string(),
            aliases: aliases.to_vec(),
            note: Some(
                "经 Cursor 出图，固定 1536×1024；账号需要 Developer 或 Sand 计划的生图权限。",
            ),
            fixed_size: Some("1536x1024"),
        }
    });
    CURSOR_MODELS
        .iter()
        .map(|id| {
            let series = CURSOR_MODELS
                .iter()
                .filter(|other| **other != *id && id.starts_with(&format!("{other}-")))
                .max_by_key(|other| other.len())
                .copied()
                .unwrap_or(id);
            let variant = if series == *id {
                "standard".to_string()
            } else {
                id[series.len() + 1..].to_string()
            };
            let (vendor, vendor_label) = vendor_of(id);
            let aliases = ALIASES
                .iter()
                .filter(|(_, to)| to == id)
                .map(|(from, _)| *from)
                .collect();
            let note = match *id {
                "auto" => Some("让 Cursor 按请求挑模型；认不出的客户端模型名也落到这里。"),
                _ => None,
            };
            CatalogEntry {
                id: id.to_string(),
                vendor,
                vendor_label,
                modality: "chat",
                series: series.to_string(),
                variant,
                aliases,
                note,
                fixed_size: None,
            }
        })
        .chain(images)
        .collect()
}

/// ChatGPT 订阅号那一侧的目录：Codex 的对话模型（`chat_models`，静态表 ∪ 上游拉到的）+
/// `gpt-image-*`。只在有可用的 ChatGPT 号时并进模型广场（由 Tauri 命令层判断）；名字与
/// Cursor 重复的（`gpt-5.6-sol` 两边都有）以 ChatGPT 这条为准——同名请求本来也是它接。
pub fn chatgpt_catalog(chat_models: &[String]) -> Vec<CatalogEntry> {
    let chat = chat_models.iter().map(|id| CatalogEntry {
        id: id.clone(),
        vendor: "openai",
        vendor_label: "OpenAI",
        modality: "chat",
        series: id.clone(),
        variant: "standard".to_string(),
        aliases: Vec::new(),
        note: Some("经 ChatGPT 订阅号（Codex 协议）。模型名后可加档位后缀：-low / -medium / -high / -xhigh。"),
        fixed_size: None,
    });
    let images = crate::codex::protocol::CODEX_IMAGE_MODELS.iter().map(|id| CatalogEntry {
        id: id.to_string(),
        vendor: "openai",
        vendor_label: "OpenAI",
        modality: "image",
        series: id.to_string(),
        variant: "standard".to_string(),
        aliases: Vec::new(),
        note: Some("经 ChatGPT 订阅号出图（image_generation 工具）。认 size / quality / background / output_format。"),
        fixed_size: None,
    });
    chat.chain(images).collect()
}

/// Cursor 目录 + ChatGPT 目录合并，同名以 ChatGPT 为准。`chatgpt = None` 表示 ChatGPT 通道没号。
pub fn merged_catalog(chatgpt: Option<&[String]>) -> Vec<CatalogEntry> {
    let mut out = catalog();
    if let Some(models) = chatgpt {
        for entry in chatgpt_catalog(models) {
            out.retain(|e| e.id != entry.id);
            out.push(entry);
        }
    }
    out
}

/// 这个名字是不是 Cursor 自家那一族的（含未列出的变体）。
///
/// 命中家族前缀还不够，还要版本号够新：`grok-3-latest` 带 `grok-` 前缀但是 xAI 自己的名字，
/// 得走别名表；`grok-4.6-fast` 才是 Cursor 的变体。版本号解析不出来时**按不是**处理——
/// 让它落到别名表或 `auto`，比发一个上游不认识的名字强。
fn is_cursor_family(name: &str) -> bool {
    for (fam, min_major) in CURSOR_FAMILIES {
        let Some(rest) = name.strip_prefix(fam) else {
            continue;
        };
        let Some(min) = min_major else {
            return true;
        };
        let first = rest.split(['-', '.']).next().unwrap_or("");
        return first.parse::<u32>().is_ok_and(|major| major >= *min);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(m: &str) -> String {
        resolve(m).upstream
    }

    #[test]
    fn cursor_names_pass_through_untouched() {
        for m in CURSOR_MODELS {
            let r = resolve(m);
            assert_eq!(&r.upstream, m);
            assert!(r.note.is_none(), "{m} 不该被改写");
        }
    }

    #[test]
    fn cursor_suffix_variants_pass_through() {
        // reasoning 档位编码在模型名里，擅自改写等于换了个模型。
        for m in [
            "claude-sonnet-5-thinking",
            "claude-opus-5-thinking-max",
            "gpt-5.6-sol-xhigh",
            "grok-4.6-fast",
            "gemini-3.7-flash-high",
        ] {
            assert_eq!(up(m), m, "{m} 应原样透传");
        }
    }

    #[test]
    fn claude_code_default_models_map_to_cursor_names() {
        // Claude Code 真实会发的那些（带日期戳）。
        assert_eq!(up("claude-sonnet-4-5-20250929"), "claude-sonnet-5");
        assert_eq!(up("claude-opus-4-1-20250805"), "claude-opus-5");
        assert_eq!(up("claude-3-5-sonnet-20241022"), "claude-sonnet-5");
        assert_eq!(up("claude-3-7-sonnet-latest"), "claude-sonnet-5");
        assert_eq!(up("claude-3-opus-20240229"), "claude-opus-5");
    }

    #[test]
    fn haiku_goes_to_auto_not_to_a_heavy_model() {
        // Claude Code 拿 haiku 做标题这类小活；给 opus 是浪费，给 auto 让上游挑。
        assert_eq!(up("claude-3-5-haiku-20241022"), "claude-sonnet-5");
        assert_eq!(up("claude-haiku-4-5-20251001"), "auto");
    }

    #[test]
    fn openai_names_map_by_family() {
        assert_eq!(up("gpt-4o"), "gpt-5.6-sol");
        assert_eq!(up("gpt-4o-mini"), "auto");
        assert_eq!(up("gpt-4.1"), "gpt-5.6-sol");
        assert_eq!(up("gpt-4-turbo-2024-04-09"), "gpt-5.6-sol");
        assert_eq!(up("o1-mini"), "auto");
        assert_eq!(up("gpt-3.5-turbo"), "auto");
    }

    #[test]
    fn gpt5_dot_names_are_cursors_own_but_bare_gpt5_is_an_alias() {
        assert_eq!(up("gpt-5.6-sol"), "gpt-5.6-sol", "Cursor 自家的");
        assert_eq!(
            up("gpt-5.9-unknown"),
            "gpt-5.9-unknown",
            "同族新版本交给上游判断"
        );
        assert_eq!(up("gpt-5-codex"), "gpt-5.6-terra", "别家的名字要映射");
    }

    #[test]
    fn google_and_xai_map() {
        assert_eq!(up("gemini-2.5-pro"), "gemini-3.7-flash");
        assert_eq!(up("gemini-3.7-flash"), "gemini-3.7-flash");
        assert_eq!(up("grok-3-latest"), "grok-4.6");
        assert_eq!(up("grok-4.6"), "grok-4.6");
    }

    #[test]
    fn family_prefix_alone_is_not_enough_the_version_has_to_be_ours() {
        // 同一个家族前缀，版本号决定它是 Cursor 的变体还是别家的名字。
        assert_eq!(up("grok-4.6-fast"), "grok-4.6-fast", "Cursor 变体，透传");
        assert_eq!(up("grok-2-1212"), "grok-4.6", "xAI 自己的，映射");
        assert_eq!(up("claude-sonnet-5-thinking"), "claude-sonnet-5-thinking");
        assert_eq!(up("claude-sonnet-4-5-20250929"), "claude-sonnet-5");
        assert_eq!(up("gemini-3.7-flash-high"), "gemini-3.7-flash-high");
        assert_eq!(up("gemini-1.5-pro"), "gemini-3.7-flash");
    }

    #[test]
    fn provider_prefixes_are_stripped() {
        assert_eq!(up("anthropic/claude-sonnet-4-5"), "claude-sonnet-5");
        assert_eq!(up("openai/gpt-4o"), "gpt-5.6-sol");
    }

    #[test]
    fn unknown_models_fall_back_to_auto_with_a_note() {
        let r = resolve("llama-3-70b");
        assert_eq!(r.upstream, "auto");
        assert!(r.note.unwrap().contains("回退 auto"));
    }

    #[test]
    fn empty_and_whitespace_become_auto_silently() {
        assert_eq!(resolve(""), Resolved::passthrough("auto"));
        assert_eq!(resolve("   ").upstream, "auto");
    }

    #[test]
    fn casing_is_ignored() {
        assert_eq!(up("Claude-Sonnet-4-5-20250929"), "claude-sonnet-5");
        assert_eq!(up("AUTO"), "auto");
    }

    #[test]
    fn mapping_notes_only_appear_when_we_changed_something() {
        assert!(resolve("claude-sonnet-5").note.is_none());
        let note = resolve("claude-sonnet-4-5-20250929").note.unwrap();
        assert!(note.contains("claude-sonnet-5"), "{note}");
    }

    #[test]
    fn catalog_covers_every_cursor_model_once_and_groups_variants_under_their_series() {
        let cat = catalog();
        assert_eq!(cat.len(), CURSOR_MODELS.len() + IMAGE_MODELS.len());
        assert!(cat
            .iter()
            .filter(|e| e.modality == "chat")
            .all(|e| CURSOR_MODELS.contains(&e.id.as_str())));
        let by_id = |id: &str| cat.iter().find(|e| e.id == id).unwrap();

        let opus_fast = by_id("claude-opus-5-thinking-max-fast");
        assert_eq!(opus_fast.series, "claude-opus-5");
        assert_eq!(opus_fast.variant, "thinking-max-fast");
        assert_eq!(opus_fast.vendor, "anthropic");

        let sol = by_id("gpt-5.6-sol");
        assert_eq!(sol.series, "gpt-5.6-sol");
        assert_eq!(sol.variant, "standard");
        assert_eq!(by_id("gpt-5.6-sol-max-fast").series, "gpt-5.6-sol");

        assert_eq!(by_id("auto").vendor, "cursor");
        assert!(by_id("auto").note.is_some());
        assert_eq!(by_id("grok-4.6").vendor, "xai");
        assert_eq!(by_id("gemini-3.7-flash").vendor, "google");
        assert_eq!(by_id("gemini-3.7-flash").modality, "chat");
    }

    #[test]
    fn the_image_model_is_listed_under_its_public_name_with_googles_id_as_alias() {
        let cat = catalog();
        let img = cat.iter().find(|e| e.id == "nano-banana-2").unwrap();
        assert_eq!(img.modality, "image");
        assert_eq!(img.vendor, "google");
        assert_eq!(img.variant, "standard");
        assert!(img.aliases.contains(&"gemini-3.1-flash-image"));
        assert!(img.note.is_some_and(|n| n.contains("1536")));
        let v = serde_json::to_value(img).unwrap();
        assert_eq!(v["modality"], "image", "前端按这个字段分到图片会话");

        assert!(is_image_model("nano-banana-2"));
        assert!(is_image_model(" gemini-3.1-flash-image "));
        assert!(!is_image_model("claude-sonnet-5"));
        assert!(!is_image_model(""));
    }

    #[test]
    fn catalog_aliases_are_the_inverse_of_the_alias_table() {
        let cat = catalog();
        let sonnet = cat.iter().find(|e| e.id == "claude-sonnet-5").unwrap();
        assert!(sonnet.aliases.contains(&"claude-3-5-sonnet"));
        assert!(sonnet.aliases.contains(&"claude-sonnet-4"));
        let auto = cat.iter().find(|e| e.id == "auto").unwrap();
        assert!(auto.aliases.contains(&"haiku"));
        assert!(auto.aliases.contains(&"gpt-4o-mini"));
        // 每条别名都要能在目录里找到归宿，否则映射表和广场对不上。
        for (from, to) in ALIASES {
            let entry = cat
                .iter()
                .find(|e| e.id == *to)
                .unwrap_or_else(|| panic!("别名 {from} 映射到不在目录里的 {to}"));
            assert!(entry.aliases.contains(from));
        }
    }

    #[test]
    fn chatgpt_catalog_merges_in_only_when_asked_and_wins_name_clashes() {
        let plain = merged_catalog(None);
        assert_eq!(plain.len(), catalog().len());
        assert!(!plain.iter().any(|e| e.id == "gpt-image-2"));

        let models = vec![
            "gpt-5.4".to_string(),
            "gpt-5.6-sol".to_string(),
            "gpt-7-new".to_string(),
        ];
        let merged = merged_catalog(Some(&models));
        let img = merged.iter().find(|e| e.id == "gpt-image-2").unwrap();
        assert_eq!(img.modality, "image");
        assert_eq!(img.vendor, "openai");
        assert_eq!(
            merged.iter().filter(|e| e.id == "gpt-5.6-sol").count(),
            1,
            "两边都有的只留一条"
        );
        assert!(merged
            .iter()
            .find(|e| e.id == "gpt-5.6-sol")
            .unwrap()
            .note
            .is_some_and(|n| n.contains("ChatGPT")));
        assert!(
            merged.iter().any(|e| e.id == "gpt-7-new"),
            "上游目录里的新模型不用改代码就进广场"
        );
        assert!(
            merged.iter().any(|e| e.id == "nano-banana-2"),
            "Cursor 的出图模型还在"
        );
        assert_eq!(img.fixed_size, None, "gpt-image 认 size，界面该摆规格菜单");
        assert_eq!(
            merged
                .iter()
                .find(|e| e.id == "nano-banana-2")
                .unwrap()
                .fixed_size,
            Some("1536x1024"),
            "Cursor 那条固定尺寸"
        );
        let v = serde_json::to_value(img).unwrap();
        assert!(
            v.get("fixedSize").is_some_and(serde_json::Value::is_null),
            "None 也发出去（null），前端靠键在不在认新老网关"
        );
    }

    #[test]
    fn strip_version_suffix_only_eats_date_stamps() {
        assert_eq!(
            strip_version_suffix("claude-3-5-sonnet-20241022"),
            "claude-3-5-sonnet"
        );
        assert_eq!(
            strip_version_suffix("claude-3-7-sonnet-latest"),
            "claude-3-7-sonnet"
        );
        assert_eq!(
            strip_version_suffix("gpt-5.6-sol"),
            "gpt-5.6-sol",
            "不是日期不能砍"
        );
        assert_eq!(strip_version_suffix("grok-4.6"), "grok-4.6");
    }
}
