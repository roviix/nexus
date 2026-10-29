//! 正文里泄漏的 `<invoke name="…">` 捞回成工具调用。
//!
//! 只认这次声明过的工具名。写在代码块或引号里的标签当正文，不执行。

#[cfg(test)]
pub enum InvokeScan {
    None,
    /// 开标签已经出现，闭标签还没到。
    Partial,
    Block {
        start: usize,
        end: usize,
        name: String,
        input: String,
    },
}

const QUOTE_CHARS: &[u8] = b"`\"'\\#!@$%^&*()-_=+[]{};:<>,.?/";

#[cfg(test)]
pub fn scan(buf: &str) -> InvokeScan {
    let Some(start) = find_invoke_start(buf) else {
        return InvokeScan::None;
    };
    let Some(end) = find_invoke_block_end(buf, start) else {
        return InvokeScan::Partial;
    };
    match parse_invoke_block(&buf[start..end]) {
        Some((name, input)) => InvokeScan::Block {
            start,
            end,
            name,
            input,
        },
        None => InvokeScan::Partial,
    }
}

pub const STRAY_TOKENS: &[&str] = &["call", "count", "card"];
/// 行首未闭合的 `<invoke` 最多扣这么多字节，再长就当正文放行，避免整条流被卡死。
pub const MAX_HOLD: usize = 262_144;

pub fn find_start(buf: &str) -> Option<usize> {
    find_invoke_start(buf)
}

pub fn block_end(buf: &str, start: usize) -> Option<usize> {
    find_invoke_block_end(buf, start)
}

pub fn parse_block(block: &str) -> Option<(String, String)> {
    parse_invoke_block(block)
}

/// 剥掉块前文本尾部独占一行的 `call` / `count` / `card`，保留前一行的换行。
pub fn strip_stray_tail(before: &str) -> &str {
    let mut end = before.len();
    loop {
        let bytes = before.as_bytes();
        let mut e = end;
        while e > 0 && (bytes[e - 1] == b'\n' || bytes[e - 1] == b'\r') {
            e -= 1;
        }
        let line_start = before[..e].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let last_line = before[line_start..e].trim();
        if STRAY_TOKENS.contains(&last_line) {
            end = line_start;
            if end == 0 {
                return "";
            }
        } else {
            break;
        }
    }
    &before[..end]
}

/// 真泄漏的 `<invoke>` 在行首；嵌在句子中间的是在讨论这个标签。
pub fn looks_like_real_leak(before: &str) -> bool {
    let trimmed = before.trim_end_matches([' ', '\t']);
    trimmed.is_empty() || trimmed.ends_with('\n') || trimmed.ends_with('\r')
}

pub fn advance_fence(open: &mut bool, partial: &mut String, text: &str) {
    for ch in text.chars() {
        if ch == '\n' {
            if partial.trim_start().starts_with("```") {
                *open = !*open;
            }
            partial.clear();
        } else {
            partial.push(ch);
        }
    }
}

pub fn fence_open_after(open: bool, partial: &str, text: &str) -> bool {
    let mut o = open;
    let mut p = partial.to_string();
    advance_fence(&mut o, &mut p, text);
    o
}

/// 缓冲区末尾没闭合的 `<…` 可能是被切开的开标签，先留着。
pub fn partial_open_suffix_len(buf: &str) -> usize {
    if let Some(lt) = buf.rfind('<') {
        if !buf[lt..].contains('>') {
            return buf.len() - lt;
        }
    }
    0
}

fn is_quote_char(buffer: &str, pos: usize) -> bool {
    buffer
        .as_bytes()
        .get(pos)
        .is_some_and(|c| QUOTE_CHARS.contains(c))
}

fn open_tag_lt_pos(buffer: &str, name_pos: usize) -> Option<usize> {
    let bytes = buffer.as_bytes();
    if name_pos == 0 {
        return None;
    }
    let prev = bytes[name_pos - 1];
    if prev == b'<' {
        return Some(name_pos - 1);
    }
    if prev == b':' {
        let i = name_pos - 1;
        let mut j = i;
        while j > 0 && {
            let c = bytes[j - 1];
            c.is_ascii_alphanumeric() || c == b'_'
        } {
            j -= 1;
        }
        if j < i && j > 0 && bytes[j - 1] == b'<' {
            return Some(j - 1);
        }
    }
    None
}

fn find_invoke_start(buffer: &str) -> Option<usize> {
    let mut search = 0;
    while let Some(rel) = buffer[search..].find("invoke") {
        let name_pos = search + rel;
        if let Some(lt) = open_tag_lt_pos(buffer, name_pos) {
            let after = name_pos + "invoke".len();
            let next_ok = buffer
                .as_bytes()
                .get(after)
                .is_none_or(|c| c.is_ascii_whitespace() || *c == b'>' || *c == b'/');
            let quoted = lt > 0 && is_quote_char(buffer, lt - 1);
            if next_ok && !quoted {
                return Some(lt);
            }
        }
        search = name_pos + "invoke".len();
    }
    None
}

fn find_invoke_block_end(buffer: &str, start: usize) -> Option<usize> {
    let boundary = find_next_invoke_open(buffer, start).unwrap_or(buffer.len());
    find_last_invoke_close(buffer, start, boundary)
}

