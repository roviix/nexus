//! 代理出网模式要写的那三个 Cursor 设置：`remote.SSH.httpProxy` / `httpsProxy` / `noProxy`。
//!
//! ## 为什么必须改这份文件
//!
//! 代理模式的机制不是我们发明的，是 `anysphere.remote-ssh` 扩展自己的：它把这三个设置的值
//! `export HTTP_PROXY=…` 写进**远程 server 的启动脚本**（连同 `SendEnv`），远程会话里的出网
//! 因此走那个地址。除此之外没有别的入口 —— 远程侧没有 `server-env-setup` 这类钩子（实测
//! 扩展的 bootstrap 里根本没有它），所以「让远程经代理出网」只能落在这个设置上。
//!
//! ## 为什么是定点文本编辑而不是 JSON 往返
//!
//! `settings.json` 是 **JSONC**：用户在里面写注释是常态（本机那份就有一条「sand 补丁按版本
//! 硬绑，自动更新会把补丁覆盖掉」——正是关于这个功能的备忘）。`serde_json` 解析再
//! `to_string_pretty` 写回会把注释和缩进全部抹掉：改一个代理端口的代价是毁掉用户的文件，
//! 这个交换任何时候都不成立。所以这里只做三件很小的事：**在顶层插一个键**、**在已有的
//! 对象里插 / 换一个 host 条目**、**把那个条目删掉**，其余字节原样不动。
//!
//! 扫描器认字符串（含转义）和两种注释，所以 `{` / `"` 出现在注释或字符串里不会把它带偏。
//!
//! ## 只写按主机的对象形式
//!
//! 这三个设置都支持两种形状：一个字符串（对所有 host 生效）或一个 `{host: url}` 对象。
//! 我们**只写对象形式、只碰自己那一台**；发现它已经是个全局字符串就报错而不是覆盖 ——
//! 那是用户给所有远程配的代理，替掉它会静默改变别的主机的行为。

use nexus_core::{AppError, ErrorCode, Result};

/// 三个设置的键名。顺序即写入顺序。
pub const HTTP_PROXY_KEY: &str = "remote.SSH.httpProxy";
pub const HTTPS_PROXY_KEY: &str = "remote.SSH.httpsProxy";
pub const NO_PROXY_KEY: &str = "remote.SSH.noProxy";

/// 不该经代理的地址。
///
/// `127.0.0.1` 那一条是必须的：远程 server 自己也往本地打（其中就包括我们**网关模式**改道到
/// `127.0.0.1:<port>` 的那条路），让它经代理会绕成一个环。
pub const NO_PROXY_VALUE: &str = "localhost,127.0.0.1,::1";

/// 一台主机在这份设置里的现状。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProxyConfig {
    /// 这台主机现在配的 http 代理地址（`None` = 没配）。
    pub http: Option<String>,
    pub https: Option<String>,
    pub no_proxy: Option<String>,
}

impl ProxyConfig {
    /// 三个键都指向 `url`（no_proxy 只要有值就算配过）。
    pub fn matches(&self, url: &str) -> bool {
        self.http.as_deref() == Some(url) && self.https.as_deref() == Some(url)
    }
}

/// 读一台主机的代理配置。文件不存在 / 读不动都当「没配」—— 这是只读路径，不该因为
/// 用户的文件坏了就让整个界面报错。
pub fn read(text: &str, host: &str) -> ProxyConfig {
    ProxyConfig {
        http: host_entry(text, HTTP_PROXY_KEY, host),
        https: host_entry(text, HTTPS_PROXY_KEY, host),
        no_proxy: host_entry(text, NO_PROXY_KEY, host),
    }
}

/// 把这台主机的三个键设成 `url`（no_proxy 用 [`NO_PROXY_VALUE`]），返回要写回的文本。
///
/// 已经是目标状态时返回 `None` —— 调用方据此跳过写盘，省掉一次无意义的备份。
pub fn apply(text: &str, host: &str, url: &str) -> Result<Option<String>> {
    let mut out = text.to_string();
    let mut changed = false;
    for (key, value) in [
        (HTTP_PROXY_KEY, url),
        (HTTPS_PROXY_KEY, url),
        (NO_PROXY_KEY, NO_PROXY_VALUE),
    ] {
        let (next, hit) = set_host_entry(&out, key, host, value)?;
        out = next;
        changed |= hit;
    }
    Ok(changed.then_some(out))
}

