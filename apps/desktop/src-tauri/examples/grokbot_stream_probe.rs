//! 用盘上现有的 Grok Bot 续期种子，验证 `sand` 身份还能不能进入
//! `aiserver.v1.InferenceService/Stream`。
//!
//! 不覆写本地 relay/direct credential；新 `grok_bot` token 只留在内存。旧种子失效时会读取
//! Grok Bot safeStorage、唤醒或创建当前账号的 Box，并在 pod 内手工重放 0.65-shaped raw Stream；
//! 它不调用官方 `sendPrompt`，不能把结果当成有状态 Agent harness 的等价对照。可能触发钥匙串授权。
//! 默认会测试几条很短的真实推理并消耗少量 Bot 额度；`LIVE_MODELS=,` 只做 pod raw 对照与取证。
//!
//! pod 内那一发是**单变量对照**：同一张票、同一条会话、同一份系统提示、同一个工具、同一组
//! 0.65 headers，只切 `requested_model`（裸 vs 官方样本的 max + effort=high + fast=true）。
//! 这样「官方 profile 是不是准入必要条件」才有归因，而不是像上一轮那样同时换掉位置、提示、
//! 工具和会话。`POD_PROFILE_ORDER=reverse` 反向复测，排掉只罚第二发的限流假象。
//!
//! ```bash
//! NEXUS_GROKBOT_STREAM_PROBE=1 cargo run -p nexus-desktop --example grokbot_stream_probe
//! ```

use nexus_gateway::{ChatRequest, DeviceIdentity, Message, Role, Sampling, StreamConfig};
use nexus_grokbot::{credential, GrokBotService};
use std::path::PathBuf;
use std::time::Duration;

fn data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("NEXUS_DATA_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }

    #[cfg(target_os = "macos")]
    {
        return PathBuf::from(std::env::var_os("HOME").expect("HOME"))
            .join("Library/Application Support/com.roviix.nexus");
    }
    #[cfg(target_os = "windows")]
    {
        return PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA"))
            .join("com.roviix.nexus");
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".config")
            });
        base.join("com.roviix.nexus")
    }
}

fn mask_email(email: Option<&str>) -> String {
    let Some(email) = email else {
        return "(unknown)".into();
    };
    match email.split_once('@') {
        Some((user, domain)) => format!("{}***@{domain}", user.chars().take(3).collect::<String>()),
        None => "***".into(),
    }
}

/// 从 Grok Bot 当前活跃槽拿一张票，不写 relay/direct credential 文件。
/// descriptor 路径失败时会用会话唤醒 Box；读取 safeStorage 在 macOS 上可能触发钥匙串授权。
async fn mint_active_ephemeral() -> Result<
    (
        String,
        String,
        Option<String>,
        nexus_grokbot::pod::ExecTarget,
    ),
    &'static str,
> {
    let secrets = nexus_grokbot::secrets::load().map_err(|_| "读取 Grok Bot 登录态失败")?;
    let descriptor_target = nexus_grokbot::descriptor::load(&secrets)
        .ok()
        .and_then(|descriptor| nexus_grokbot::pod::ExecTarget::from_descriptor(&descriptor));
    let (target, seed) = match descriptor_target {
        Some(target) => match nexus_grokbot::pod::read_renewal_credential(&target).await {
            Ok(seed) => (target, seed),
            Err(_) => {
                let ensured =
                    nexus_grokbot::pod::ensure_sandbox(&secrets.session_token, &secrets.machine_id)
                        .await
                        .map_err(|_| "唤醒 Grok Bot Box 失败")?;
                let target = nexus_grokbot::pod::ExecTarget::from_ensure(&ensured)
                    .ok_or("Box 没返回 exec daemon")?;
                let seed = nexus_grokbot::pod::read_renewal_credential(&target)
                    .await
                    .map_err(|_| "从 Box 读取续期种子失败")?;
                (target, seed)
            }
        },
        None => {
            let ensured =
                nexus_grokbot::pod::ensure_sandbox(&secrets.session_token, &secrets.machine_id)
                    .await
                    .map_err(|_| "唤醒 Grok Bot Box 失败")?;
            let target = nexus_grokbot::pod::ExecTarget::from_ensure(&ensured)
                .ok_or("Box 没返回 exec daemon")?;
            let seed = nexus_grokbot::pod::read_renewal_credential(&target)
                .await
                .map_err(|_| "从 Box 读取续期种子失败")?;
            (target, seed)
        }
    };
    let renewed = credential::renew(&seed)
        .await
        .map_err(|_| "Grok Bot 续期失败")?;
    Ok((
        renewed.grok_bot_token,
        secrets.machine_id,
        secrets.account.email,
        target,
    ))
}

