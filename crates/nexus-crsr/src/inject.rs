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

use regex::Regex;
use std::sync::LazyLock;

/// 盘上认装、Sand 互斥、卸载反向，都靠这一串。改字等于认不出旧安装。
pub const AUTH_MARKER: &str = "/*CRSR_AUTH_V1*/";
pub const TRANSPORT_MARKER: &str = "/*CRSR_TRANSPORT_V1*/";

/// 桌面端期望命中数：agent-host + always-local 各两处（auth + transport）。
pub const EXPECTED_HITS: u32 = 4;

/// 保留给测试与兼容分析。
pub const VAR_DECLS: &[&str] = &[
    "var r,n,s,o,i,a,c,l,u,d,p,h;", // cursor-agent-host/dist/main.js
    "var r,n,s,i,o,a,u,c,l,m,d,f;", // cursor-always-local/dist/main.js
];

/// 匹配未打补丁的 applyAuthorization 函数头与变量声明。
/// Capture 1: req param (如 `e` 或 `t`)
/// Capture 2: opts param (如 `t` 或 `e`)
/// Capture 3: 变量声明 (如 `var ...;`，若混淆变动则可能为空)
/// Capture 4: 后续判断 `if(<opts>.overrideAuthToken){`
static UNPATCHED_AUTH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"applyAuthorization\(([a-zA-Z0-9_$]+),([a-zA-Z0-9_$]+)\)\{return a\(this,void 0,void 0,function\*\(\)\{((?:var [^;]+;)?)(if\([a-zA-Z0-9_$]+\.overrideAuthToken\)\{)").unwrap()
});

/// 匹配已打补丁的 applyAuthorization。
/// Capture 1: req param
/// Capture 2: opts param
/// Capture 3: 变量声明
/// Capture 4: 后续判断 `if(<opts>.overrideAuthToken){`
static PATCHED_AUTH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"applyAuthorization\(([a-zA-Z0-9_$]+),([a-zA-Z0-9_$]+)\)\{return a\(this,void 0,void 0,function\*\(\)\{((?:var [^;]+;)?)/\*(?:CRSR|NEXUS)_AUTH_V1\*/[\s\S]*?return\}(if\([a-zA-Z0-9_$]+\.overrideAuthToken\)\{)").unwrap()
});

/// 匹配未打补丁的 _getTransportForService 方法入口。
/// Capture 1: service param (如 `t` 或 `e`)
/// Capture 2: method param (如 `e` 或 `t`)
/// Capture 3: bool/opts param (如 `r`)
static UNPATCHED_TRANSPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"_getTransportForService\(([a-zA-Z0-9_$]+),([a-zA-Z0-9_$]+),([a-zA-Z0-9_$]+)=!0\)\{let n;").unwrap()
});

/// 匹配已打补丁的 _getTransportForService。
static PATCHED_TRANSPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"_getTransportForService\(([a-zA-Z0-9_$]+),([a-zA-Z0-9_$]+),([a-zA-Z0-9_$]+)=!0\)\{/\*(?:CRSR|NEXUS)_TRANSPORT_V1\*/[\s\S]*?\}catch\(__cErr\)\{\}let n;").unwrap()
});

pub fn original(vars: &str) -> String {
    format!("applyAuthorization(e,t){{return a(this,void 0,void 0,function*(){{{vars}if(t.overrideAuthToken){{")
}

pub fn patched(vars: &str) -> String {
    format!(
        "applyAuthorization(e,t){{return a(this,void 0,void 0,function*(){{{vars}{}if(t.overrideAuthToken){{",
        auth_block("e")
    )
}

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