/// 把这台主机的三个条目删掉；某个键因此空了就连键一起删。没有可删的返回 `None`。
pub fn remove(text: &str, host: &str) -> Result<Option<String>> {
    let mut out = text.to_string();
    let mut changed = false;
    for key in [HTTP_PROXY_KEY, HTTPS_PROXY_KEY, NO_PROXY_KEY] {
        let (next, hit) = remove_host_entry(&out, key, host);
        out = next;
        changed |= hit;
    }
    Ok(changed.then_some(out))
}

// ---------------------------------------------------------------------------
// JSONC 扫描
// ---------------------------------------------------------------------------

/// 一个键值对在文本里的位置。
struct Span {
    /// `"key"` 的起点。
    key_start: usize,
    /// 值的起点（跳过了 `:` 和空白）。
    value_start: usize,
    /// 值的终点（不含）。
    value_end: usize,
}

/// 从 `at` 起跳过空白与注释，返回第一个有内容的下标。
fn skip_trivia(s: &[u8], mut at: usize) -> usize {
    while at < s.len() {
        match s[at] {
            b' ' | b'\t' | b'\r' | b'\n' | b',' => at += 1,
            b'/' if at + 1 < s.len() && s[at + 1] == b'/' => {
                while at < s.len() && s[at] != b'\n' {
                    at += 1;
                }
            }
            b'/' if at + 1 < s.len() && s[at + 1] == b'*' => {
                at += 2;
                while at + 1 < s.len() && !(s[at] == b'*' && s[at + 1] == b'/') {
                    at += 1;
                }
                at = (at + 2).min(s.len());
            }
            _ => break,
        }
    }
    at
}

