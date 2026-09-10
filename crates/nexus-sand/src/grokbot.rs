//! Grok Bot 鉴权的两个注入块（Box Relay / 直连），以及旧版 interceptor 的原文（只为迁移卸掉）。
//!
//! 凭证的读取 / 生成 / 续期全在 `nexus-grokbot`；这里只有要写进 `main.js` 的 JS 字面量。
//! 两个块都插在 `TransportFactory.applyAuthorization` 的变量声明之后、`if(t.overrideAuthToken){`
//! 之前，按 `InferenceService/Stream` 守卫，命中就 `return`（跳过官方的 IDE token 逻辑）。
//! 外层是 `__awaiter` 包着的 generator，所以块里可以 `yield` promise。
//!
//! **写法约束**：这些字面量里有大量 `{}` / `{...}`，绝不能过 `format!` 的位置参数（`{}` 会被当占位
//! 符——正是第一版双逗号 bug 的来源）；拼接只用命名参数或 `concat!`。

pub use nexus_grokbot::{
    relay_config_path, BoxRelayDescriptor, DirectCredentialInfo, GrokBotService, GrokBotStatus,
    RelayInfo, StreamCredential, BOX_RELAY_PATH,
};

/// 第一版直连 Connect interceptor（挂在 `originTransport` 的 `interceptors`，从未生效）。
/// 仅用于把老盘上的它迁走。
pub fn legacy_direct_stream_interceptor() -> &'static str {
    r#"(function(){var P=typeof process!=="undefined"&&process.platform,F=P==="win32"?require("path").join(require("os").homedir(),"AppData","Roaming","com.roviix.nexus","grokbot-stream-credential.json"):P==="darwin"?require("path").join(require("os").homedir(),"Library","Application Support","com.roviix.nexus","grokbot-stream-credential.json"):require("path").join(require("os").homedir(),".config","com.roviix.nexus","grokbot-stream-credential.json"),E=typeof process!=="undefined"&&process.env&&process.env.NEXUS_GROKBOT_CREDENTIAL_FILE,G=E||F;function C(m){var e=Math.floor(Date.now()/1e6),b=[e>>40&255,e>>32&255,e>>24&255,e>>16&255,e>>8&255,e&255],p=165,o=new Uint8Array(6);for(var i=0;i<6;i++)o[i]=(b[i]^p+i)&255,p=o[i];return Buffer.from(o).toString("base64url")+m}function R(){try{return JSON.parse(require("fs").readFileSync(G,"utf8"))}catch(e){return null}}function W(j){try{require("fs").writeFileSync(G,JSON.stringify(j))}catch(e){}}function L(){var j=R();if(!j||!j.grokBotToken)return Promise.resolve(null);if(j.expiresAtMs&&Date.now()>j.expiresAtMs-12e4&&j.renewalCredential)return fetch("https://api2.cursor.sh/sand-box/inference-credential",{method:"POST",headers:{"content-type":"application/json","x-cursor-client-type":"sa"+"nd","x-cursor-client-version":j.clientVersion||"0.44.0","x-sand-box-namespace":j.namespace||"prod"},body:JSON.stringify({credential:j.renewalCredential})}).then(function(r){return r.json()}).then(function(b){if(b.grokBotToken){j.grokBotToken=b.grokBotToken;j.expiresAtMs=b.expiresAtMs||Date.now()+6e5;W(j)}return j}).catch(function(){return j});return Promise.resolve(j)}return function(n){return function(r){return L().then(function(j){if(j&&j.grokBotToken&&j.machineId&&r.service.typeName==="aiserver.v1.InferenceService"&&r.method.name==="Stream"){r.header.set("authorization","Bearer "+j.grokBotToken);r.header.set("x-cursor-checksum",C(j.machineId));r.header.set("x-cursor-client-type","sa"+"nd");r.header.set("x-cursor-client-version",j.clientVersion||"0.44.0");r.header.set("x-sand-box-namespace",j.namespace||"prod");r.header.set("x-ghost-mode","false");try{r.header.delete("x-client-key")}catch(e){}try{r.header.delete("x-session-id")}catch(e){}try{r.header.delete("x-cursor-config-version")}catch(e){}}return n(r)})}}})()"#
}

