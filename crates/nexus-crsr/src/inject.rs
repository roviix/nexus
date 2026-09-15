//! 写入 `applyAuthorization` 的 JS 块。
//!
//! 挂点与 Sand 的 Grok 鉴权是同一处：变量声明之后、`if(t.overrideAuthToken){` 之前。
//! 区别：
//! - 只拦 `agent.v1.*` 和 BackgroundComposer（原生 Agent / IDE 里的 Cloud Agent）
//! - **不**改 `x-cursor-client-type`
//! - **不**改 URL、**不**删 IDE 头
//! - Bearer 换成 `crsr_` 兑出来的短期 JWT；快过期就在补丁里 `yield fetch` 再兑
//!
//! 注入体里满是 `{}`，绝不能过 `format!` 的位置参数。

/// 盘上认装、Sand 互斥、卸载反向，都靠这一串。改字等于认不出旧安装。
pub const AUTH_MARKER: &str = "/*CRSR_AUTH_V1*/";

const METHOD_PREFIX: &str = "applyAuthorization(e,t){return a(this,void 0,void 0,function*(){";
const METHOD_SUFFIX: &str = "if(t.overrideAuthToken){";

/// 3.19.13 两处 `applyAuthorization`（agent-host / always-local）只差变量声明顺序。
pub const VAR_DECLS: &[&str] = &[
    "var n,r,o,s,i,a,l,c,u,d,m,p;", // cursor-agent-host/dist/main.js
    "var n,r,s,o,i,a,l,u,m,c,d,p;", // cursor-always-local/dist/main.js
];

pub fn original(vars: &str) -> String {
    format!("{METHOD_PREFIX}{vars}{METHOD_SUFFIX}")
}

pub fn patched(vars: &str) -> String {
    format!("{METHOD_PREFIX}{vars}{}{METHOD_SUFFIX}", auth_block())
}

/// 桌面端期望命中数：agent-host + always-local 各一处。
pub const EXPECTED_HITS: u32 = 2;

/// 这些 marker 出现 = Agent 面板已经不走原生 `agent.v1 Run`，或挂点已被占用。
pub fn sand_conflict_reason(content: &str) -> Option<&'static str> {
    if content.contains("/*SAND_GROK_BOX_RELAY_AUTH_V1*/")
        || content.contains("/*SAND_GROKBOT_DIRECT_AUTH_V1*/")
        || content.contains("/*SAND_GROKBOT_STREAM_AUTH_V1*/")
    {
        return Some("Sand 的 Grok 鉴权已经占用 applyAuthorization。");
    }
    if content.contains("/*SAND_DIRECT_INFERENCE_STREAM_V1*/")
        || content.contains("/*SAND_MANAGED_LOCAL_ROUTE_V1*/")
        || content.contains("/*SAND_CLIENT_MODE_V1*/")
    {
        return Some("已经装了 Sand 通道，Agent 面板不走原生 Run。");
    }
    None
}

/// 注入块。路径与 Grok 直连凭证同一屋顶（`com.roviix.nexus`），文件名不同。
pub fn auth_block() -> &'static str {
    concat!(
        "/*CRSR_AUTH_V1*/",
        r#"const __crsrSvc=e?.service?.typeName||"";"#,
        r#"if(__crsrSvc.indexOf("agent.v1.")===0||__crsrSvc==="aiserver.v1.BackgroundComposerService"){"#,
        r#"const __crsrFs=require("node:fs"),__crsrPath=require("node:path"),__crsrOs=require("node:os"),"#,
        r#"__crsrFile=process.env.NEXUS_CRSR_CREDENTIAL_FILE||("#,
        r#"process.platform==="win32"?"#,
        r#"__crsrPath.join(process.env.APPDATA||__crsrPath.join(__crsrOs.homedir(),"AppData","Roaming"),"#,
        r#""com.roviix.nexus","crsr-agent-credential.json"):"#,
        r#"__crsrPath.join(__crsrOs.homedir(),"#,
        r#"process.platform==="darwin"?"Library/Application Support":".config","#,
        r#""com.roviix.nexus","crsr-agent-credential.json"));"#,
        r#"let __crsrCred;try{__crsrCred=JSON.parse(__crsrFs.readFileSync(__crsrFile,"utf8"))}catch(__crsrReadErr){__crsrCred=null}"#,
        r#"if(!__crsrCred?.apiKey)throw new Error("#,
        r#""[CRSR_AUTH_NOT_SELECTED] pick a crsr_ account in Nexus");"#,
        r#"if(!__crsrCred.accessToken||!__crsrCred.expiresAtMs||Date.now()>__crsrCred.expiresAtMs-12e4){"#,
        r#"try{const __crsrResp=yield fetch("https://api2.cursor.sh/auth/exchange_user_api_key","#,
        r#"{method:"POST",headers:{"content-type":"application/json","authorization":"Bearer "+__crsrCred.apiKey},"#,
        r#"body:"{}"}),__crsrBody=yield __crsrResp.json(),"#,
        r#"__crsrTok=__crsrBody&&(__crsrBody.accessToken||__crsrBody.access_token);"#,
        r#"if(__crsrTok){__crsrCred.accessToken=__crsrTok;try{const __crsrP=__crsrTok.split(".")[1],"#,
        r#"__crsrJ=JSON.parse(Buffer.from(__crsrP.replace(/-/g,"+").replace(/_/g,"/"),"base64").toString("utf8"));"#,
        r#"__crsrCred.expiresAtMs=typeof __crsrJ.exp==="number"?__crsrJ.exp*1e3:Date.now()+36e5}"#,
        r#"catch(__crsrExpErr){__crsrCred.expiresAtMs=Date.now()+36e5}"#,
        r#"__crsrCred.renewedAtMs=Date.now();"#,
        r#"__crsrFs.writeFileSync(__crsrFile,JSON.stringify(__crsrCred,null,2)+"\n")}}"#,
        r#"catch(__crsrErr){}}"#,
        r#"if(!__crsrCred.accessToken)throw new Error("[CRSR_AUTH_TOKEN_MISSING] exchange failed");"#,
        r#"e.header.set("Authorization",`Bearer ${__crsrCred.accessToken}`);return}"#,
    )
}

