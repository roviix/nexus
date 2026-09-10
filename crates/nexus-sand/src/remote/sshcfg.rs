//! `~/.ssh/config` 里那几行 `RemoteForward` 的读与删。
//!
//! ## 现在只用来清理
//!
//! 早期版本有一种「挂在 Cursor 的连接上」的隧道模式：把 `RemoteForward` 写进用户的 ssh config，
//! 让 Cursor 每次连远程时 ssh 自己带起反向转发。想法是生命周期天生对齐、应用不开也成立；实际
//! 上它依赖 sshd / ssh 网关真的执行 `-R`——在容器平台的 ssh 网关上这不成立（见 [`super::tunnel`]
//! 模块说明），而且转发建不起来只在 Cursor 自己的 ssh 输出里留一句 warning，用户永远看不到。
//!
//! 隧道改成 ssh 会话里的多路复用中继之后，这个模式没有了。留下的只有 [`read`]（认出还在不在）和
//! [`remove`]（启动时把它删掉），[`apply`] 保留给测试对照，不再有人调用。
//!
//! ## 编辑纪律
//!
//! 和 Cursor 设置那边（[`super::proxy`]）同一条：**只加自己那几行、别的字节一个不动**。
//! 用一对注释标记把我们的行围起来，移除时按标记精确删掉。
//!
//! 两个真实存在的坑，都在本机那份 config 上：
//!
//! 1. **`Host *` 不能碰。** 那份 config 第 2 行就是 `Host *`（通用设置）。往它里面插一条
//!    `RemoteForward` 等于给**每一台**主机都开这个反向转发 —— 连别人的跳板机都会被塞一个
//!    监听端口。所以只认「模式恰好就是这个别名」的块。
//! 2. **一个块可以挂多个模式**（`Host a b c`）。那种块是共享的，插进去会影响别的主机；
//!    这时候在文件末尾另起一个只有这个别名的块。ssh 会把所有匹配块的选项合起来用，而
//!    `RemoteForward` 是**可累加**的指令，所以另起一块和插在原块里等效。

use nexus_core::{AppError, ErrorCode, Result};

const BEGIN: &str = "# >>> nexus-sand remote-forward >>>";
const END: &str = "# <<< nexus-sand remote-forward <<<";

/// `user@host` 这种形式没法写进 `Host` 模式（ssh 的模式里不能有 `@`），这个模式只支持
/// `~/.ssh/config` 里的别名。
pub fn is_alias(host: &str) -> bool {
    !host.is_empty()
        && !host.contains('@')
        && !host.contains(':')
        && !host.contains(|c: char| c.is_whitespace())
}

fn not_an_alias(host: &str) -> AppError {
    AppError::new(
        ErrorCode::InvalidInput,
        format!("`{host}` 不是 ssh config 里的别名，没法把转发写进配置。"),
    )
    .with_hint(
        "这个模式要往 ~/.ssh/config 的 `Host <别名>` 块里加一行。\
         给这台机器在 config 里起一个别名（Cursor 的 Remote-SSH 也认它），或改用「应用管」那种隧道。",
    )
}

/// 一行 `RemoteForward`。远程 loopback 上的 `remote_port` → 本机 `local_port`。
///
/// 显式写 `127.0.0.1:` 前缀而不是只写端口号：只写端口时 sshd 按 `GatewayPorts` 决定绑哪个地址，
/// 默认是 loopback，但配了 `GatewayPorts yes` 的机器会绑到 `0.0.0.0` —— 那就把本机的网关 / 代理
/// 暴露给整个内网了。
pub fn forward_line(remote_port: u16, local_port: u16) -> String {
    format!("RemoteForward 127.0.0.1:{remote_port} 127.0.0.1:{local_port}")
}

/// 这台主机此刻在 config 里配着的转发（我们写的那一段里的）。`None` = 没写过。
pub fn read(text: &str, host: &str) -> Option<String> {
    let block = find_block(text, host)?;
    let managed = managed_span(&text[block.body_start..block.end])?;
    text[block.body_start + managed.0..block.body_start + managed.1]
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("RemoteForward"))
        .map(str::to_string)
}

