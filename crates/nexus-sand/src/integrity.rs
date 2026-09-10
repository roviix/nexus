//! Cursor 自己的完整性校验，改了 bundle 就得同步，否则启动报「安装已损坏」。两处：
//!
//! 1. `extensionHostProcess.js` 内嵌了每个内置扩展 `main.js` 的 sha256（hex）；
//! 2. `product.json` 的 `checksums` 记着 `out/` 下若干文件的 sha256（base64、去 `=`）。
//!
//! 与 Python 版的差别：product.json 这里做**文本级值替换**而不是重新序列化——保住原文件的
//! 键序、缩进、BOM，diff 只剩被改的那几个值。

use base64::Engine;
use nexus_core::{AppError, ErrorCode, Result};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn sha256_hex(data: &[u8]) -> String {
    let d = Sha256::digest(data);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// product.json 用的形式：base64(sha256) 去掉尾部 `=`。
pub fn product_checksum(data: &[u8]) -> String {
    let d = Sha256::digest(data);
    base64::engine::general_purpose::STANDARD
        .encode(d)
        .trim_end_matches('=')
        .to_string()
}

/// 把 `changed` 里每个扩展新 `main.js` 的 sha256 写进 `extensionHostProcess.js` 内容。
/// 返回新内容；没有任何变化时返回 `None`。
pub fn update_extension_hashes(
    ext_host: &str,
    changed: &[(&str, &[u8])],
) -> Result<Option<String>> {
    let mut out = ext_host.to_string();
    for (ext, main_js) in changed {
        let id = format!("anysphere.{ext}");
        if !out.contains(&format!("\"{id}\"")) {
            continue;
        }
        let digest = sha256_hex(main_js);
        let re = Regex::new(&format!(
            r#"("{}"\s*:\s*\{{[\s\S]{{0,2400}}?"main\.js"\s*:\s*")[0-9a-f]{{64}}(")"#,
            regex::escape(&id)
        ))
        .map_err(|e| AppError::internal(format!("扩展 hash 正则构造失败：{e}")))?;
        let mut hits = 0;
        out = re
            .replacen(&out, 1, |c: &regex::Captures<'_>| {
                hits += 1;
                format!("{}{digest}{}", &c[1], &c[2])
            })
            .into_owned();
        if hits > 1 {
            return Err(AppError::new(
                ErrorCode::SandIntegrity,
                format!("{id} 的内嵌 main.js 哈希不唯一。"),
            ));
        }
    }
    Ok((out != ext_host).then_some(out))
}

/// 校验：每个扩展 `main.js` 的实际 sha256 与内嵌值一致。
pub fn verify_extension_hashes(ext_host: &str, actual: &[(&str, &[u8])]) -> Result<()> {
    for (ext, main_js) in actual {
        let id = format!("anysphere.{ext}");
        if !ext_host.contains(&format!("\"{id}\"")) {
            continue;
        }
        let re = Regex::new(&format!(
            r#""{}"\s*:\s*\{{[\s\S]{{0,2400}}?"main\.js"\s*:\s*"([0-9a-f]{{64}})""#,
            regex::escape(&id)
        ))
        .map_err(|e| AppError::internal(format!("扩展 hash 正则构造失败：{e}")))?;
        if let Some(c) = re.captures(ext_host) {
            if c[1] != sha256_hex(main_js) {
                return Err(AppError::new(
                    ErrorCode::SandIntegrity,
                    format!("{id} 的内嵌哈希校验失败。"),
                ));
            }
        }
    }
    Ok(())
}

/// 重算 `product.json` 的 `checksums`。`planned` 是「即将写入」的文件内容（键为绝对路径），
/// 不在其中的文件按磁盘现状算。返回新的 product.json 字节；无变化时 `None`。
pub fn sync_product_checksums(
    product_json: &[u8],
    out_root: &Path,
    planned: &HashMap<PathBuf, Vec<u8>>,
) -> Result<Option<Vec<u8>>> {
    let (bom, body) = split_bom(product_json);
    let text = std::str::from_utf8(body)
        .map_err(|_| AppError::new(ErrorCode::SandIntegrity, "product.json 不是 UTF-8。"))?;
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| {
        AppError::new(
            ErrorCode::SandIntegrity,
            format!("product.json 无法解析：{e}"),
        )
    })?;
    let Some(checksums) = json.get("checksums").and_then(|v| v.as_object()) else {
        return Ok(None);
    };
    let out_root = out_root.canonicalize().unwrap_or(out_root.to_path_buf());

    let mut next = text.to_string();
    let mut changed = false;
    for (key, old) in checksums {
        let Some(old) = old.as_str() else { continue };
        let rel: PathBuf = key.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        let target = out_root.join(rel);
        let target = target.canonicalize().unwrap_or(target);
        if !target.starts_with(&out_root) {
            return Err(AppError::new(
                ErrorCode::SandIntegrity,
                format!("product.json checksum 路径逃逸：{key}"),
            ));
        }
        let data = match planned.get(&target) {
            Some(bytes) => bytes.clone(),
            None if target.is_file() => std::fs::read(&target)?,
            None => continue,
        };
        let digest = product_checksum(&data);
        if digest == old {
            continue;
        }
        // 键里的 `/` 在 JSON 源里可能写成 `\/`，两种都认。
        let key_pat = regex::escape(key).replace('/', r"\\?/");
        let re = Regex::new(&format!(
            r#"("{key_pat}"\s*:\s*"){}(")"#,
            regex::escape(old)
        ))
        .map_err(|e| AppError::internal(format!("checksum 正则构造失败：{e}")))?;
        let replaced = re.replacen(&next, 1, |c: &regex::Captures<'_>| {
            format!("{}{digest}{}", &c[1], &c[2])
        });
        if replaced == next {
            return Err(AppError::new(
                ErrorCode::SandIntegrity,
                format!("product.json 里找不到 checksum 条目 {key} 的原文。"),
            ));
        }
        next = replaced.into_owned();
        changed = true;
    }
    if !changed {
        return Ok(None);
    }
    let mut bytes = bom.to_vec();
    bytes.extend_from_slice(next.as_bytes());
    Ok(Some(bytes))
}