fn find_next_invoke_open(buffer: &str, start: usize) -> Option<usize> {
    let after_open = start + buffer[start..].find('>')? + 1;
    let region = &buffer[after_open..];
    let mut search = 0usize;
    while let Some(rel) = region[search..].find("invoke") {
        let name_pos = search + rel;
        if let Some(lt) = open_tag_lt_pos(region, name_pos) {
            let after = name_pos + "invoke".len();
            let next_ok = region
                .as_bytes()
                .get(after)
                .is_none_or(|c| c.is_ascii_whitespace() || *c == b'>' || *c == b'/');
            if next_ok {
                return Some(after_open + lt);
            }
        }
        search = name_pos + "invoke".len();
    }
    None
}

fn find_last_invoke_close(buffer: &str, from: usize, boundary: usize) -> Option<usize> {
    let region_end = boundary.min(buffer.len());
    if from >= region_end {
        return None;
    }
    let region = &buffer[from..region_end];
    let bytes = region.as_bytes();
    let mut search = 0usize;
    let mut last = None;
    while let Some(rel) = region[search..].find("invoke>") {
        let name_pos = search + rel;
        // 按字节比：`name_pos - 2` 可能落在一个汉字中间，按 `&str` 切会 panic，整条流跟着断掉。
        if name_pos >= 2 && &bytes[name_pos - 2..name_pos] == b"</" {
            last = Some(from + name_pos + "invoke>".len());
        } else if name_pos >= 1 && bytes[name_pos - 1] == b':' {
            let mut j = name_pos - 1;
            while j > 0 && {
                let c = bytes[j - 1];
                c.is_ascii_alphanumeric() || c == b'_'
            } {
                j -= 1;
            }
            if j >= 2 && &bytes[j - 2..j] == b"</" {
                last = Some(from + name_pos + "invoke>".len());
            }
        }
        search = name_pos + "invoke>".len();
    }
    last
}

fn extract_name_attr(tag: &str) -> Option<String> {
    let needle = "name=\"";
    let rel = tag.find(needle)?;
    let start = rel + needle.len();
    let end_rel = tag[start..].find('"')?;
    let name = &tag[start..start + end_rel];
    (!name.is_empty()).then(|| name.to_string())
}

fn parse_invoke_block(block: &str) -> Option<(String, String)> {
    let open_end = block.find('>')?;
    let tool_name = extract_name_attr(&block[..=open_end])?;
    let mut map = serde_json::Map::new();
    let body = &block[open_end + 1..];
    let mut cursor = 0usize;
    while let Some(rel) = body[cursor..].find("parameter name=\"") {
        let name_kw = cursor + rel;
        if open_tag_lt_pos(body, name_kw).is_none() {
            cursor = name_kw + "parameter".len();
            continue;
        }
        let tag_gt = name_kw + body[name_kw..].find('>')?;
        let key = extract_name_attr(&body[name_kw..=tag_gt])?;
        let val_start = tag_gt + 1;
        let (close_start, close_end) = find_param_close(body, val_start)?;
        map.insert(
            key,
            serde_json::Value::String(body[val_start..close_start].to_string()),
        );
        cursor = close_end;
    }
    let input = serde_json::to_string(&serde_json::Value::Object(map)).ok()?;
    Some((tool_name, input))
}

fn find_param_close(body: &str, from: usize) -> Option<(usize, usize)> {
    let boundary = find_next_param_open(body, from).unwrap_or(body.len());
    let region = &body[from..boundary];
    let kw = "parameter>";
    let bytes = region.as_bytes();
    let mut last = None;
    let mut search = 0usize;
    while let Some(rel) = region[search..].find(kw) {
        let name_pos = search + rel;
        if name_pos >= 2 && &bytes[name_pos - 2..name_pos] == b"</" {
            last = Some((from + name_pos - 2, from + name_pos + kw.len()));
        } else if name_pos >= 1 && bytes[name_pos - 1] == b':' {
            let mut j = name_pos - 1;
            while j > 0 && {
                let c = bytes[j - 1];
                c.is_ascii_alphanumeric() || c == b'_'
            } {
                j -= 1;
            }
            if j >= 2 && &bytes[j - 2..j] == b"</" {
                last = Some((from + j - 2, from + name_pos + kw.len()));
            }
        }
        search = name_pos + kw.len();
    }
    last
}

fn find_next_param_open(body: &str, from: usize) -> Option<usize> {
    let mut search = from;
    while let Some(rel) = body[search..].find("parameter name=\"") {
        let kw_pos = search + rel;
        if let Some(lt) = open_tag_lt_pos(body, kw_pos) {
            return Some(lt);
        }
        search = kw_pos + "parameter".len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_prefixed_invoke_and_ignores_a_quoted_one() {
        let buf = "see `<invoke name=\"nope\">` then <antml:invoke name=\"read_file\"><parameter name=\"path\">a.rs</parameter></antml:invoke> tail";
        match scan(buf) {
            InvokeScan::Block {
                start,
                end,
                name,
                input,
            } => {
                assert!(start < end);
                assert_eq!(name, "read_file");
                assert_eq!(input, r#"{"path":"a.rs"}"#);
            }
            _ => panic!("expected a block"),
        }
    }
}