const POD_STREAM_PROBE_JS: &str = r#"
(async()=>{
  const fs=require("fs"),http2=require("http2"),crypto=require("crypto");
  const varint=(value)=>{let n=BigInt(value),out=[];do{let b=Number(n&127n);n>>=7n;if(n)b|=128;out.push(b)}while(n);return Buffer.from(out)};
  const bytes=(field,value)=>{const b=Buffer.isBuffer(value)?value:Buffer.from(value);return Buffer.concat([varint((field<<3)|2),varint(b.length),b])};
  const scalar=(field,value)=>Buffer.concat([varint(field<<3),varint(value)]);
  const readVar=(buf,start)=>{let n=0n,shift=0n,i=start;for(;i<buf.length;i++){const b=buf[i];n|=BigInt(b&127)<<shift;if(!(b&128))return[Number(n),i+1];shift+=7n}return[0,buf.length]};
  const field=(buf,want)=>{let i=0;while(i<buf.length){let tag;[tag,i]=readVar(buf,i);const no=tag>>3,wire=tag&7;if(wire===2){let len;[len,i]=readVar(buf,i);const v=buf.subarray(i,i+len);i+=len;if(no===want)return v}else if(wire===0){[,i]=readVar(buf,i)}else if(wire===1)i+=8;else if(wire===5)i+=4;else break}return null};
  // 续期种子和 boxOwnerTeamId 取自同一个 sand-host 进程的 environ：官方 Stream 的
  // x-cursor-team-id 正是 positiveInt(env.SAND_BOX_OWNER_TEAM_ID) 这一路来的。
  let seed="",teamId="";
  for(const id of fs.readdirSync("/proc")){if(!/^\d+$/.test(id))continue;try{const env=fs.readFileSync(`/proc/${id}/environ`).toString().split("\0");const hit=env.find(v=>v.startsWith("SAND_INFERENCE_RENEWAL_CREDENTIAL="));if(!hit)continue;seed=hit.slice(hit.indexOf("=")+1);const team=env.find(v=>v.startsWith("SAND_BOX_OWNER_TEAM_ID="));if(team)teamId=team.slice(team.indexOf("=")+1).trim();break}catch{}}
  if(!seed){console.log(JSON.stringify({ok:false,stage:"seed_missing"}));return}
  if(!/^\d+$/.test(teamId))teamId="";
  const officialHeaders={"content-type":"application/json","user-agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Sand/0.65.0 Safari/537.36","x-cursor-client-type":"sand","x-cursor-client-source":"sand-desktop","x-cursor-client-version":"0.65.0","x-sand-box-namespace":"prod","x-cursor-client-os":"linux"};
  const renewal=await fetch("https://api2.cursor.sh/sand-box/inference-credential",{method:"POST",headers:officialHeaders,body:JSON.stringify({credential:seed})});
  const renewed=await renewal.json().catch(()=>({}));
  const token=renewed.grokBotToken;
  if(!renewal.ok||!token){console.log(JSON.stringify({ok:false,stage:"renewal",http:renewal.status}));return}
  let systemPrompt="You are a helpful Grok Bot agent.";try{systemPrompt=fs.readFileSync("/workspace/sand-system-prompt-dump.md","utf8")}catch{}
  const systemMessage=Buffer.concat([scalar(1,4),bytes(2,systemPrompt)]);
  const message=Buffer.concat([scalar(1,1),bytes(2,"Use the SendMessage tool to reply with exactly: 1")]);
  const sendMessageTool=Buffer.concat([bytes(1,"SendMessage"),bytes(2,"Send a text message to the user.")]);
  const parameter=(id,value)=>Buffer.concat([bytes(1,id),bytes(2,value)]);
  // 对照矩阵：profile（bare / 官方 full）× x-cursor-team-id（不带 / 带）。
  // 其余一切固定，所以哪一项让准入翻转就是哪一项的作用。
  const bareRequested=Buffer.concat([bytes(1,"grok-4.7")]);
  const fullRequested=Buffer.concat([bytes(1,"grok-4.7"),scalar(2,1),bytes(3,parameter("effort","high")),bytes(3,parameter("fast","true"))]);
  const arms=[["bare",bareRequested,false],["full",fullRequested,false]].concat(teamId?[["bare+team",bareRequested,true],["full+team",fullRequested,true]]:[]);
  let activeAgentId=crypto.randomUUID();try{const active=JSON.parse(fs.readFileSync("/home/box/sand-data/agents/active-agent.json","utf8"));activeAgentId=active.agentId||active.activeAgentId||Object.values(active).find(value=>typeof value==="string"&&/^[0-9a-f-]{36}$/i.test(value))||activeAgentId}catch{}
  const conversationId=activeAgentId,conversationGroupId=activeAgentId;
  const now=Math.floor(Date.now()/1e6),src=[now>>40&255,now>>32&255,now>>24&255,now>>16&255,now>>8&255,now&255],ob=[];let prev=165;for(let i=0;i<6;i++){ob[i]=(src[i]^prev)+i&255;prev=ob[i]}
  let hostMachineId="__MACHINE_ID__";
  try{const hostSecrets=JSON.parse(fs.readFileSync("/home/box/sand-data/host-secrets.json","utf8"));const found=Object.values(hostSecrets).find(value=>typeof value==="string"&&value.length>=16);if(found)hostMachineId=found}catch{}
  const checksum=Buffer.from(ob).toString("base64url")+hostMachineId;
  // 两发共用同一张票、同一条会话、同一份系统提示与同一组 headers；只有 requested_model
  // 和各自的 request / invocation id 不同，这样结果才能归因到 profile 本身。
  const streamHeaders={":method":"POST",":path":"/aiserver.v1.InferenceService/Stream","content-type":"application/connect+proto","connect-protocol-version":"1","authorization":`Bearer ${token}`,"user-agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Sand/0.65.0 Safari/537.36","x-cursor-checksum":checksum,"x-cursor-client-type":"sand","x-cursor-client-source":"sand-desktop","x-cursor-client-version":"0.65.0","x-cursor-client-os":"linux","x-sand-box-namespace":"prod","x-ghost-mode":"false"};
  const send=(profile,requested,withTeam)=>new Promise((resolve)=>{
    const invocationId=crypto.randomUUID();
    const request=Buffer.concat([bytes(1,systemMessage),bytes(1,message),bytes(2,sendMessageTool),bytes(6,invocationId),bytes(7,requested),bytes(8,conversationId),bytes(12,conversationGroupId),bytes(20,"user")]);
    const envelope=Buffer.alloc(5+request.length);envelope[0]=0;envelope.writeUInt32BE(request.length,1);request.copy(envelope,5);
    const session=http2.connect("https://api2.cursor.sh");
    let status=0,chunks=[],finished=false;
    const finish=(result)=>{if(finished)return;finished=true;try{session.close()}catch{};resolve(Object.assign({arm:profile,team:!!withTeam,requestBytes:request.length},result))};
    session.on("error",e=>finish({ok:false,stage:"session",error:e.code||e.name}));
    const stream=session.request(Object.assign({},streamHeaders,withTeam&&teamId?{"x-cursor-team-id":teamId}:{},{"x-request-id":crypto.randomUUID()}));
    stream.on("response",headers=>{status=Number(headers[":status"]||0)});
    stream.on("data",chunk=>chunks.push(chunk));
    stream.on("error",e=>finish({ok:false,stage:"stream",http:status,error:e.code||e.name}));
    stream.on("end",()=>{const buf=Buffer.concat(chunks);let i=0,text="",frames=0,endCode="",endMessage="";while(i+5<=buf.length){const flags=buf[i],len=buf.readUInt32BE(i+1);if(i+5+len>buf.length)break;const payload=buf.subarray(i+5,i+5+len);i+=5+len;if(flags&2){try{const tail=JSON.parse(payload.toString());endCode=tail.error?.code||"";endMessage=tail.error?.message||""}catch{}}else{frames++;const part=field(payload,1),piece=part&&field(part,1);if(piece)text+=piece.toString()}}finish({ok:status===200&&frames>0&&!endCode,http:status,text:text.slice(0,20),frames,endCode,endMessage:endMessage.slice(0,80),bytes:buf.length})});
    stream.end(envelope);
    setTimeout(()=>finish({ok:false,stage:"timeout",http:status,bytes:chunks.reduce((n,b)=>n+b.length,0)}),25000).unref();
  });
  // 顺序本身也是混杂项（限流只罚后面几发就会看起来像某一项的作用）：POD_PROFILE_ORDER=reverse 复测。
  const order=process.env.POD_PROFILE_ORDER==="reverse"?arms.slice().reverse():arms;
  const results=[];
  for(const [profile,requested,withTeam] of order)results.push(await send(profile,requested,withTeam));
  console.log(JSON.stringify({probe:"profile-team-matrix",order:order.map(a=>a[0]),held:{ticket:"same",conversationId,systemPromptChars:systemPrompt.length,tool:"SendMessage",clientVersion:"0.65.0"},teamIdDigits:teamId.length,passed:results.filter(r=>r.ok).map(r=>r.arm),results}));
})().catch(e=>console.log(JSON.stringify({ok:false,stage:"exception",error:e.code||e.name||"Error"})));
"#;

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// `POD_INSPECT_ONLY=1`：只做只读取证，不再发 pod 内请求。想换取证切面重跑时用它，
/// 省掉重复的真实请求。
async fn probe_stream_inside_pod_unless_inspect_only(
    target: &nexus_grokbot::pod::ExecTarget,
    machine_id: &str,
) -> Result<Option<String>, &'static str> {
    if std::env::var("POD_INSPECT_ONLY").as_deref() == Ok("1") {
        return Ok(None);
    }
    probe_stream_inside_pod(target, machine_id).await.map(Some)
}