/// 校验 product.json 的 checksums 与磁盘一致。返回校验过的条目数。
pub fn verify_product_checksums(product_json: &[u8], out_root: &Path) -> Result<u32> {
    let (_, body) = split_bom(product_json);
    let json: serde_json::Value = serde_json::from_slice(body).map_err(|e| {
        AppError::new(
            ErrorCode::SandIntegrity,
            format!("product.json 无法解析：{e}"),
        )
    })?;
    let Some(checksums) = json.get("checksums").and_then(|v| v.as_object()) else {
        return Ok(0);
    };
    let out_root = out_root.canonicalize().unwrap_or(out_root.to_path_buf());
    let mut checked = 0;
    for (key, want) in checksums {
        let Some(want) = want.as_str() else { continue };
        let rel: PathBuf = key.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        let target = out_root.join(rel);
        let target = target.canonicalize().unwrap_or(target);
        if !target.starts_with(&out_root) || !target.is_file() {
            continue;
        }
        checked += 1;
        if product_checksum(&std::fs::read(&target)?) != want {
            return Err(AppError::new(
                ErrorCode::SandIntegrity,
                format!("product.json 完整性哈希校验失败：{key}"),
            ));
        }
    }
    Ok(checked)
}

fn split_bom(data: &[u8]) -> (&[u8], &[u8]) {
    match data.strip_prefix(b"\xef\xbb\xbf") {
        Some(rest) => (&data[..3], rest),
        None => (&[], data),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_checksum_matches_python_form() {
        // python: base64.b64encode(sha256(b"abc").digest()).rstrip(b"=")
        assert_eq!(
            product_checksum(b"abc"),
            "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn extension_hash_is_rewritten_in_place_once() {
        let host = r#"{"anysphere.cursor-agent-host":{"x":1,"main.js":"0000000000000000000000000000000000000000000000000000000000000000"}}"#;
        let out = update_extension_hashes(host, &[("cursor-agent-host", b"new")])
            .unwrap()
            .unwrap();
        assert!(out.contains(&sha256_hex(b"new")));
        assert!(!out.contains("0000000000000000"));
        // 已一致 → None
        assert!(
            update_extension_hashes(&out, &[("cursor-agent-host", b"new")])
                .unwrap()
                .is_none()
        );
        // 未列出的扩展不动
        assert!(
            update_extension_hashes(host, &[("cursor-agent-exec", b"x")])
                .unwrap()
                .is_none()
        );
        verify_extension_hashes(&out, &[("cursor-agent-host", b"new")]).unwrap();
        assert_eq!(
            verify_extension_hashes(&out, &[("cursor-agent-host", b"other")])
                .unwrap_err()
                .code,
            ErrorCode::SandIntegrity
        );
    }

    #[test]
    fn product_checksums_are_replaced_textually_preserving_layout_and_bom() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(out.join("vs")).unwrap();
        let f = out.join("vs/a.js");
        std::fs::write(&f, b"old").unwrap();
        let old_sum = product_checksum(b"old");
        let src = format!(
            "\u{feff}{{\n\t\"name\": \"Cursor\",\n\t\"checksums\": {{\n\t\t\"vs/a.js\": \"{old_sum}\"\n\t}}\n}}"
        );
        let mut planned = HashMap::new();
        planned.insert(f.canonicalize().unwrap(), b"new".to_vec());
        let next = sync_product_checksums(src.as_bytes(), &out, &planned)
            .unwrap()
            .unwrap();
        let text = String::from_utf8(next.clone()).unwrap();
        assert!(text.starts_with('\u{feff}'), "BOM 要保留");
        assert!(
            text.contains("\n\t\"name\": \"Cursor\""),
            "缩进与键序要保留"
        );
        assert!(text.contains(&product_checksum(b"new")));
        assert!(!text.contains(&old_sum));
        // 磁盘还是 old → verify 失败；写入 new 后通过
        assert_eq!(
            verify_product_checksums(&next, &out).unwrap_err().code,
            ErrorCode::SandIntegrity
        );
        std::fs::write(&f, b"new").unwrap();
        assert_eq!(verify_product_checksums(&next, &out).unwrap(), 1);
        // 已一致 → None
        assert!(sync_product_checksums(&next, &out, &HashMap::new())
            .unwrap()
            .is_none());
    }

    #[test]
    fn checksum_path_escape_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(dir.path().join("secret"), b"x").unwrap();
        let src = r#"{"checksums":{"../secret":"AAAA"}}"#;
        let err = sync_product_checksums(src.as_bytes(), &out, &HashMap::new()).unwrap_err();
        assert_eq!(err.code, ErrorCode::SandIntegrity);
    }
}