/// Box Relay 块（字节级与 v135 脚本一致，两边互认）：改 `e.url` 到 Box 内 relay，Bearer 换 descriptor
/// 的短期 token，真正的 grokBotToken 留在 Box。
pub fn grok_box_relay_auth_block() -> &'static str {
    concat!(
        "/*SAND_GROK_BOX_RELAY_AUTH_V1*/",
        r#"const __sandGrokStream=e?.service?.typeName==="aiserver.v1.InferenceService""#,
        r#"&&e?.method?.name==="Stream";"#,
        r#"if(__sandGrokStream){"#,
        r#"const __sandRelayFs=require("node:fs"),"#,
        r#"__sandRelayPath=require("node:path"),__sandRelayOs=require("node:os"),"#,
        r#"__sandRelayConfigPath=process.env.SAND_GROK_BOX_RELAY_CONFIG||("#,
        r#"process.platform==="win32"?"#,
        r#"__sandRelayPath.join(process.env.LOCALAPPDATA||process.env.APPDATA||"#,
        r#"__sandRelayPath.join(__sandRelayOs.homedir(),"AppData","Local"),"#,
        r#""SandClientModeStream","sand-client-cli","grok-box-relay.json"):"#,
        r#"__sandRelayPath.join(__sandRelayOs.homedir(),"#,
        r#"process.platform==="darwin"?"Library/Application Support":".config","#,
        r#""SandClientModeStream","sand-client-cli","grok-box-relay.json")),"#,
        r#"__sandRelayConfig=JSON.parse(__sandRelayFs.readFileSync("#,
        r#"__sandRelayConfigPath,"utf8"));"#,
        r#"if(!__sandRelayConfig?.baseUrl||!__sandRelayConfig?.token)throw new Error("#,
        r#""[SAND_GROK_BOX_RELAY_CONFIG_INVALID] Grok Bot gateway descriptor is missing");"#,
        r#"e.url=new URL(__sandRelayConfig.relayPath||"#,
        r#""/sand-stream-relay/aiserver.v1.InferenceService/Stream","#,
        r#"__sandRelayConfig.baseUrl).toString();"#,
        r#"e.header.set("Authorization",`Bearer ${__sandRelayConfig.token}`);"#,
        r#"for(const[__sandHeader,__sandValue]of Object.entries("#,
        r#"__sandRelayConfig.headers||{}))"string"==typeof __sandValue&&"#,
        r#"__sandValue.length&&e.header.set(__sandHeader,__sandValue);"#,
        r#"e.header.set("x-cursor-client-type",String("sand")),"#,
        r#"e.header.set("x-cursor-client-source","sand-desktop"),"#,
        r#"e.header.set("x-cursor-client-version","0.44.0"),"#,
        r#"e.header.set("x-sand-box-namespace","prod");return}"#,
    )
}

