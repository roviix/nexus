import { describe, expect, it } from "vitest";
import { bareAccessJwt, flycursorHint } from "./tokenPaste";

const JWT = "eyJhbGciOiJIUzI1NiJ9.eyJ0eXBlIjoic2Vzc2lvbiJ9.sig";

describe("bareAccessJwt", () => {
  it("cookie 形状只留下裸 JWT", () => {
    expect(bareAccessJwt(`user_01ABC::${JWT}`)).toBe(JWT);
  });

  it("本来就是裸 JWT 时原样返回", () => {
    expect(bareAccessJwt(JWT)).toBe(JWT);
  });

  it("不是 JWT 的外壳不凑", () => {
    expect(bareAccessJwt("user_01ABC::not-a-jwt")).toBeNull();
    expect(bareAccessJwt("")).toBeNull();
    expect(bareAccessJwt("eyJonly.twoparts")).toBeNull();
  });
});

describe("flycursorHint", () => {
  it("session 告诉人复制裸 JWT，web 告诉人先转换", () => {
    expect(flycursorHint("session")).toContain("复制 access token");
    expect(flycursorHint("web")).toContain("换成桌面 session");
    expect(flycursorHint("web")).toContain("404");
    expect(flycursorHint(null)).toBeNull();
    expect(flycursorHint("api_key_token")).toBeNull();
  });
});
