#!/usr/bin/env python3
"""从 extract-inference-proto.py 的 JSON 生成 prost-derive 的 Rust（src/proto.rs）。

    python3 ../../../gateway/scripts/extract-inference-proto.py > /dev/null   # 写 /tmp/inference_proto.json
    cp /tmp/inference_proto.json proto/inference-<cursor 版本>.json
    python3 scripts/gen-proto.py proto/inference-<cursor 版本>.json > src/proto.rs
    cargo fmt -p nexus-gateway

字段号、字段名、类型全部来自机器提取的 JSON，这个脚本一行都不手抄——写错一个字段号
不会报错，只会让上游回一个语焉不详的 400。它只掌握两类 JSON 里解不出来的知识
（提取器碰到压缩后的变量名 `<Yd>` 这种就留了个占位）：哪个字段是 google.protobuf 的
Struct / Value，哪个 enum 字段对应哪个 enum。两张表都按 (消息, 字段) 索引而不是按
`<Yd>` 这种会随每次 Cursor 构建变的名字索引。

遇到表里没有的未解析引用**直接报错退出**：Cursor 升级加了新字段时要人看一眼，
不能静默糊过去。
"""
import json
import re
import sys

# (消息, 字段) → google.protobuf 类型
WELL_KNOWN = {
    ("InferenceAgentTool", "parameters"): "Struct",
    ("InferenceNamedProviderDefinedTool", "options"): "Struct",
    ("InferenceProviderMetadataInfo", "metadata"): "Struct",
    ("InferenceToolCall", "args"): "Struct",
    ("InferenceToolResultPart", "result"): "Value",
}
# (消息, 字段) → enum 类型名（提取器把所有 enum 引用都印成 `<A>`，分不出是哪个）
ENUM_OF = {
    ("InferenceCoreMessage", "role"): "InferenceMessageRole",
    ("InferenceResponseMessage", "role"): "InferenceMessageRole",
    ("InferenceProviderWarning", "trigger"): "InferenceProviderWarningTrigger",
    ("InferenceStreamError", "error_type"): "InferenceStreamErrorType",
    ("InferenceStreamRequest", "inference_reason"): "InferenceReason",
}
SCALAR = {
    "string": "::prost::alloc::string::String",
    "bool": "bool",
    "int32": "i32", "sint32": "i32", "sfixed32": "i32",
    "int64": "i64", "sint64": "i64", "sfixed64": "i64",
    "uint32": "u32", "fixed32": "u32",
    "uint64": "u64", "fixed64": "u64",
    "float": "f32",
    "double": "f64",
    "bytes": "::prost::alloc::vec::Vec<u8>",
}
RUST_KEYWORDS = {
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
    "move", "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait",
    "true", "type", "unsafe", "use", "where", "while",
}


def die(msg):
    print(f"gen-proto: {msg}", file=sys.stderr)
    sys.exit(1)


def short(type_name):
    return type_name.split(".")[-1]


def camel(snake_name):
    return "".join(p[:1].upper() + p[1:] for p in snake_name.split("_") if p)


def snake(camel_name):
    return re.sub(r"(?<!^)(?=[A-Z])", "_", camel_name).lower()


def ident(name):
    return f"r#{name}" if name in RUST_KEYWORDS else name


def scalar_attr(t):
    return 'bytes = "vec"' if t == "bytes" else t


def resolve_message_type(msg, field, in_oneof_mod):
    """字段的消息类型 → Rust 路径。未解析引用查 WELL_KNOWN，查不到就报错。"""
    t = field["type"]
    if t.startswith("<"):
        wk = WELL_KNOWN.get((msg, field["name"]))
        if not wk:
            die(f"{msg}.{field['name']} 的消息类型 {t} 解不出来，且不在 WELL_KNOWN 表里")
        return f"::prost_types::{wk}"
    name = short(t)
    return f"super::{name}" if in_oneof_mod else name


def resolve_enum_type(msg, field):
    t = field["type"]
    if t.startswith("<"):
        e = ENUM_OF.get((msg, field["name"]))
        if not e:
            die(f"{msg}.{field['name']} 的 enum 类型 {t} 解不出来，且不在 ENUM_OF 表里")
        return e
    return short(t)