async fn probe_stream_inside_pod(
    target: &nexus_grokbot::pod::ExecTarget,
    machine_id: &str,
) -> Result<String, &'static str> {
    let script = POD_STREAM_PROBE_JS.replace("__MACHINE_ID__", machine_id);
    // 只认这一个字面量，不把任意环境变量拼进远端命令。
    let prefix = match std::env::var("POD_PROFILE_ORDER").as_deref() {
        Ok("reverse") => "POD_PROFILE_ORDER=reverse ",
        _ => "",
    };
    let command = format!("{prefix}node -e {}", shell_quote(&script));
    match tokio::time::timeout(
        Duration::from_secs(150),
        nexus_grokbot::pod::shell(target, &command),
    )
    .await
    {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(_)) => Err("pod 内 Stream 探针执行失败"),
        Err(_) => Err("pod 内 Stream 探针超过 150 秒"),
    }
}

const POD_RUNTIME_INSPECT_JS: &str = r#"
(async()=>{
  const fs=require("fs"),text=fs.readFileSync("/opt/sand/sand-host/host-main.cjs","utf8"),focus="__FOCUS__";
  const clean=s=>s.replace(/eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/g,"<jwt>").replace(/sbi_[A-Za-z0-9_-]+/g,"<sbi>").replace(/Bearer [A-Za-z0-9._-]+/g,"Bearer <redacted>").replace(/\s+/g," ");
  if(focus==="host-log"){
    const raw=fs.existsSync("/tmp/sand-host.log")?fs.readFileSync("/tmp/sand-host.log","utf8"):"";
    const rows=raw.split("\n").filter(line=>/model|inference|stream|permission|requestId|requested/i.test(line)).slice(-120).map(clean);
    console.log(JSON.stringify({focus,logLines:rows}));return;
  }
  if(focus==="store-schema"){
    let active="";try{active=JSON.parse(fs.readFileSync("/home/box/sand-data/agents/active-agent.json","utf8")).agentId||""}catch{}
    const dbPath=active?`/home/box/sand-data/agents/${active}/store.db`:"";
    try{const {DatabaseSync}=require("node:sqlite"),db=new DatabaseSync(dbPath,{readOnly:true});const tables=db.prepare("select name, sql from sqlite_master where type in ('table','view') order by name").all().map(row=>({name:row.name,sql:String(row.sql||"").slice(0,1000),count:(()=>{try{return Number(db.prepare(`select count(*) n from \"${String(row.name).replace(/\"/g,'\"\"')}\"`).get().n)}catch{return null}})()}));console.log(JSON.stringify({focus,dbExists:fs.existsSync(dbPath),tables}));db.close()}catch(e){const code=`import sqlite3,json,sys\np=sys.argv[1]\nc=sqlite3.connect('file:'+p+'?mode=ro',uri=True)\nrows=[]\nfor n,s in c.execute(\"select name,sql from sqlite_master where type in ('table','view') order by name\"):\n try: count=c.execute('select count(*) from \"'+n.replace('\\\"','\\\"\\\"')+'\"').fetchone()[0]\n except: count=None\n rows.append({'name':n,'sql':(s or '')[:1000],'count':count})\nprint(json.dumps({'focus':'store-schema','dbExists':True,'tables':rows}))`;const out=require("child_process").spawnSync("python3",["-c",code,dbPath],{encoding:"utf8",timeout:15000});console.log(out.stdout.trim()||JSON.stringify({focus,error:out.error?.code||e.code||e.name}))}return;
  }
  if(focus==="backend-identity"){
    const contexts=[];let at=0;
    while((at=text.indexOf("clientVersion",at))>=0&&contexts.length<8){const row=clean(text.slice(Math.max(0,at-400),Math.min(text.length,at+900)));if(row.includes("boxNamespace")&&row.includes("clientOS")&&!contexts.includes(row))contexts.push(row);at+=13}
    let version="";try{version=fs.readFileSync("/opt/sand/sand-host/version","utf8").trim()}catch{}
    console.log(JSON.stringify({focus,version,contexts}));return;
  }
  if(focus==="runtime-metadata"){
    let packageJson=null;try{packageJson=JSON.parse(fs.readFileSync("/opt/sand/sand-host/package.json","utf8"))}catch{}
    const env={};
    for(const id of fs.readdirSync("/proc")){if(!/^\d+$/.test(id))continue;try{if(fs.readlinkSync(`/proc/${id}/cwd`)!=="/opt/sand/sand-host")continue;for(const item of fs.readFileSync(`/proc/${id}/environ`).toString().split("\0")){const at=item.indexOf("=");if(at<=0)continue;const name=item.slice(0,at),value=item.slice(at+1);if(/VERSION|CLIENT_OS|NAMESPACE|PACKAGED|NODE_ENV/.test(name)&&value.length<200)env[name]=value}}catch{}}
    console.log(JSON.stringify({focus,packageJson,env}));return;
  }
  if(focus==="gateway-methods"){
    const names=[];for(const match of text.matchAll(/([A-Za-z_$][A-Za-z0-9_$]*):\s*rpcMethod\(\)\.args\(/g))names.push(match[1]);
    console.log(JSON.stringify({focus,names:[...new Set(names)].filter(name=>/agent|prompt|bot/i.test(name))}));return;
  }
  if(focus==="process-topology"){
    const rows=[];
    for(const id of fs.readdirSync("/proc")){if(!/^\d+$/.test(id))continue;try{const cmd=fs.readFileSync(`/proc/${id}/cmdline`).toString().replace(/\0/g," ").trim();if(!/node|sand|agent|cursor/i.test(cmd))continue;const cwd=fs.readlinkSync(`/proc/${id}/cwd`);const stat=fs.readFileSync(`/proc/${id}/stat`,"utf8").split(" ");rows.push({pid:Number(id),ppid:Number(stat[3]),cwd,cmd:clean(cmd).slice(0,240)})}catch{}}
    console.log(JSON.stringify({focus,rows}));return;
  }
  if(focus==="websocket-runtime"){
    const candidates={};
    for(const name of ["ws","undici","websocket"]){try{candidates[name]=require.resolve(name)}catch{candidates[name]=null}}
    let inspectorOpen=false;try{const r=await fetch("http://127.0.0.1:9229/json/list",{signal:AbortSignal.timeout(1000)});inspectorOpen=r.ok}catch{}
    console.log(JSON.stringify({focus,node:process.version,globalWebSocket:typeof WebSocket==="function",candidates,inspectorOpen}));return;
  }
  if(focus==="inference-env"){
    const rows=[];
    for(const id of fs.readdirSync("/proc")){if(!/^\d+$/.test(id))continue;try{if(fs.readlinkSync(`/proc/${id}/cwd`)!=="/opt/sand/sand-host")continue;const env=Object.fromEntries(fs.readFileSync(`/proc/${id}/environ`).toString().split("\0").filter(Boolean).map(v=>{const i=v.indexOf("=");return[v.slice(0,i),v.slice(i+1)]}));rows.push({pid:Number(id),agentMockResponsePresent:!!env.SAND_AGENT_MOCK_RESPONSE,agentMockResponseLength:(env.SAND_AGENT_MOCK_RESPONSE||"").length,backendUrl:env.SAND_BACKEND_URL||env.CURSOR_API_BASE_URL||null,originBackendUrl:env.SAND_ORIGIN_BACKEND_URL||null,devTokenFilePresent:!!env.SAND_DEV_INFERENCE_TOKEN_FILE,renewalCredentialPresent:!!env.SAND_INFERENCE_RENEWAL_CREDENTIAL,boxOwnerTeamIdPresent:!!env.SAND_BOX_OWNER_TEAM_ID,defaultModelId:env.SAND_DEFAULT_MODEL_ID||null})}catch{}}
    console.log(JSON.stringify({focus,rows}));return;
  }
  if(focus==="credential-claims"){
    // 只读比对：我们自己用 box 种子换到的票 vs box 自己盘上留下的票，看是不是同一类凭证。
    // 只输出声明字段与到期信息，绝不输出 token、种子或签名。
    const claimsOf=(jwt)=>{try{const parts=String(jwt).split(".");if(parts.length!==3)return null;const body=JSON.parse(Buffer.from(parts[1].replace(/-/g,"+").replace(/_/g,"/"),"base64").toString());const out={keys:Object.keys(body).sort()};for(const k of ["type","scope","aud","iss","plan","tier","entitlement","workload","traffic_type","is_grok_bot","grok_bot"])if(body[k]!==undefined)out[k]=typeof body[k]==="object"?JSON.stringify(body[k]).slice(0,120):String(body[k]).slice(0,120);if(body.sub)out.subjectLength=String(body.sub).length;if(body.exp)out.ttlSec=Number(body.exp)-Math.floor(Date.now()/1e3);return out}catch(e){return {error:e.name}}};
    let seed="";
    for(const id of fs.readdirSync("/proc")){if(!/^\d+$/.test(id))continue;try{const env=fs.readFileSync(`/proc/${id}/environ`).toString().split("\0");const hit=env.find(v=>v.startsWith("SAND_INFERENCE_RENEWAL_CREDENTIAL="));if(hit){seed=hit.slice(hit.indexOf("=")+1);break}}catch{}}
    const officialHeaders2={"content-type":"application/json","user-agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Sand/0.65.0 Safari/537.36","x-cursor-client-type":"sand","x-cursor-client-source":"sand-desktop","x-cursor-client-version":"0.65.0","x-sand-box-namespace":"prod","x-cursor-client-os":"linux"};
    let minted=null;
    if(seed){try{const resp=await fetch("https://api2.cursor.sh/sand-box/inference-credential",{method:"POST",headers:officialHeaders2,body:JSON.stringify({credential:seed})});const body=await resp.json().catch(()=>({}));minted={http:resp.status,responseKeys:Object.keys(body||{}).sort(),grokBot:claimsOf(body.grokBotToken),access:claimsOf(body.accessToken)}}catch(e){minted={error:e.name}}}
    // box 自己盘上的票：只扫 sand-data 里的 JSON，提取其中的 JWT 做声明摘要。
    const onDisk=[];
    const walk=(dir,depth)=>{if(depth>3)return;let items=[];try{items=fs.readdirSync(dir,{withFileTypes:true})}catch{return}for(const item of items){const full=`${dir}/${item.name}`;if(item.isDirectory()){if(/node_modules|\.git|csnaps/.test(item.name))continue;walk(full,depth+1);continue}if(!/\.json$|credential|token/i.test(item.name))continue;let raw="";try{if(fs.statSync(full).size>512000)continue;raw=fs.readFileSync(full,"utf8")}catch{continue}const found=raw.match(/eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/g);if(!found)continue;for(const jwt of found.slice(0,3))onDisk.push({fileName:item.name,claims:claimsOf(jwt)})}};
    walk("/home/box/sand-data",0);
    console.log(JSON.stringify({focus,seedPresent:!!seed,minted,onDisk:onDisk.slice(0,12)}));return;
  }
  if(focus==="stored-models"){
    const rows=[],teamIds=[];
    const inspect=(value,path)=>{if(value==null||typeof value!=="object")return;if(!Array.isArray(value)&&typeof value.modelId==="string"){rows.push({path,modelId:value.modelId,maxMode:value.maxMode??null,builtInModel:value.builtInModel??null,parameters:Array.isArray(value.parameters)?value.parameters.map(p=>({id:p?.id,value:p?.value})):[]})}for(const [key,child] of Object.entries(value)){if(/team/i.test(key)&&(typeof child==="number"||typeof child==="string"&&/^\d+$/.test(child)))teamIds.push({path:path?`${path}.${key}`:key,value:String(child)});inspect(child,path?`${path}.${key}`:key)}};
    for(const file of ["/home/box/sand-data/settings.json","/home/box/sand-data/sand-feature-flag-overrides.json"]){if(!fs.existsSync(file))continue;try{inspect(JSON.parse(fs.readFileSync(file,"utf8")),file)}catch{}}
    console.log(JSON.stringify({focus,rows,teamIds}));return;
  }
  const named={};
  for(const name of ["resolveSandBackendAuthContext","applySandInferenceRequestContext","resolveSandBackendRequestHeaders","createSandInferenceInterceptor","createSandBackendTransport","createSandCursorBackendClient","buildStreamRequest","createSandDefaultRequestedModel","createSandRequestedModelFromSelection","resolveSandRequestedModel","buildSandRequestLineageHeaders","createCursorInferencePromptSession","createProtoSessionProvider","sandBackendUserAgentOf","getSandBackendClientHeaders","applyLocalCliModeHeader","sandClientBaseVersionOf","sandClientVersionOf","sandBoxNamespaceOf","sandVariantOf","backendUrlOf","originBackendUrlOf","sandBackendIdentityOf","readSandProcessEnvironment","createHostAuthService","renewSandBoxInferenceCredential","getOrCreateHostMachineId","createCursorChecksum"]){const at=text.indexOf(`function ${name}(`);named[name]=at<0?"":clean(text.slice(at,Math.min(text.length,at+5200)))}
  const around={};
  for(const pattern of ["inferenceRequestContext","getGrokBotAccessToken","getGrokBotToken","getTeamId","expectedAccountScope","boxOwnerTeamId","x-cursor-team-id","inferenceProxyJwt","trafficType","SAND_BOX_AUTH_ID","CURSOR_AGENT_SOCKET","SAND_DEFAULT_MODEL_ID","createSandDefaultRequestedModel","getDefaultModel","new InferenceStreamRequest","InferenceStreamRequest|","InferenceReason|","ProtoSessionProvider = class","ProtoPromptBuilder = class","ProtoPromptExecutor = class","buildStreamRequest({","originatingFlowKey","callerRoutingReasonKey","turnUnitTypeKey","turnUnitIdKey","parentRequestIdKey","rootParentRequestIdKey","inferenceReason","lineage","sendPrompt","createAgent:","deleteAgents:","temporalAgent","Temporal"]){let from=0,at,rows=[];while((at=text.indexOf(pattern,from))>=0&&rows.length<6){rows.push(clean(text.slice(Math.max(0,at-500),Math.min(text.length,at+4000))));from=at+pattern.length}around[pattern]=rows}
  console.log(JSON.stringify({hostBytes:text.length,focus,named:named[focus]||null,around:around[focus]||[]}));
})()
"#;

/// `POD_INSPECT_FOCUS` 支持逗号分隔；逐项按白名单校验，不认的整条丢掉。
/// 一次运行取多项可以少唤醒 pod、少发请求。
fn requested_inspect_focuses() -> Vec<&'static str> {
    let requested = std::env::var("POD_INSPECT_FOCUS").unwrap_or_default();
    let picked: Vec<&'static str> = requested
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter_map(|name| INSPECT_FOCUSES.iter().copied().find(|a| *a == name))
        .collect();
    if picked.is_empty() {
        vec![INSPECT_FOCUSES[0]]
    } else {
        picked
    }
}

