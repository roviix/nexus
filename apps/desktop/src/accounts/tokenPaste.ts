/**
 * 把凭证页上那一串整理成 FlyCursor / Cursor IDE 的 access token 框能收的裸 JWT。
 *
 * 两个框看的是外壳，不看 JWT 里的 `type`：
 * - access token 框要 `eyJ` 开头的裸 JWT；
 * - session token 框要 `user_xxx::eyJ…` 这个 cookie 形状。
 *
 * 把 cookie 整段贴进 access token 框，开头不是 `eyJ`，直接被拒。
 * 反过来，cookie 里的 JWT 若是 `type=web`，换个壳也还是 web：写进 IDE 会掉登录，
 * 拿去走登录批准会 404 / 401。这一层外壳拆不开，调用方得自己看 `accessTokenType`。
 */

/** `user_xxx::eyJ…` 或裸 `eyJ…` 里的那把 JWT。对不上就 `null`。 */
export function bareAccessJwt(raw: string): string | null {
  const text = raw.trim();
  if (!text) return null;
  const jwt = text.includes("::") ? text.slice(text.lastIndexOf("::") + 2) : text;
  const parts = jwt.split(".");
  if (parts.length !== 3 || !jwt.startsWith("eyJ")) return null;
  if (parts.some((p) => p.length === 0)) return null;
  return jwt;
}

/**
 * 凭证页上给 FlyCursor 的那一句。没有可说的 type 就不说，免得把还没解析出来的号说错。
 */
export function flycursorHint(type: string | null | undefined): string | null {
  if (type === "session") {
    return "给 FlyCursor 的 access token 框贴 eyJ 开头的裸 JWT：显示上面这一行，点「复制 access token」。整段 user_xxx::… 是网站 cookie，贴进那个框，或再点它的「获取 accessToken」，都会失败。";
  }
  if (type === "web") {
    return "这把是 type=web 的网站会话。FlyCursor 的两个框都只看外壳、不改 JWT 里的 type：拿去换桌面 token 会 404（登录批准没完成）或 401（会话已被吊销，哪怕 exp 还没到）。先换成桌面 session，再复制裸 JWT。写进 Cursor 会掉登录。";
  }
  return null;
}