pub fn count_hits(content: &str) -> u32 {
    let mut n = 0;
    for vars in VAR_DECLS {
        if content.contains(&patched(vars)) {
            n += 1;
        }
    }
    n
}

pub fn count_anchors(content: &str) -> u32 {
    let mut n = 0;
    for vars in VAR_DECLS {
        let orig = original(vars);
        if content.contains(&orig) || content.contains(&patched(vars)) {
            n += 1;
        }
    }
    n
}

pub fn apply(content: &str) -> Option<String> {
    let mut out = content.to_string();
    let mut hit = false;
    for vars in VAR_DECLS {
        let orig = original(vars);
        let next = patched(vars);
        if out.contains(&next) {
            continue;
        }
        if out.contains(&orig) {
            out = out.replace(&orig, &next);
            hit = true;
        }
    }
    hit.then_some(out)
}

pub fn remove(content: &str) -> Option<String> {
    if !content.contains(AUTH_MARKER) {
        return None;
    }
    let mut out = content.to_string();
    let mut hit = false;
    for vars in VAR_DECLS {
        let orig = original(vars);
        let next = patched(vars);
        if out.contains(&next) {
            out = out.replace(&next, &orig);
            hit = true;
        }
    }
    hit.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_carries_marker_and_does_not_touch_sand() {
        let b = auth_block();
        assert!(b.starts_with(AUTH_MARKER));
        assert!(b.contains("agent.v1."));
        assert!(b.contains("BackgroundComposerService"));
        assert!(b.contains("exchange_user_api_key"));
        assert!(b.contains("crsr-agent-credential.json"));
        assert!(b.contains("CRSR_AUTH_NOT_SELECTED"));
        assert!(!b.contains("x-cursor-client-type"));
        assert!(!b.contains("InferenceService"));
        assert!(!b.contains("header.delete"));
        assert!(!b.contains(",,"));
        assert!(b.ends_with("return}"));
    }

    #[test]
    fn apply_remove_roundtrips_both_var_decl_orders() {
        let stock = format!(
            "pre {} mid {} post",
            original(VAR_DECLS[0]),
            original(VAR_DECLS[1])
        );
        let patched_all = apply(&stock).expect("should hit");
        assert_eq!(count_hits(&patched_all), 2);
        assert!(sand_conflict_reason(&patched_all).is_none());
        let back = remove(&patched_all).expect("should reverse");
        assert_eq!(back, stock);
        assert!(apply(&patched_all).is_none(), "idempotent");
        assert!(remove(&stock).is_none());
    }

    #[test]
    fn sand_markers_are_a_conflict() {
        assert!(sand_conflict_reason("/*SAND_CLIENT_MODE_V1*/").is_some());
        assert!(sand_conflict_reason("/*SAND_GROK_BOX_RELAY_AUTH_V1*/").is_some());
        assert!(sand_conflict_reason("plain applyAuthorization").is_none());
    }

    #[test]
    fn block_is_syntactically_valid_js() {
        let Ok(node) = which_node() else {
            eprintln!("no node on PATH; skipping syntax check");
            return;
        };
        let src = format!(
            "function* g(e,t){{var n,r,o,s,i,a,l,c,u,d,m,p;{block}if(t.overrideAuthToken){{}}}}",
            block = auth_block()
        );
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("block.js");
        std::fs::write(&file, src).unwrap();
        let out = std::process::Command::new(node)
            .args(["--check", file.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "node --check: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn which_node() -> Result<std::path::PathBuf, ()> {
        let paths = std::env::var_os("PATH").ok_or(())?;
        std::env::split_paths(&paths)
            .map(|d| d.join("node"))
            .chain([
                std::path::PathBuf::from("/opt/homebrew/bin/node"),
                std::path::PathBuf::from("/usr/local/bin/node"),
            ])
            .find(|p| p.is_file())
            .ok_or(())
    }
}