/// 允许取证的切面。第一项是默认值；只有这里列出的字符串会被拼进远端命令。
const INSPECT_FOCUSES: &[&str] = &[
    "resolveSandBackendAuthContext",
    "applySandInferenceRequestContext",
    "resolveSandBackendRequestHeaders",
    "createSandInferenceInterceptor",
    "createSandBackendTransport",
    "createSandCursorBackendClient",
    "buildStreamRequest",
    "createSandDefaultRequestedModel",
    "createSandRequestedModelFromSelection",
    "resolveSandRequestedModel",
    "buildSandRequestLineageHeaders",
    "createCursorInferencePromptSession",
    "createProtoSessionProvider",
    "sandBackendUserAgentOf",
    "getSandBackendClientHeaders",
    "applyLocalCliModeHeader",
    "sandClientBaseVersionOf",
    "sandClientVersionOf",
    "sandBoxNamespaceOf",
    "sandVariantOf",
    "backendUrlOf",
    "originBackendUrlOf",
    "sandBackendIdentityOf",
    "readSandProcessEnvironment",
    "createHostAuthService",
    "renewSandBoxInferenceCredential",
    "getOrCreateHostMachineId",
    "createCursorChecksum",
    "host-log",
    "gateway-methods",
    "process-topology",
    "websocket-runtime",
    "inference-env",
    "credential-claims",
    "stored-models",
    "runtime-metadata",
    "backend-identity",
    "store-schema",
    "inferenceRequestContext",
    "getGrokBotAccessToken",
    "getGrokBotToken",
    "getTeamId",
    "expectedAccountScope",
    "boxOwnerTeamId",
    "x-cursor-team-id",
    "inferenceProxyJwt",
    "trafficType",
    "SAND_BOX_AUTH_ID",
    "CURSOR_AGENT_SOCKET",
    "SAND_DEFAULT_MODEL_ID",
    "createSandDefaultRequestedModel",
    "getDefaultModel",
    "new InferenceStreamRequest",
    "InferenceStreamRequest|",
    "InferenceReason|",
    "ProtoSessionProvider = class",
    "ProtoPromptBuilder = class",
    "ProtoPromptExecutor = class",
    "buildStreamRequest({",
    "originatingFlowKey",
    "callerRoutingReasonKey",
    "turnUnitTypeKey",
    "turnUnitIdKey",
    "parentRequestIdKey",
    "rootParentRequestIdKey",
    "inferenceReason",
    "lineage",
    "sendPrompt",
    "createAgent:",
    "deleteAgents:",
    "temporalAgent",
    "Temporal",
];

