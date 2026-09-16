import { describe, expect, it } from "vitest";
import { looksLikeLookupPaste, matchLookup, parseLookup } from "./lookup";

describe("parseLookup", () => {
  it("一行一个、逗号、空格混着都能捞出来，顺序按出现先后", () => {
    expect(parseLookup("a@x.com\nb@x.com, c@x.com d@x.com;e@x.com")).toEqual([
      "a@x.com",
      "b@x.com",
      "c@x.com",
      "d@x.com",
      "e@x.com",
    ]);
  });

  it("邮箱----密码 这种导出清单只认邮箱那一段", () => {
    expect(parseLookup("a@x.com----P@ss----word\nb@x.com----eyJhbGci.abc")).toEqual(["a@x.com", "b@x.com"]);
  });

  it("一段 JSON 也行", () => {
    expect(parseLookup('{"accounts":[{"email":"A@X.com","refreshToken":"rt"},{"email":"b@x.com"}]}')).toEqual([
      "a@x.com",
      "b@x.com",
    ]);
  });

  it("大小写不敏感、去重", () => {
    expect(parseLookup("A@X.com\na@x.com\nb@x.com")).toEqual(["a@x.com", "b@x.com"]);
  });

  it("没有邮箱就是空表", () => {
    expect(parseLookup("")).toEqual([]);
    expect(parseLookup("随便一些字 没有 at 符号")).toEqual([]);
  });
});

describe("matchLookup", () => {
  const list = [
    { email: "a@x.com", archivedAt: null },
    { email: "B@x.com", archivedAt: "2026-09-01T00:00:00Z" },
    { email: "c@x.com", archivedAt: null },
  ];

  it("找到的按粘贴顺序排，缺的单独列出，归档的另外点名", () => {
    const r = matchLookup(list, ["c@x.com", "zz@x.com", "b@x.com", "a@x.com"]);
    expect(r.found.map((a) => a.email)).toEqual(["c@x.com", "B@x.com", "a@x.com"]);
    expect(r.missing).toEqual(["zz@x.com"]);
    expect(r.archived.map((a) => a.email)).toEqual(["B@x.com"]);
  });

  it("空清单什么都没有", () => {
    expect(matchLookup(list, [])).toEqual({ found: [], archived: [], missing: [] });
  });
});

describe("looksLikeLookupPaste", () => {
  it("两个以上邮箱才算清单；单个邮箱还是普通搜索", () => {
    expect(looksLikeLookupPaste("a@x.com")).toBe(false);
    expect(looksLikeLookupPaste("a@x.com\nb@x.com")).toBe(true);
    expect(looksLikeLookupPaste("a@x.com, b@x.com")).toBe(true);
    expect(looksLikeLookupPaste("outlook")).toBe(false);
  });
});