def plain_field(msg, f):
    """非 oneof 字段 → (prost 属性, Rust 类型)。"""
    kind = f["kind"]
    rep, opt = f.get("repeated", False), f.get("opt", False)
    if kind == "scalar":
        t = f["type"]
        if t not in SCALAR:
            die(f"{msg}.{f['name']} 未知标量类型 {t}")
        attr, ty = scalar_attr(t), SCALAR[t]
        if rep:
            return f"{attr}, repeated", f"::prost::alloc::vec::Vec<{ty}>"
        if opt:
            return f"{attr}, optional", f"::core::option::Option<{ty}>"
        return attr, ty
    if kind == "message":
        ty = resolve_message_type(msg, f, in_oneof_mod=False)
        if rep:
            return "message, repeated", f"::prost::alloc::vec::Vec<{ty}>"
        # prost 里单个消息字段一律 Option（proto3 消息字段有显式存在性），与 opt 与否无关。
        return "message, optional", f"::core::option::Option<{ty}>"
    if kind == "enum":
        e = resolve_enum_type(msg, f)
        attr = f'enumeration = "{e}"'
        if rep:
            return f"{attr}, repeated", "::prost::alloc::vec::Vec<i32>"
        if opt:
            return f"{attr}, optional", "::core::option::Option<i32>"
        return attr, "i32"
    if kind == "map":
        m = re.fullmatch(r"map<(\w+),(\w+)>", f["type"].replace(" ", ""))
        if not m:
            die(f"{msg}.{f['name']} map 类型解不出来：{f['type']}")
        k, v = m.group(1), m.group(2)
        if k not in SCALAR or v not in SCALAR:
            die(f"{msg}.{f['name']} 只支持标量 map，遇到 {f['type']}")
        return f'map = "{k}, {v}"', f"::std::collections::HashMap<{SCALAR[k]}, {SCALAR[v]}>"
    die(f"{msg}.{f['name']} 未知字段种类 {kind}")


def oneof_variant(msg, f):
    """oneof 分支 → (prost 属性, 变体载荷类型)。分支不会是 repeated / optional。"""
    kind = f["kind"]
    if kind == "scalar":
        t = f["type"]
        return scalar_attr(t), SCALAR[t]
    if kind == "message":
        return "message", resolve_message_type(msg, f, in_oneof_mod=True)
    if kind == "enum":
        return f'enumeration = "super::{resolve_enum_type(msg, f)}"', "i32"
    die(f"{msg}.{f['name']} oneof 分支不支持种类 {kind}")


def gen_message(name, spec):
    fields = sorted(spec["fields"], key=lambda x: x["no"])
    oneofs, plain = {}, []
    for f in fields:
        (oneofs.setdefault(f["oneof"], []) if f.get("oneof") else plain).append(f)

    out = ["#[derive(Clone, PartialEq, ::prost::Message)]", f"pub struct {name} {{"]
    for f in plain:
        attr, ty = plain_field(name, f)
        out.append(f'    #[prost({attr}, tag = "{f["no"]}")]')
        out.append(f"    pub {ident(f['name'])}: {ty},")
    for oname, ofs in oneofs.items():
        tags = ", ".join(str(f["no"]) for f in ofs)
        path = f"{snake(name)}::{camel(oname)}"
        out.append(f'    #[prost(oneof = "{path}", tags = "{tags}")]')
        out.append(f"    pub {ident(oname)}: ::core::option::Option<{path}>,")
    out.append("}")

    if oneofs:
        out.append(f"/// `{name}` 的 oneof。")
        out.append(f"pub mod {snake(name)} {{")
        for oname, ofs in oneofs.items():
            out.append("    #[derive(Clone, PartialEq, ::prost::Oneof)]")
            out.append(f"    pub enum {camel(oname)} {{")
            for f in ofs:
                attr, ty = oneof_variant(name, f)
                out.append(f'        #[prost({attr}, tag = "{f["no"]}")]')
                out.append(f"        {camel(f['name'])}({ty}),")
            out.append("    }")
        out.append("}")
    return "\n".join(out)


def gen_enum(name, spec):
    prefix = snake(name).upper() + "_"
    out = [
        "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]",
        "#[repr(i32)]",
        f"pub enum {name} {{",
    ]
    for v in sorted(spec["fields"], key=lambda x: x["no"]):
        raw = v["name"]
        stripped = raw[len(prefix):] if raw.startswith(prefix) else raw
        if not stripped or stripped[0].isdigit():
            stripped = raw
        out.append(f"    {camel(stripped.lower())} = {v['no']},")
    out.append("}")
    return "\n".join(out)


def main():
    if len(sys.argv) != 2:
        die("用法：gen-proto.py <inference_proto.json>")
    src = sys.argv[1]
    spec = json.load(open(src, encoding="utf-8"))

    header = f"""//! `aiserver.v1.Inference*` 的 protobuf 类型。
//!
//! **生成文件，不要手改。** 由 `scripts/gen-proto.py` 从 `{src}` 生成——那份 JSON 是
//! `gateway/scripts/extract-inference-proto.py` 对 Cursor desktop bundle 机器提取的结果，
//! 字段号一个都没经过人手。Cursor 升级后按 `scripts/gen-proto.py` 头部的步骤重跑，
//! 拿 git diff 看协议动了什么。
//!
//! 只有两处是人给的知识（都在生成器里、按 (消息, 字段) 索引）：哪些字段是
//! google.protobuf 的 Struct / Value，哪些 enum 字段对应哪个 enum。
#![allow(clippy::large_enum_variant, clippy::enum_variant_names)]
"""
    blocks = [header]
    for full_name in sorted(spec):
        name = short(full_name)
        body = spec[full_name]
        if body["kind"] == "message":
            blocks.append(gen_message(name, body))
        elif body["kind"] == "enum":
            blocks.append(gen_enum(name, body))
        else:
            die(f"{full_name} 未知种类 {body['kind']}")
    print("\n\n".join(blocks))


if __name__ == "__main__":
    main()