async fn inspect_pod_runtime(
    target: &nexus_grokbot::pod::ExecTarget,
    focus: &str,
) -> Result<String, &'static str> {
    let script = POD_RUNTIME_INSPECT_JS.replace("__FOCUS__", focus);
    let command = format!("node -e {}", shell_quote(&script));
    match tokio::time::timeout(
        Duration::from_secs(45),
        nexus_grokbot::pod::shell(target, &command),
    )
    .await
    {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(_)) => Err("pod runtime 取证失败"),
        Err(_) => Err("pod runtime 取证超过 45 秒"),
    }
}

#[tokio::main]
async fn main() {
    if std::env::var("NEXUS_GROKBOT_STREAM_PROBE").ok().as_deref() != Some("1") {
        eprintln!(
            "这会续一张 Grok Bot 票并打一发真实 Stream。确认后设 NEXUS_GROKBOT_STREAM_PROBE=1。"
        );
        std::process::exit(2);
    }

    let service = GrokBotService::offline(&data_dir());
    let saved = match service.stream_credential() {
        Ok(Some(credential)) => credential,
        Ok(None) => {
            eprintln!("没有 grokbot-stream-credential.json；先在 Sand 页生成直连凭证。");
            std::process::exit(3);
        }
        Err(_) => {
            eprintln!("读取 Grok Bot 直连凭证失败（未输出文件内容）。");
            std::process::exit(3);
        }
    };
    if saved.validate().is_err() || !service.direct_matches_active(&saved) {
        eprintln!("直连凭证无效，或属于 Grok Bot 上一个登录槽；不发请求。");
        std::process::exit(3);
    }
    let Some(seed) = saved
        .renewal_credential
        .as_deref()
        .filter(|seed| seed.starts_with("sbi_"))
    else {
        eprintln!("直连凭证没有可用的续期种子；不发请求。");
        std::process::exit(3);
    };

    eprintln!(
        "账号 {} · 续期种子已就绪 · 新票只留内存",
        mask_email(saved.account_email.as_deref())
    );
    let (grok_bot_token, machine_id, pod_target) =
        match tokio::time::timeout(Duration::from_secs(25), credential::renew(seed)).await {
            Ok(Ok(renewed)) => (renewed.grok_bot_token, saved.machine_id, None),
            _ => {
                eprintln!("盘上续期种子已失效；先从 Grok Bot 当前活跃槽临时取票（不覆写凭证）");
                match tokio::time::timeout(Duration::from_secs(100), mint_active_ephemeral()).await
                {
                    Ok(Ok((token, machine_id, email, target))) => {
                        eprintln!("Grok Bot 活跃账号 {}", mask_email(email.as_deref()));
                        (token, machine_id, Some(target))
                    }
                    active_error => {
                        let reason = match active_error {
                            Ok(Err(reason)) => reason,
                            Err(_) => "Grok Bot 活跃槽取票超过 100 秒",
                            Ok(Ok(_)) => unreachable!(),
                        };
                        eprintln!("{reason}；改用当前 Cursor 会话临时唤醒/创建 Box");
                        let cursor = nexus_cursor::Cursor::detect().expect("找不到本机 Cursor");
                        let auth = cursor.state.read_auth().expect("读不到 Cursor 登录态");
                        let access = auth
                            .get("cursorAuth/accessToken")
                            .filter(|token| !token.is_empty())
                            .expect("Cursor 当前没登录")
                            .to_string();
                        let email = auth.email().unwrap_or_else(|| "(unknown)".into());
                        let machine = cursor.machine.read();
                        let machine_id = if machine.machine_id.len() == 64 {
                            machine.machine_id
                        } else {
                            DeviceIdentity::derived(&access).machine_id
                        };
                        eprintln!("临时 Box 账号 {}", mask_email(Some(&email)));
                        let minted = match tokio::time::timeout(
                            Duration::from_secs(75),
                            service.mint_direct_for_account_ephemeral(&email, &access, &machine_id),
                        )
                        .await
                        {
                            Ok(Ok(credential)) => credential,
                            Ok(Err(error)) => {
                                eprintln!("临时取票失败：{}", error.message);
                                std::process::exit(1);
                            }
                            Err(_) => {
                                eprintln!("临时取票超过 75 秒；已停止。");
                                std::process::exit(1);
                            }
                        };
                        (minted.grok_bot_token, minted.machine_id, None)
                    }
                }
            }
        };
    eprintln!("取票成功；开始建立 sand Stream");

    let identity = DeviceIdentity::pinned(&grok_bot_token, machine_id.clone());
    let config = StreamConfig {
        client_type: "sand".into(),
        idle_timeout: Duration::from_secs(35),
        max_turn: Duration::from_secs(55),
        ..StreamConfig::default()
    };
    let models = std::env::var("LIVE_MODELS")
        .unwrap_or_else(|_| "grok-4.7,grok-4.5,sand-cua,default,grok-4.6".into());
    let mut passed = 0usize;
    let mut total = 0usize;
    for model in models.split(',').map(str::trim).filter(|m| !m.is_empty()) {
        total += 1;
        let request = ChatRequest {
            model: model.to_string(),
            messages: vec![Message::text(Role::User, "Reply with exactly: 1")],
            sampling: Sampling {
                max_output_tokens: Some(8),
                ..Sampling::default()
            },
            ..ChatRequest::default()
        };
        let mut text = String::new();
        let mut on_delta = |delta| {
            if let nexus_gateway::Delta::Text(part) = delta {
                text.push_str(&part);
            }
        };
        let result = tokio::time::timeout(
            Duration::from_secs(70),
            nexus_gateway::inference::stream(
                &nexus_gateway::http_client(),
                &config,
                &grok_bot_token,
                &identity,
                &request,
                &mut on_delta,
            ),
        )
        .await;
        match result {
            Err(_) => eprintln!("FAIL · {model} · Stream 总时长超过 70 秒"),
            Ok(Ok(completion)) => {
                passed += 1;
                println!(
                    "PASS · {model} → {} · text={:?} · usage={}/{}",
                    completion.routed_model.as_deref().unwrap_or("?"),
                    text.trim(),
                    completion.usage.input_tokens,
                    completion.usage.output_tokens
                );
            }
            Ok(Err(error)) => eprintln!(
                "FAIL · {model} · kind={} status={} cursor_code={:?} · {}",
                error.kind.as_str(),
                error.status,
                error.cursor_code,
                error.message
            ),
        }
    }
    println!("Stream 模型准入：{passed}/{total}");
    if passed == 0 {
        if let Some(target) = pod_target.as_ref() {
            eprintln!(
                "Mac 直连全部被拒；在同一活跃 pod 内手工重放 0.65-shaped raw Stream，\
                 并对裸 / 完整 profile 各发一次（票据、会话、系统提示、工具与 headers 固定）"
            );
            match probe_stream_inside_pod_unless_inspect_only(target, &machine_id).await {
                Ok(None) => eprintln!("POD_INSPECT_ONLY=1：跳过 pod 内对照，只做只读取证"),
                Ok(Some(result)) => {
                    println!("pod 内 profile A/B：{result}");
                    eprintln!(
                        "两档结论不同时，再用 live_matrix 在外部跑同一组对照，区分是 profile 还是执行位置：\
                         NEXUS_GATEWAY_PROBE=1 PROBE_CLIENT_TYPE=sand PROBE_ACCESS_TOKEN=… PROBE_MACHINE_ID=… \
                         LIVE_MODELS=grok-4.7 cargo run -p nexus-gateway --example live_matrix -- raw profile-ab"
                    );
                }
                Err(reason) => eprintln!("pod 内 profile A/B：{reason}"),
            }
            eprintln!("读取 pod 当前 sand-host 的脱敏推理调用线索");
            for focus in requested_inspect_focuses() {
                match inspect_pod_runtime(target, focus).await {
                    Ok(result) => println!("pod runtime[{focus}]：{result}"),
                    Err(reason) => eprintln!("pod runtime[{focus}]：{reason}"),
                }
            }
        } else {
            eprintln!("这次票据没有关联的活跃 pod target，无法补做 pod 内对照");
        }
        std::process::exit(1);
    }
}