/// 一个 JSON 字符串从 `at`（必须指向 `"`）到闭引号之后的下标。认 `\"` 转义。
fn end_of_string(s: &[u8], at: usize) -> Option<usize> {
    if s.get(at) != Some(&b'"') {
        return None;
    }
    let mut i = at + 1;
    while i < s.len() {
        match s[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// 一个值（对象 / 数组 / 字符串 / 字面量）从 `at` 到它结束之后的下标。
/// 括号计数会跳过字符串与注释，所以值里出现 `}` 不会提前收尾。
fn end_of_value(s: &[u8], at: usize) -> Option<usize> {
    match s.get(at)? {
        b'"' => end_of_string(s, at),
        b'{' | b'[' => {
            let mut depth = 0i32;
            let mut i = at;
            while i < s.len() {
                match s[i] {
                    b'"' => i = end_of_string(s, i)?,
                    b'/' if i + 1 < s.len() && (s[i + 1] == b'/' || s[i + 1] == b'*') => {
                        i = skip_trivia(s, i);
                    }
                    b'{' | b'[' => {
                        depth += 1;
                        i += 1;
                    }
                    b'}' | b']' => {
                        depth -= 1;
                        i += 1;
                        if depth == 0 {
                            return Some(i);
                        }
                    }
                    _ => i += 1,
                }
            }
            None
        }
        // true / false / null / 数字：到分隔符为止。
        _ => {
            let mut i = at;
            while i < s.len() && !matches!(s[i], b',' | b'}' | b']' | b'\n' | b' ' | b'\t' | b'\r')
            {
                i += 1;
            }
            (i > at).then_some(i)
        }
    }
}

/// 在 `obj_start`（指向 `{`）这个对象里找**直接**子键 `key`。
fn find_entry(s: &[u8], obj_start: usize, key: &str) -> Option<Span> {
    let want = format!("\"{key}\"");
    let mut i = obj_start + 1;
    loop {
        i = skip_trivia(s, i);
        if i >= s.len() || s[i] == b'}' {
            return None;
        }
        let key_start = i;
        let key_end = end_of_string(s, i)?;
        let colon = skip_trivia(s, key_end);
        if s.get(colon) != Some(&b':') {
            return None;
        }
        let value_start = skip_trivia(s, colon + 1);
        let value_end = end_of_value(s, value_start)?;
        if &s[key_start..key_end] == want.as_bytes() {
            return Some(Span {
                key_start,
                value_start,
                value_end,
            });
        }
        i = value_end;
    }
}

/// 顶层 `{` 的下标。
fn root_object(s: &[u8]) -> Option<usize> {
    let at = skip_trivia(s, 0);
    (s.get(at) == Some(&b'{')).then_some(at)
}

/// `obj_start` 指向的对象里一条都没有（只有空白 / 注释）。
fn is_empty_object(s: &[u8], obj_start: usize) -> bool {
    s.get(skip_trivia(s, obj_start + 1)) == Some(&b'}')
}

/// 这个键下这台主机配的值。
fn host_entry(text: &str, key: &str, host: &str) -> Option<String> {
    let s = text.as_bytes();
    let root = root_object(s)?;
    let span = find_entry(s, root, key)?;
    let value = &text[span.value_start..span.value_end];
    if value.as_bytes().first() == Some(&b'{') {
        let inner = find_entry(s, span.value_start, host)?;
        return unquote(&text[inner.value_start..inner.value_end]);
    }
    // 全局字符串形式：对这台主机确实生效，读的时候要如实报出来。
    unquote(value)
}

fn unquote(raw: &str) -> Option<String> {
    serde_json::from_str::<String>(raw.trim()).ok()
}

fn quote(v: &str) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "\"\"".into())
}

/// 猜一份文件用的缩进（顶层第一个键前面那串空白）。猜不到用 4 空格 —— Cursor 自己写的
/// 就是 4 空格。这不影响正确性，只影响插进去的那行看起来是不是和邻居一样。
fn indent_of(text: &str) -> String {
    let s = text.as_bytes();
    if let Some(root) = root_object(s) {
        let after = &text[root + 1..];
        if let Some(line) = after.split('\n').nth(1) {
            let ws: String = line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            if !ws.is_empty() {
                return ws;
            }
        }
    }
    "    ".into()
}

fn conflict(key: &str, current: &str) -> AppError {
    AppError::new(
        ErrorCode::InvalidInput,
        format!("Cursor 里的 `{key}` 是一个对所有远程生效的地址（{current}），没有改动它。"),
    )
    .with_hint(
        "替掉它会静默改变别的远程主机的出网方式。把那一项改成 \
         `{\"主机名\": \"http://…\"}` 的按主机形式，或让本机代理端口与它一致。",
    )
}

/// 设置 `root[key][host] = value`。返回 `(新文本, 是否真的改了)`。
fn set_host_entry(text: &str, key: &str, host: &str, value: &str) -> Result<(String, bool)> {
    let s = text.as_bytes();
    let Some(root) = root_object(s) else {
        return Err(
            AppError::invalid("Cursor 的 settings.json 顶层不是一个对象，没有改动它。")
                .with_hint("把它改成 `{ … }` 的形状再试。"),
        );
    };
    let entry = format!("{}: {}", quote(host), quote(value));

    let Some(span) = find_entry(s, root, key) else {
        // 键不存在：在顶层第一个位置插一整条。插在开头而不是末尾——末尾要判断前一条有没有
        // 逗号、有没有跟着注释，开头只需要紧跟 `{`。
        let ind = indent_of(text);
        // 空对象后面不能跟逗号：`{"a":1,}` 这种尾逗号 Cursor 容得下，`serde_json` 容不下，
        // 而我们写出去的东西必须是**严格合法**的 JSON——用户可能拿别的工具读它。
        let sep = if is_empty_object(s, root) { "" } else { "," };
        let inserted = format!("\n{ind}{}: {{ {entry} }}{sep}", quote(key));
        let mut out = String::with_capacity(text.len() + inserted.len());
        out.push_str(&text[..root + 1]);
        out.push_str(&inserted);
        out.push_str(&text[root + 1..]);
        return Ok((out, true));
    };

    let current = &text[span.value_start..span.value_end];
    if current.as_bytes().first() != Some(&b'{') {
        let shown = unquote(current).unwrap_or_else(|| current.trim().to_string());
        if shown == value {
            // 全局字符串正好就是我们要的值：不算冲突，也不用改。
            return Ok((text.to_string(), false));
        }
        return Err(conflict(key, &shown));
    }

    // 已经是对象：换掉或插入这台主机那一条。
    if let Some(inner) = find_entry(s, span.value_start, host) {
        if text[inner.value_start..inner.value_end].trim() == quote(value) {
            return Ok((text.to_string(), false));
        }
        let mut out = String::with_capacity(text.len() + 16);
        out.push_str(&text[..inner.value_start]);
        out.push_str(&quote(value));
        out.push_str(&text[inner.value_end..]);
        return Ok((out, true));
    }
    let inserted = if is_empty_object(s, span.value_start) {
        format!(" {entry} ")
    } else {
        format!(" {entry},")
    };
    let mut out = String::with_capacity(text.len() + inserted.len());
    out.push_str(&text[..span.value_start + 1]);
    out.push_str(&inserted);
    out.push_str(&text[span.value_start + 1..]);
    Ok((out, true))
}

/// 删掉 `root[key][host]`；对象空了连键一起删。返回 `(新文本, 是否真的改了)`。
fn remove_host_entry(text: &str, key: &str, host: &str) -> (String, bool) {
    let s = text.as_bytes();
    let Some(root) = root_object(s) else {
        return (text.to_string(), false);
    };
    let Some(span) = find_entry(s, root, key) else {
        return (text.to_string(), false);
    };
    // 全局字符串不是我们写的，不动它。
    if s.get(span.value_start) != Some(&b'{') {
        return (text.to_string(), false);
    }
    let Some(inner) = find_entry(s, span.value_start, host) else {
        return (text.to_string(), false);
    };

    // 这台主机是对象里唯一一条 → 整个键删掉，别在用户文件里留一个空对象。
    let only = find_other_entry(s, span.value_start, host).is_none();
    let (cut_from, cut_to) = if only {
        (span.key_start, span.value_end)
    } else {
        (inner.key_start, inner.value_end)
    };

    // 尾随的逗号要一起吃掉，否则留下 `{,` / `,,` 这种读不了的东西。
    let mut to = cut_to;
    while to < s.len() && matches!(s[to], b' ' | b'\t') {
        to += 1;
    }
    let ate_comma = s.get(to) == Some(&b',');
    if ate_comma {
        to += 1;
    }

    // 往前吃掉这一条自己那行的缩进，连行首的换行一起 —— 不然删完会留下一行空白。
    // 「用户自己写的空行」不受影响：只吃紧挨着被删条目的那一个换行。
    let mut from = cut_from;
    while from > 0 && matches!(s[from - 1], b' ' | b'\t') {
        from -= 1;
    }
    if from > 0 && s[from - 1] == b'\n' {
        from -= 1;
        if from > 0 && s[from - 1] == b'\r' {
            from -= 1;
        }
    }

    // 没吃到尾随逗号说明我们是最后一条，得把**前面**那个逗号摘掉。
    if !ate_comma {
        let mut back = from;
        while back > 0 && matches!(s[back - 1], b' ' | b'\t' | b'\n' | b'\r') {
            back -= 1;
        }
        if back > 0 && s[back - 1] == b',' {
            from = back - 1;
        }
    }

    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..from]);
    out.push_str(&text[to..]);
    (out, true)
}