/// 写入 / 更新这台主机的转发行。已经是目标状态返回 `None`。
pub fn apply(text: &str, host: &str, remote_port: u16, local_port: u16) -> Result<Option<String>> {
    if !is_alias(host) {
        return Err(not_an_alias(host));
    }
    let want = forward_line(remote_port, local_port);
    if read(text, host).as_deref() == Some(want.as_str()) {
        return Ok(None);
    }
    // 先把旧的那一段摘掉（可能是别的端口），再插新的 —— 免得留下两行互相打架的转发。
    let cleaned = remove(text, host)?.unwrap_or_else(|| text.to_string());

    let Some(block) = find_block(&cleaned, host) else {
        // 没有专属块：在末尾另起一个。
        let mut out = cleaned;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.ends_with("\n\n") && !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("Host {host}\n{BEGIN}\n    {want}\n{END}\n"));
        return Ok(Some(out));
    };

    // 有专属块：插在块首（紧跟 Host 行），这样它离 `Host` 最近、一眼看得见是给谁的。
    let indent = block_indent(&cleaned, &block);
    let inserted = format!("{indent}{BEGIN}\n{indent}{want}\n{indent}{END}\n");
    let mut out = String::with_capacity(cleaned.len() + inserted.len());
    out.push_str(&cleaned[..block.body_start]);
    out.push_str(&inserted);
    out.push_str(&cleaned[block.body_start..]);
    Ok(Some(out))
}

/// 摘掉这台主机的转发行。没有可摘的返回 `None`。
pub fn remove(text: &str, host: &str) -> Result<Option<String>> {
    let Some(block) = find_block(text, host) else {
        return Ok(None);
    };
    let body = &text[block.body_start..block.end];
    let Some((from, to)) = managed_span(body) else {
        return Ok(None);
    };
    let (abs_from, abs_to) = (block.body_start + from, block.body_start + to);

    // 我们自己建的那种块（`Host x` 底下只有这一段）：连 Host 行一起删掉，不留一个空块。
    let rest_of_block = format!("{}{}", &body[..from], &body[to..]);
    let block_is_ours_only = rest_of_block
        .lines()
        .all(|l| l.trim().is_empty() || l.trim_start().starts_with('#'));
    let (mut cut_from, cut_to) = if block_is_ours_only {
        (block.start, block.end)
    } else {
        (abs_from, abs_to)
    };

    // 我们自己建的块前面有一个用于分隔的空行（`apply` 加的），删的时候要一起吃回去，
    // 否则「装上再摘掉」不是逐字还原，文件里会一次次攒空行。只在这一种情况下回吃：
    // 那个空行是我们加的，属于我们那一段。
    if block_is_ours_only {
        let bytes = text.as_bytes();
        while cut_from >= 2 && bytes[cut_from - 1] == b'\n' && bytes[cut_from - 2] == b'\n' {
            cut_from -= 1;
        }
    }

    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..cut_from]);
    out.push_str(&text[cut_to..]);
    Ok(Some(out))
}

// ---------------------------------------------------------------------------
// 扫描
// ---------------------------------------------------------------------------

struct Block {
    /// `Host` 那一行的起点。
    start: usize,
    /// `Host` 行之后（块内容的起点）。
    body_start: usize,
    /// 块结束（下一个 `Host` / `Match` 行的起点，或文件末尾）。
    end: usize,
}

/// 一行是不是 `Host`（关键字大小写不敏感，ssh 就是这样）。返回它的模式列表。
fn host_patterns(line: &str) -> Option<Vec<&str>> {
    let t = line.trim();
    if t.starts_with('#') {
        return None;
    }
    let (kw, rest) = t.split_once(|c: char| c.is_whitespace())?;
    // `Host=x` 这种等号写法 ssh 也认，但极少见；这里只认空白分隔，认不出就当普通行
    // （不认识的行我们一概不碰，这比猜错安全）。
    kw.eq_ignore_ascii_case("Host")
        .then(|| rest.split_whitespace().collect())
}

/// 一行是不是会**结束**当前块：另一个 `Host`，或任何 `Match`。
fn starts_new_block(line: &str) -> bool {
    if host_patterns(line).is_some() {
        return true;
    }
    let t = line.trim();
    !t.starts_with('#')
        && t.split_once(|c: char| c.is_whitespace())
            .map(|(kw, _)| kw.eq_ignore_ascii_case("Match"))
            .unwrap_or(false)
}

/// 找「模式恰好只有这个别名」的块。
///
/// **必须精确匹配**：`Host *` 也能匹配到这台主机，但往它里面写等于给所有主机开转发。
/// 多模式的共享块同理不碰（调用方会另起一个专属块）。
fn find_block(text: &str, host: &str) -> Option<Block> {
    let mut at = 0usize;
    let mut found: Option<Block> = None;
    for line in text.split_inclusive('\n') {
        let line_start = at;
        at += line.len();
        if let Some(patterns) = host_patterns(line) {
            // 上一个块到这里为止。
            if let Some(b) = found.as_mut() {
                if b.end == usize::MAX {
                    b.end = line_start;
                }
                return found;
            }
            if patterns.as_slice() == [host] {
                found = Some(Block {
                    start: line_start,
                    body_start: at,
                    end: usize::MAX,
                });
            }
        } else if starts_new_block(line) {
            if let Some(b) = found.as_mut() {
                if b.end == usize::MAX {
                    b.end = line_start;
                }
                return found;
            }
        }
    }
    if let Some(b) = found.as_mut() {
        if b.end == usize::MAX {
            b.end = text.len();
        }
    }
    found
}