/// 写入 applyAuthorization 的鉴权 JS 块。
/// 注意：绝不直接改写 __crsrReq.url，以防止 Connect-ES HTTP/2 出现 origin mismatch 崩溃。
pub fn auth_block(req_var: &str) -> String {
    let req = req_var;
    format!(
        concat!(
            "/*CRSR_AUTH_V1*/",
            "const __crsrReq={req};",
            r#"const __crsrSvc=__crsrReq?.service?.typeName||"";"#,
            r#"const __crsrIsAgent=__crsrSvc.indexOf("agent.v1.")===0||__crsrSvc==="aiserver.v1.BackgroundComposerService";"#,
            r#"const __crsrIsAi=__crsrSvc.indexOf("aiserver.v1.")===0;"#,
            r#"if(__crsrIsAgent||__crsrIsAi){{"#,
            r#"const __crsrFs=require("node:fs"),__crsrPath=require("node:path"),__crsrOs=require("node:os"),"#,
            r#"__crsrFile=process.env.NEXUS_CRSR_CREDENTIAL_FILE||("#,
            r#"process.platform==="win32"?"#,
            r#"__crsrPath.join(process.env.APPDATA||__crsrPath.join(__crsrOs.homedir(),"AppData","Roaming"),"#,
            r#""com.roviix.nexus","crsr-agent-credential.json"):"#,
            r#"__crsrPath.join(__crsrOs.homedir(),"#,
            r#"process.platform==="darwin"?"Library/Application Support":".config","#,
            r#""com.roviix.nexus","crsr-agent-credential.json"));"#,
            r#"let __crsrCred;try{{__crsrCred=JSON.parse(__crsrFs.readFileSync(__crsrFile,"utf8"))}}catch(__crsrReadErr){{__crsrCred=null}}"#,
            r#"if(__crsrCred?.apiKey&&(!__crsrCred.accessToken||!__crsrCred.expiresAtMs||Date.now()>__crsrCred.expiresAtMs-12e4)){{"#,
            r#"try{{const __crsrResp=yield fetch("https://api2.cursor.sh/auth/exchange_user_api_key","#,
            r#"{{method:"POST",headers:{{"content-type":"application/json","authorization":"Bearer "+__crsrCred.apiKey}},"#,
            r#"body:"{{}}"}}),__crsrBody=yield __crsrResp.json(),"#,
            r#"__crsrTok=__crsrBody&&(__crsrBody.accessToken||__crsrBody.access_token);"#,
            r#"if(__crsrTok){{__crsrCred.accessToken=__crsrTok;try{{const __crsrP=__crsrTok.split(".")[1],"#,
            r#"__crsrJ=JSON.parse(Buffer.from(__crsrP.replace(/-/g,"+").replace(/_/g,"/"),"base64").toString("utf8"));"#,
            r#"__crsrCred.expiresAtMs=typeof __crsrJ.exp==="number"?__crsrJ.exp*1e3:Date.now()+36e5}}"#,
            r#"catch(__crsrExpErr){{__crsrCred.expiresAtMs=Date.now()+36e5}}"#,
            r#"__crsrCred.renewedAtMs=Date.now();"#,
            r#"__crsrFs.writeFileSync(__crsrFile,JSON.stringify(__crsrCred,null,2)+"\n")}}}}"#,
            r#"catch(__crsrErr){{}}}}"#,
            r#"if(__crsrCred?.gatewayUrl){{"#,
            r#"if(__crsrCred.accessToken&&(!__crsrCred.expiresAtMs||Date.now()<=__crsrCred.expiresAtMs))__crsrReq.header.set("Authorization",`Bearer ${{__crsrCred.accessToken}}`);"#,
            r#"return}}"#,
            r#"if(!__crsrIsAgent)return;"#,
            r#"if(!__crsrCred?.apiKey)throw new Error("#,
            r#""[CRSR_AUTH_NOT_SELECTED] pick a crsr_ account in Nexus");"#,
            r#"if(!__crsrCred.accessToken)throw new Error("[CRSR_AUTH_TOKEN_MISSING] exchange failed");"#,
            r#"__crsrReq.header.set("Authorization",`Bearer ${{__crsrCred.accessToken}}`);return}}"#,
        ),
        req = req
    )
}

