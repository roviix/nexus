// Nexus Sand 远程中继（在远程主机上用 cursor-server 自带的 node 跑）。
//
// 在远程 127.0.0.1:<port> 上监听；每条进来的 TCP 连接都编个号，字节流经本进程的 stdout 送回
// 本机、从 stdin 收回来——stdin/stdout 就是那条 ssh 会话。本机那头（Rust `remote::tunnel`）把
// 每个号接到本机的网关透传口或本机代理。
//
// 为什么不用 `ssh -R`：有的 ssh 网关（容器平台常见）只把命令转进工作区、不转反向端口转发——
// `-R` 请求被接受了，工作区里却什么都没在听；而 exec 通道对二进制是透明的、到处都有。
//
// 帧：1 字节类型 | 4 字节流号（大端）| 4 字节长度（大端）| 载荷。
//   1 OPEN  远程→本机：新连接（无载荷）
//   2 DATA  双向
//   3 EOF   双向：这一侧不再发了（对端半关）
//   4 CLOSE 双向：连接彻底没了
// 就绪 / 起不来各打一行文本到 stdout（`NEXUS-RELAY 1 READY <port>` / `… ERROR <code> <msg>`），
// 之后 stdout 只走帧。远程的 shell profile 往 stdout 打招呼也不怕：本机那头只认这行前缀。
"use strict";
const net = require("net");
const fs = require("fs");
const path = require("path");
const os = require("os");

const PROTO = "NEXUS-RELAY 1";
const OPEN = 1, DATA = 2, EOF = 3, CLOSE = 4;
const port = Number(process.argv[1]);
if (!Number.isInteger(port) || port <= 0 || port > 65535) {
  process.stdout.write(`${PROTO} ERROR EINVAL 端口不合法：${process.argv[1]}\n`);
  process.exit(2);
}

const socks = new Map();
let nextId = 1;
let ready = false;

function frame(type, id, payload) {
  const head = Buffer.allocUnsafe(9);
  head[0] = type;
  head.writeUInt32BE(id, 1);
  head.writeUInt32BE(payload ? payload.length : 0, 5);
  return payload && payload.length ? Buffer.concat([head, payload]) : head;
}

// 往 ssh 写不动了就让所有连接先停一停：远程这头的应用比链路快是常态。
let outPaused = false;
function send(buf) {
  if (!process.stdout.write(buf) && !outPaused) {
    outPaused = true;
    for (const s of socks.values()) s.pause();
  }
}
process.stdout.on("drain", () => {
  outPaused = false;
  for (const s of socks.values()) s.resume();
});
// stdout 断了 = ssh 没了。安静退出，本机那头会重连再起一条。
process.stdout.on("error", () => process.exit(0));

// allowHalfOpen：客户端发完请求就半关（FIN）是合法的，本机那头的应答还在路上；默认行为会在
// 收到 FIN 时自动把写侧也关掉，应答的尾巴就丢了（真机上表现为下载被截断）。我们自己在收到
// 本机的 EOF 帧时 end()，两边都完了 socket 才关。
const server = net.createServer({ allowHalfOpen: true }, (sock) => {
  const id = nextId++;
  if (nextId > 0xffffffff) nextId = 1;
  socks.set(id, sock);
  sock.setNoDelay(true);
  send(frame(OPEN, id));
  sock.on("data", (chunk) => send(frame(DATA, id, chunk)));
  sock.on("end", () => { if (socks.has(id)) send(frame(EOF, id)); });
  sock.on("close", () => { if (socks.delete(id)) send(frame(CLOSE, id)); });
  sock.on("error", () => { /* close 会跟着来 */ });
});
server.on("error", (e) => onListenError(e));

// ---------------------------------------------------------------- 监听（含抢回自己的旧端口）

const pidDir = path.join(os.homedir(), ".nexus-sand");
const pidFile = path.join(pidDir, `relay-${port}.pid`);
let retried = false;

function onListenError(e) {
  // 上一条 ssh 断得突然时，旧中继要等 sshd 察觉才会退，端口还被它占着。认得出是自己
  // （pid 文件 + cmdline 里有我们的标记）就直接请它走，再试一次。
  if (e.code === "EADDRINUSE" && !retried && reclaim()) {
    retried = true;
    setTimeout(() => server.listen(port, "127.0.0.1"), 400);
    return;
  }
  process.stdout.write(`${PROTO} ERROR ${e.code || "EUNKNOWN"} ${e.message}\n`);
  process.exit(2);
}

function reclaim() {
  try {
    const pid = Number(fs.readFileSync(pidFile, "utf8").trim());
    if (!Number.isInteger(pid) || pid <= 1 || pid === process.pid) return false;
    let cmdline = "";
    try { cmdline = fs.readFileSync(`/proc/${pid}/cmdline`, "latin1"); } catch { return false; }
    if (!cmdline.includes(PROTO)) return false;
    process.kill(pid, "SIGTERM");
    return true;
  } catch {
    return false;
  }
}

server.listen(port, "127.0.0.1", () => {
  try {
    fs.mkdirSync(pidDir, { recursive: true });
    fs.writeFileSync(pidFile, String(process.pid));
  } catch { /* 写不了就算了，只影响下次抢端口 */ }
  ready = true;
  process.stdout.write(`${PROTO} READY ${server.address().port}\n`);
});

// ---------------------------------------------------------------- 收帧

let inBuf = Buffer.alloc(0);
// 某条连接写不动了就整体停读 stdin：一条 ssh 会话里没有第二个可以停的地方。
const blocked = new Set();

process.stdin.on("data", (chunk) => {
  inBuf = inBuf.length ? Buffer.concat([inBuf, chunk]) : chunk;
  while (inBuf.length >= 9) {
    const type = inBuf[0];
    const id = inBuf.readUInt32BE(1);
    const len = inBuf.readUInt32BE(5);
    if (inBuf.length < 9 + len) break;
    const payload = inBuf.subarray(9, 9 + len);
    inBuf = inBuf.subarray(9 + len);
    const s = socks.get(id);
    if (!s) continue;
    if (type === DATA) {
      if (!s.write(Buffer.from(payload)) && !blocked.has(id)) {
        blocked.add(id);
        process.stdin.pause();
        s.once("drain", () => {
          blocked.delete(id);
          if (blocked.size === 0) process.stdin.resume();
        });
      }
    } else if (type === EOF) {
      s.end();
    } else if (type === CLOSE) {
      socks.delete(id);
      blocked.delete(id);
      if (blocked.size === 0) process.stdin.resume();
      s.destroy();
    }
  }
});
process.stdin.on("end", () => process.exit(0));
process.stdin.on("error", () => process.exit(0));

process.on("SIGTERM", () => process.exit(0));
process.on("exit", () => {
  if (!ready) return;
  try {
    if (fs.readFileSync(pidFile, "utf8").trim() === String(process.pid)) fs.unlinkSync(pidFile);
  } catch { /* 无所谓 */ }
});