/// 对象里除了 `host` 还有没有别的键。
fn find_other_entry(s: &[u8], obj_start: usize, host: &str) -> Option<usize> {
    let skip = format!("\"{host}\"");
    let mut i = obj_start + 1;
    loop {
        i = skip_trivia(s, i);
        if i >= s.len() || s[i] == b'}' {
            return None;
        }
        let key_start = i;
        let key_end = end_of_string(s, i)?;
        let colon = skip_trivia(s, key_end);
        let value_start = skip_trivia(s, colon + 1);
        let value_end = end_of_value(s, value_start)?;
        if &s[key_start..key_end] != skip.as_bytes() {
            return Some(key_start);
        }
        i = value_end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机那份 settings.json 的形状：带注释、4 空格缩进、已经有一个按主机的 remote.SSH 对象。
    const REAL: &str = r#"{
    "window.commandCenter": true,
    // sand 补丁按版本硬绑，自动更新会把补丁覆盖掉并冲走做 diff 的旧版基线
    "update.mode": "none",
    "remote.SSH.remotePlatform": {
        "devbox-01": "linux"
    },
    "remote.SSH.localServerDownload": "always"
}
"#;

    #[test]
    fn applying_keeps_every_comment_and_only_adds_the_three_keys() {
        let out = apply(REAL, "devbox-01", "http://127.0.0.1:7890")
            .unwrap()
            .expect("首次配置该有改动");
        // 用户的注释和原有设置一字不动 —— 这是这个模块存在的全部理由。
        assert!(
            out.contains("// sand 补丁按版本硬绑"),
            "注释被抹掉了：\n{out}"
        );
        assert!(out.contains(r#""update.mode": "none""#));
        assert!(
            out.contains(r#""devbox-01": "linux""#),
            "别的 remote.SSH 设置不能动"
        );
        assert!(out.contains(r#""remote.SSH.localServerDownload": "always""#));

        let cfg = read(&out, "devbox-01");
        assert_eq!(cfg.http.as_deref(), Some("http://127.0.0.1:7890"));
        assert_eq!(cfg.https.as_deref(), Some("http://127.0.0.1:7890"));
        assert_eq!(cfg.no_proxy.as_deref(), Some(NO_PROXY_VALUE));
        assert!(cfg.matches("http://127.0.0.1:7890"));
        // 写出来的还得是合法 JSON（去掉注释之后）。
        assert!(parses(&out), "写出来的不是合法 JSONC：\n{out}");
    }

    #[test]
    fn applying_twice_is_a_no_op() {
        let once = apply(REAL, "box", "http://127.0.0.1:7890")
            .unwrap()
            .unwrap();
        assert_eq!(
            apply(&once, "box", "http://127.0.0.1:7890").unwrap(),
            None,
            "已经是目标状态就不该再写一遍盘"
        );
    }

    #[test]
    fn changing_the_port_rewrites_only_that_value() {
        let once = apply(REAL, "box", "http://127.0.0.1:7890")
            .unwrap()
            .unwrap();
        let twice = apply(&once, "box", "http://127.0.0.1:1080")
            .unwrap()
            .expect("换端口要改");
        assert_eq!(
            read(&twice, "box").http.as_deref(),
            Some("http://127.0.0.1:1080")
        );
        assert!(twice.contains("// sand 补丁按版本硬绑"));
        assert!(parses(&twice));
    }

    #[test]
    fn a_second_host_joins_the_same_object_without_disturbing_the_first() {
        let a = apply(REAL, "box-a", "http://127.0.0.1:7890")
            .unwrap()
            .unwrap();
        let b = apply(&a, "box-b", "http://127.0.0.1:7891")
            .unwrap()
            .unwrap();
        assert_eq!(
            read(&b, "box-a").http.as_deref(),
            Some("http://127.0.0.1:7890")
        );
        assert_eq!(
            read(&b, "box-b").http.as_deref(),
            Some("http://127.0.0.1:7891")
        );
        assert!(parses(&b));

        // 摘掉一台，另一台必须完好，键也还在。
        let left = remove(&b, "box-a").unwrap().expect("该有改动");
        assert!(read(&left, "box-a").http.is_none());
        assert_eq!(
            read(&left, "box-b").http.as_deref(),
            Some("http://127.0.0.1:7891")
        );
        assert!(parses(&left), "{left}");
    }

    #[test]
    fn removing_the_last_host_drops_the_key_entirely() {
        let on = apply(REAL, "box", "http://127.0.0.1:7890")
            .unwrap()
            .unwrap();
        let off = remove(&on, "box").unwrap().expect("该有改动");
        for key in [HTTP_PROXY_KEY, HTTPS_PROXY_KEY, NO_PROXY_KEY] {
            assert!(!off.contains(key), "空对象该连键一起删：{key}\n{off}");
        }
        // 回到原样（允许空白差异），注释和别的设置都在。
        assert!(off.contains("// sand 补丁按版本硬绑"));
        assert!(off.contains(r#""devbox-01": "linux""#));
        assert!(parses(&off), "{off}");
        assert_eq!(remove(&off, "box").unwrap(), None, "没得删就别报改动");
    }

    /// 全局字符串是用户给**所有**远程配的，替掉它会静默改变别的主机 —— 必须报错。
    #[test]
    fn a_global_string_is_refused_not_overwritten() {
        let text = r#"{
    "remote.SSH.httpProxy": "http://10.0.0.1:3128"
}
"#;
        let err = apply(text, "box", "http://127.0.0.1:7890").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.message.contains("remote.SSH.httpProxy"));
        assert!(err.hint.is_some(), "被拒了要说下一步怎么办");
        // 读的时候要如实报出它对这台主机生效。
        assert_eq!(
            read(text, "box").http.as_deref(),
            Some("http://10.0.0.1:3128")
        );
        // 全局值恰好就是我们要的：那一项不算冲突、也不去动它，但另外两个键还没设，要补上
        // ——https 那一路才是 CONNECT api2 真正走的，缺了代理模式就是半通。
        let out = apply(text, "box", "http://10.0.0.1:3128")
            .unwrap()
            .expect("缺的键要补");
        assert!(
            out.contains(r#""remote.SSH.httpProxy": "http://10.0.0.1:3128""#),
            "用户的全局设置必须原样留着：\n{out}"
        );
        assert_eq!(
            read(&out, "box").https.as_deref(),
            Some("http://10.0.0.1:3128")
        );
        assert_eq!(read(&out, "box").no_proxy.as_deref(), Some(NO_PROXY_VALUE));
        assert!(parses(&out), "{out}");

        // 也不该去删用户的全局设置。
        let off = remove(&out, "box")
            .unwrap()
            .expect("补上的那两个键要能摘掉");
        assert!(
            off.contains(r#""remote.SSH.httpProxy": "http://10.0.0.1:3128""#),
            "全局设置不是我们写的，不能删：\n{off}"
        );
        assert!(read(&off, "box").https.is_none());
        assert!(parses(&off), "{off}");
    }

    /// 大括号 / 引号出现在注释和字符串里，不能把扫描器带偏。
    #[test]
    fn braces_inside_comments_and_strings_do_not_confuse_the_scanner() {
        let text = r#"{
    // 这里有个假的 } 和一个 "键"
    "a": "值里也有 } 和 \" 转义",
    /* 块注释里
       还有 { } */
    "remote.SSH.httpProxy": {
        // 注释夹在对象里
        "other": "http://10.0.0.2:1"
    },
    "b": [1, 2, { "c": 3 }]
}
"#;
        let out = apply(text, "box", "http://127.0.0.1:7890")
            .unwrap()
            .unwrap();
        assert_eq!(
            read(&out, "other").http.as_deref(),
            Some("http://10.0.0.2:1"),
            "原有条目要留着"
        );
        assert_eq!(
            read(&out, "box").http.as_deref(),
            Some("http://127.0.0.1:7890")
        );
        assert!(out.contains("值里也有 } 和 \\\" 转义"));
        assert!(out.contains("// 注释夹在对象里"));
        assert!(out.contains(r#""b": [1, 2, { "c": 3 }]"#));
        assert!(parses(&out), "{out}");
    }

    #[test]
    fn an_empty_or_minimal_file_still_works() {
        for text in ["{}", "{}\n", "{\n}\n"] {
            let out = apply(text, "box", "http://127.0.0.1:7890")
                .unwrap()
                .unwrap_or_else(|| panic!("{text:?} 该能写"));
            assert_eq!(
                read(&out, "box").http.as_deref(),
                Some("http://127.0.0.1:7890"),
                "{out}"
            );
            assert!(parses(&out), "{text:?} → {out}");
            let off = remove(&out, "box").unwrap().unwrap();
            assert!(parses(&off), "{off}");
        }
    }

    #[test]
    fn a_broken_file_is_refused_rather_than_mangled() {
        let err = apply("[]", "box", "http://127.0.0.1:7890").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        // 只读路径不该因为文件坏了就炸。
        assert_eq!(read("not json at all", "box"), ProxyConfig::default());
    }

    #[test]
    fn no_proxy_keeps_loopback_off_the_tunnel() {
        // 网关模式改道到 127.0.0.1:<port>；那条路经代理会绕成一个环。
        assert!(NO_PROXY_VALUE.contains("127.0.0.1"));
        assert!(NO_PROXY_VALUE.contains("localhost"));
    }

    /// 去掉注释之后必须是合法 JSON —— 我们的编辑不能把用户文件写成 Cursor 读不了的东西。
    fn parses(text: &str) -> bool {
        let stripped = strip_comments(text);
        serde_json::from_str::<serde_json::Value>(&stripped).is_ok()
    }

    fn strip_comments(text: &str) -> String {
        let s = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < s.len() {
            match s[i] {
                b'"' => {
                    let end = end_of_string(s, i).unwrap_or(s.len());
                    out.push_str(&text[i..end]);
                    i = end;
                }
                b'/' if i + 1 < s.len() && s[i + 1] == b'/' => {
                    while i < s.len() && s[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if i + 1 < s.len() && s[i + 1] == b'*' => {
                    i += 2;
                    while i + 1 < s.len() && !(s[i] == b'*' && s[i + 1] == b'/') {
                        i += 1;
                    }
                    i = (i + 2).min(s.len());
                }
                _ => {
                    out.push(s[i] as char);
                    i += 1;
                }
            }
        }
        out
    }
}