/// 写入 _getTransportForService 的路由调度 JS 块。
/// 将 aiserver.v1.InferenceService 精准分流至本地网关，使用纯 HTTP/1.1 transport 规避 HTTP/2 session origin 冲突。
pub fn transport_block(svc_var: &str) -> String {
    let svc = svc_var;
    format!(
        concat!(
            "/*CRSR_TRANSPORT_V1*/",
            "try{{const __cFs=require(\"node:fs\"),__cPath=require(\"node:path\"),__cOs=require(\"node:os\");",
            "const __cFile=process.env.NEXUS_CRSR_CREDENTIAL_FILE||(",
            "process.platform===\"win32\"?",
            "__cPath.join(process.env.APPDATA||__cPath.join(__cOs.homedir(),\"AppData\",\"Roaming\"),\"com.roviix.nexus\",\"crsr-agent-credential.json\"): ",
            "__cPath.join(__cOs.homedir(),process.platform===\"darwin\"?\"Library/Application Support\":\".config\",\"com.roviix.nexus\",\"crsr-agent-credential.json\"));",
            "let __cCred;",
            "if(!this._nexusCredTime||Date.now()-this._nexusCredTime>2000){{",
            "this._nexusCredTime=Date.now();",
            "try{{__cCred=JSON.parse(__cFs.readFileSync(__cFile,\"utf8\"))}}catch(e){{__cCred=null}}",
            "this._nexusCachedCred=__cCred}}else{{__cCred=this._nexusCachedCred}}",
            "if(__cCred?.gatewayUrl&&this.transportFactory){{",
            "const __cSvc={svc};",
            "if(__cSvc===\"aiserver.v1.InferenceService\"||__cSvc===\"agent.v1.AgentService\"||__cSvc===\"aiserver.v1.BidiService\"||__cSvc===\"aiserver.v1.AiService\"){{" ,
            "if(!this._nexusTransport||this._nexusGatewayUrl!==__cCred.gatewayUrl){{",
            "this._nexusGatewayUrl=__cCred.gatewayUrl;",
            "this._nexusTransport=this.transportFactory.createTransport({{baseUrl:__cCred.gatewayUrl.replace(/\\/+$/,\"\"),useHttp2:!1,maybeUseCppSpoofToken:!1}})}}",
            "return this._nexusTransport;}}}}}}catch(__cErr){{}}"
        ),
        svc = svc
    )
}

pub fn count_hits(content: &str) -> u32 {
    let auth = PATCHED_AUTH_RE.find_iter(content).count() as u32;
    let transport = PATCHED_TRANSPORT_RE.find_iter(content).count() as u32;
    auth + transport
}

pub fn count_anchors(content: &str) -> u32 {
    let unpatched_auth = UNPATCHED_AUTH_RE.find_iter(content).count() as u32;
    let patched_auth = PATCHED_AUTH_RE.find_iter(content).count() as u32;
    let unpatched_trans = UNPATCHED_TRANSPORT_RE.find_iter(content).count() as u32;
    let patched_trans = PATCHED_TRANSPORT_RE.find_iter(content).count() as u32;
    unpatched_auth + patched_auth + unpatched_trans + patched_trans
}