/// 我们那一段（`BEGIN` 行首 → `END` 行尾之后）在 `body` 里的范围。
fn managed_span(body: &str) -> Option<(usize, usize)> {
    let begin = body.find(BEGIN)?;
    // 连这一行的缩进一起吃掉。
    let from = body[..begin].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end_marker = body[begin..].find(END)? + begin;
    let mut to = body[end_marker..]
        .find('\n')
        .map(|i| end_marker + i + 1)
        .unwrap_or(body.len());
    // END 后面若紧跟空行，一起收掉，别在块里留空洞。
    if body[to..].starts_with('\n') {
        to += 1;
    }
    Some((from, to))
}

/// 猜这个块内部用的缩进（照它第一行来）。ssh 不要求缩进，但用户的文件里都有，跟上才不突兀。
fn block_indent(text: &str, block: &Block) -> String {
    let body = &text[block.body_start..block.end];
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let ws: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        return if ws.is_empty() { "    ".into() } else { ws };
    }
    "    ".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份典型 config 的形状（主机名、用户名、IP 都是虚构的）：开头一个 `Host *` 通用块、注释掉的块、多个具体主机、
    /// 主机之间有空行、块里还有注释掉的 ProxyCommand。
    const REAL: &str = "# 通用设置\nHost *\n    ServerAliveInterval 30\n    ServerAliveCountMax 2\n\n#Host *\n#    ProxyCommand nc -X 5 -x 10.0.0.4:7890 %h %p\n\nHost ws2-hhb\n    HostName gw.example.internal\n    Port 22\n    ProxyCommand nc -X 5 -x 10.0.0.2:7890 %h %p\n\n\nHost devbox-01\n    HostName gw.example.internal\n    Port 22\n    User alice.example.ws\n    #ProxyCommand nc -X 5 -x 10.0.0.5:7890 %h %p\n    ProxyCommand nc -X 5 -x 10.0.0.12:7890 %h %p\n\nHost test01\n    HostName 10.0.20.68\n    User root\n";

    #[test]
    fn the_forward_lands_in_the_right_block_and_nothing_else_moves() {
        let out = apply(REAL, "devbox-01", 8788, 8788).unwrap().expect("该写");
        // 只多了我们那三行。
        assert_eq!(
            out.lines().count(),
            REAL.lines().count() + 3,
            "多写了别的行：\n{out}"
        );
        assert_eq!(
            read(&out, "devbox-01").as_deref(),
            Some("RemoteForward 127.0.0.1:8788 127.0.0.1:8788")
        );
        // 缩进跟着块里原有的走。
        assert!(out.contains("    RemoteForward 127.0.0.1:8788 127.0.0.1:8788"));
        // 用户的东西一个字不动。
        for keep in [
            "# 通用设置",
            "    ServerAliveInterval 30",
            "#    ProxyCommand nc -X 5 -x 10.0.0.4:7890 %h %p",
            "    ProxyCommand nc -X 5 -x 10.0.0.12:7890 %h %p",
            "    #ProxyCommand nc -X 5 -x 10.0.0.5:7890 %h %p",
            "Host test01",
        ] {
            assert!(out.contains(keep), "丢了 {keep:?}：\n{out}");
        }
        // 别的主机没有被写进转发 —— 尤其是 `Host *`。
        assert!(read(&out, "ws2-hhb").is_none());
        assert!(read(&out, "*").is_none(), "绝不能写进 Host *");
        assert!(read(&out, "test01").is_none());
    }

    /// `Host *` 也匹配这台主机，但写进去等于给**每一台**主机开反向转发。
    #[test]
    fn a_wildcard_block_is_never_used_even_though_it_matches() {
        let text = "Host *\n    ServerAliveInterval 30\n";
        let out = apply(text, "box", 1, 2).unwrap().unwrap();
        // 新建了专属块，没动 `Host *`。
        assert!(out.contains("Host box"));
        let star_block = out.split("Host box").next().unwrap();
        assert!(
            !star_block.contains("RemoteForward"),
            "转发被写进了 Host * ：\n{out}"
        );
        assert_eq!(
            read(&out, "box").as_deref(),
            Some("RemoteForward 127.0.0.1:1 127.0.0.1:2")
        );
    }

    /// 共享块（`Host a b`）不碰：插进去会连带影响别的主机。
    #[test]
    fn a_shared_block_is_left_alone_and_a_dedicated_one_is_appended() {
        let text = "Host lab-a lab-b\n    User me\n";
        let out = apply(text, "lab-a", 21890, 7890).unwrap().unwrap();
        assert!(
            out.starts_with("Host lab-a lab-b\n    User me\n"),
            "共享块要原样留着：\n{out}"
        );
        assert!(out.contains("Host lab-a\n"));
        assert_eq!(
            read(&out, "lab-a").as_deref(),
            Some("RemoteForward 127.0.0.1:21890 127.0.0.1:7890")
        );
        // 摘掉时把我们建的那个块整个收走。
        let off = remove(&out, "lab-a").unwrap().unwrap();
        assert_eq!(off, text, "该回到一模一样：\n{off}");
    }

    #[test]
    fn changing_the_port_replaces_the_line_instead_of_adding_a_second() {
        let once = apply(REAL, "devbox-01", 8788, 8788).unwrap().unwrap();
        let twice = apply(&once, "devbox-01", 21890, 7890).unwrap().unwrap();
        assert_eq!(
            twice.matches("RemoteForward").count(),
            1,
            "两行转发会互相打架：\n{twice}"
        );
        assert_eq!(
            read(&twice, "devbox-01").as_deref(),
            Some("RemoteForward 127.0.0.1:21890 127.0.0.1:7890")
        );
        assert_eq!(twice.matches(BEGIN).count(), 1);
    }

    #[test]
    fn applying_twice_is_a_no_op_and_removing_restores_the_original() {
        let once = apply(REAL, "devbox-01", 8788, 8788).unwrap().unwrap();
        assert_eq!(apply(&once, "devbox-01", 8788, 8788).unwrap(), None);
        let off = remove(&once, "devbox-01").unwrap().expect("该摘掉");
        assert_eq!(off, REAL, "摘完必须逐字回到原样：\n{off}");
        assert_eq!(remove(&off, "devbox-01").unwrap(), None);
    }

    /// 关键字大小写不敏感（ssh 就是这样），别因为用户写了 `host` 就去新建一个重复块。
    #[test]
    fn the_host_keyword_is_case_insensitive() {
        let text = "host box\n    User me\n";
        let out = apply(text, "box", 1, 2).unwrap().unwrap();
        assert!(!out.contains("Host box\n"), "不该另起一个重复的块：\n{out}");
        assert!(out.contains("    RemoteForward"));
        assert_eq!(remove(&out, "box").unwrap().unwrap(), text);
    }

    /// 反复「装上 → 摘掉」必须逐字还原，不能一次次在文件里攒空行。
    #[test]
    fn install_remove_cycles_do_not_accumulate_blank_lines() {
        let mut text = REAL.to_string();
        for _ in 0..3 {
            let on = apply(&text, "新主机", 8788, 8788).unwrap().unwrap();
            text = remove(&on, "新主机").unwrap().unwrap();
        }
        assert_eq!(text, REAL, "攒下了痕迹：\n{text}");
    }

    /// `Match` 也会结束一个块；我们那一段不能被塞到 Match 后面去。
    #[test]
    fn a_match_line_ends_the_block() {
        let text = "Host box\n    User me\nMatch host other\n    User you\n";
        let out = apply(text, "box", 1, 2).unwrap().unwrap();
        let before_match = out.split("Match host other").next().unwrap();
        assert!(
            before_match.contains("RemoteForward"),
            "该在 Match 之前：\n{out}"
        );
        assert!(out.contains("Match host other\n    User you\n"));
        assert_eq!(remove(&out, "box").unwrap().unwrap(), text);
    }

    #[test]
    fn an_empty_config_gets_a_fresh_block() {
        for text in ["", "\n"] {
            let out = apply(text, "box", 8788, 8788).unwrap().unwrap();
            assert!(out.contains("Host box\n"));
            assert_eq!(
                read(&out, "box").as_deref(),
                Some("RemoteForward 127.0.0.1:8788 127.0.0.1:8788")
            );
        }
    }

    /// `user@host` 写不进 `Host` 模式（ssh 的模式里不能有 `@`）—— 要报错而不是写出一个
    /// 永远匹配不上的块。
    #[test]
    fn a_user_at_host_target_is_refused_with_a_way_out() {
        assert!(!is_alias("me@10.0.0.8"));
        assert!(!is_alias("box:22"));
        assert!(!is_alias(""));
        assert!(is_alias("devbox-01"));
        let err = apply(REAL, "me@10.0.0.8", 1, 2).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.hint.as_deref().unwrap_or_default().contains("别名"));
    }

    /// 转发行必须显式绑 `127.0.0.1`：只写端口时，配了 `GatewayPorts yes` 的 sshd 会绑到
    /// `0.0.0.0`，把本机的网关 / 代理暴露给整个内网。
    #[test]
    fn the_forward_binds_loopback_explicitly() {
        let line = forward_line(8788, 8788);
        assert_eq!(line, "RemoteForward 127.0.0.1:8788 127.0.0.1:8788");
        assert!(!line.contains("RemoteForward 8788"));
    }
}