/// 直连块：读 Nexus 的 `grokbot-stream-credential.json`，快过期就凭 `sbi_*` 续（免鉴权，见
/// `nexus-grokbot::credential`），Bearer 换 grokBotToken，补 sand 头，`return`。
///
/// 2026-09-09 头容忍度矩阵（`gateway/scripts/probe-grokbot-header-tolerance.mjs`）：IDE 多余头、
/// 3.19.13 版本号、别的 machineId 的 checksum 都放行；**只有 `x-cursor-client-type` 必须是 `sand`**。
/// 所以这里除 Authorization 外只补形态一致的几个头，不删任何 IDE 头。
///
/// 续期失败静默：token 还没过期就照旧发；真过期了服务端会回 16，界面上的凭证状态卡会提示重生成。
pub fn grokbot_direct_auth_block() -> &'static str {
    concat!(
        "/*SAND_GROKBOT_DIRECT_AUTH_V1*/",
        r#"const __sandGrokStream=e?.service?.typeName==="aiserver.v1.InferenceService""#,
        r#"&&e?.method?.name==="Stream";"#,
        r#"if(__sandGrokStream){"#,
        r#"const __sdFs=require("node:fs"),__sdPath=require("node:path"),__sdOs=require("node:os"),"#,
        r#"__sdFile=process.env.NEXUS_GROKBOT_CREDENTIAL_FILE||("#,
        r#"process.platform==="win32"?"#,
        r#"__sdPath.join(process.env.APPDATA||__sdPath.join(__sdOs.homedir(),"AppData","Roaming"),"#,
        r#""com.roviix.nexus","grokbot-stream-credential.json"):"#,
        r#"__sdPath.join(__sdOs.homedir(),"#,
        r#"process.platform==="darwin"?"Library/Application Support":".config","#,
        r#""com.roviix.nexus","grokbot-stream-credential.json"));"#,
        r#"let __sdCred=JSON.parse(__sdFs.readFileSync(__sdFile,"utf8"));"#,
        r#"if(!__sdCred?.grokBotToken)throw new Error("#,
        r#""[SAND_GROKBOT_DIRECT_CONFIG_INVALID] Nexus grokbot credential is missing");"#,
        r#"if(__sdCred.renewalCredential&&(!__sdCred.expiresAtMs||Date.now()>__sdCred.expiresAtMs-12e4)){"#,
        r#"try{const __sdResp=yield fetch("https://api2.cursor.sh/sand-box/inference-credential","#,
        r#"{method:"POST",headers:{"content-type":"application/json"},"#,
        r#"body:JSON.stringify({credential:__sdCred.renewalCredential})}),"#,
        r#"__sdBody=yield __sdResp.json();"#,
        r#"if(__sdBody?.grokBotToken){__sdCred.grokBotToken=__sdBody.grokBotToken,"#,
        r#"__sdCred.expiresAtMs=__sdBody.expiresAtMs||Date.now()+6e5,__sdCred.renewedAtMs=Date.now(),"#,
        r#"__sdFs.writeFileSync(__sdFile,JSON.stringify(__sdCred,null,2)+"\n")}}catch(__sdErr){}}"#,
        r#"e.header.set("Authorization",`Bearer ${__sdCred.grokBotToken}`);"#,
        r#"if(__sdCred.machineId){const __sdTs=Math.floor(Date.now()/1e6),"#,
        r#"__sdIn=[__sdTs>>40&255,__sdTs>>32&255,__sdTs>>24&255,__sdTs>>16&255,__sdTs>>8&255,__sdTs&255],"#,
        r#"__sdOut=new Uint8Array(6);let __sdPrev=165;"#,
        r#"for(let __sdI=0;__sdI<6;__sdI++)__sdOut[__sdI]=(__sdIn[__sdI]^__sdPrev)+__sdI&255,__sdPrev=__sdOut[__sdI];"#,
        r#"e.header.set("x-cursor-checksum",Buffer.from(__sdOut).toString("base64url")+__sdCred.machineId)}"#,
        r#"e.header.set("x-cursor-client-type",String("sand")),"#,
        r#"e.header.set("x-cursor-client-source","sand-desktop"),"#,
        r#"e.header.set("x-cursor-client-version",__sdCred.clientVersion||"0.44.0"),"#,
        r#"e.header.set("x-sand-box-namespace",__sdCred.namespace||"prod"),"#,
        r#"e.header.set("x-ghost-mode","false");return}"#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_carry_their_markers_and_guard_on_stream() {
        for (block, marker) in [
            (
                grok_box_relay_auth_block(),
                "/*SAND_GROK_BOX_RELAY_AUTH_V1*/",
            ),
            (
                grokbot_direct_auth_block(),
                "/*SAND_GROKBOT_DIRECT_AUTH_V1*/",
            ),
        ] {
            assert!(block.starts_with(marker));
            assert!(block.contains(r#"e?.method?.name==="Stream""#));
            assert!(block.ends_with("return}"));
            assert!(!block.contains(",,"));
        }
    }

    #[test]
    fn direct_block_reads_the_same_file_nexus_grokbot_writes() {
        let b = grokbot_direct_auth_block();
        assert!(b.contains(nexus_grokbot::STREAM_CREDENTIAL_FILENAME));
        assert!(b.contains("com.roviix.nexus"));
        assert!(b.contains("sand-box/inference-credential"));
        // 只补头不删头：容忍度矩阵已证明 IDE 多余头无害。
        assert!(!b.contains("header.delete"));
    }

    /// 直连块是合法 JS（放进一个 generator 里 `node --check`）。
    #[test]
    fn direct_block_is_syntactically_valid_js() {
        let Ok(node) = which_node() else {
            eprintln!("no node on PATH; skipping syntax check");
            return;
        };
        let src = format!(
            "function* g(e,t){{var n,r,o,s,i,a,l,c,u,d,m,p;{block}if(t.overrideAuthToken){{}}}}",
            block = grokbot_direct_auth_block()
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