pub fn apply(content: &str) -> Option<String> {
    let mut hit = false;
    let mut text = content.to_string();

    // 1. applyAuthorization
    if UNPATCHED_AUTH_RE.is_match(&text) {
        hit = true;
        text = UNPATCHED_AUTH_RE
            .replace_all(&text, |caps: &regex::Captures| {
                let req = &caps[1];
                let opts = &caps[2];
                let vars = &caps[3];
                let suffix = &caps[4];
                let block = auth_block(req);
                format!("applyAuthorization({req},{opts}){{return a(this,void 0,void 0,function*(){{{vars}{block}{suffix}")
            })
            .into_owned();
    } else if PATCHED_AUTH_RE.is_match(&text) {
        let upgraded = PATCHED_AUTH_RE
            .replace_all(&text, |caps: &regex::Captures| {
                let req = &caps[1];
                let opts = &caps[2];
                let vars = &caps[3];
                let suffix = &caps[4];
                let block = auth_block(req);
                format!("applyAuthorization({req},{opts}){{return a(this,void 0,void 0,function*(){{{vars}{block}{suffix}")
            })
            .into_owned();
        if upgraded != text {
            hit = true;
            text = upgraded;
        }
    }

    // 2. _getTransportForService
    if UNPATCHED_TRANSPORT_RE.is_match(&text) {
        hit = true;
        text = UNPATCHED_TRANSPORT_RE
            .replace_all(&text, |caps: &regex::Captures| {
                let p1 = &caps[1];
                let p2 = &caps[2];
                let p3 = &caps[3];
                let block = transport_block(p1);
                format!("_getTransportForService({p1},{p2},{p3}=!0){{{block}let n;")
            })
            .into_owned();
    } else if PATCHED_TRANSPORT_RE.is_match(&text) {
        let upgraded = PATCHED_TRANSPORT_RE
            .replace_all(&text, |caps: &regex::Captures| {
                let p1 = &caps[1];
                let p2 = &caps[2];
                let p3 = &caps[3];
                let block = transport_block(p1);
                format!("_getTransportForService({p1},{p2},{p3}=!0){{{block}let n;")
            })
            .into_owned();
        if upgraded != text {
            hit = true;
            text = upgraded;
        }
    }

    if hit {
        Some(text)
    } else {
        None
    }
}

pub fn remove(content: &str) -> Option<String> {
    if !content.contains(AUTH_MARKER) && !content.contains(TRANSPORT_MARKER) {
        return None;
    }
    let mut hit = false;
    let mut text = content.to_string();

    if PATCHED_AUTH_RE.is_match(&text) {
        hit = true;
        text = PATCHED_AUTH_RE
            .replace_all(&text, |caps: &regex::Captures| {
                let req = &caps[1];
                let opts = &caps[2];
                let vars = &caps[3];
                let suffix = &caps[4];
                format!("applyAuthorization({req},{opts}){{return a(this,void 0,void 0,function*(){{{vars}{suffix}")
            })
            .into_owned();
    }

    if PATCHED_TRANSPORT_RE.is_match(&text) {
        hit = true;
        text = PATCHED_TRANSPORT_RE
            .replace_all(&text, |caps: &regex::Captures| {
                let p1 = &caps[1];
                let p2 = &caps[2];
                let p3 = &caps[3];
                format!("_getTransportForService({p1},{p2},{p3}=!0){{let n;")
            })
            .into_owned();
    }

    if hit {
        Some(text)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_carries_marker_and_does_not_touch_sand() {
        let b = auth_block("e");
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
            "pre {} mid {} between _getTransportForService(t,e,r=!0){{let n; and _getTransportForService(e,t,r=!0){{let n; post",
            original("var r,n,s,o,i,a,c,l,u,d,p,h;"),
            "applyAuthorization(t,e){return a(this,void 0,void 0,function*(){var r,n,s,i,o,a,u,c,l,m,d,f;if(e.overrideAuthToken){"
        );
        assert_eq!(count_anchors(&stock), 4);
        assert_eq!(count_hits(&stock), 0);

        let patched_all = apply(&stock).expect("should hit");
        assert_eq!(count_hits(&patched_all), 4);
        assert_eq!(count_anchors(&patched_all), 4);
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
        let src_auth = format!(
            "function* g(e,t){{var n,r,o,s,i,a,l,c,u,d,m,p;{block}if(t.overrideAuthToken){{}}}}",
            block = auth_block("e")
        );
        let src_transport = format!(
            "class Dummy {{ _getTransportForService(t,e,r=!0){{{block}let n;}} }}",
            block = transport_block("t")
        );
        let dir = tempfile::tempdir().unwrap();
        let file_auth = dir.path().join("auth.js");
        let file_trans = dir.path().join("transport.js");
        std::fs::write(&file_auth, src_auth).unwrap();
        std::fs::write(&file_trans, src_transport).unwrap();
        for f in [&file_auth, &file_trans] {
            let out = std::process::Command::new(&node)
                .args(["--check", f.to_str().unwrap()])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "node --check: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
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
