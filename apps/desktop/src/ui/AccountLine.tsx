/**
 * 一个平台账号在列表里长什么样 —— **所有账号入口共用同一副骨架**。
 *
 * 账号库、切号池、网关池、概览列的都是平台身份。平台 adapter 决定下面的用量长什么样；
 * 这一层只负责身份、徽章、状态、用量区和动作在每张卡上落到同一坐标。
 *
 * 卡片是**窄卡四段**，摆在一个多列自适应的网格里（`.accts`）：
 *
 *   ● arvid.pfeffer@outlook.com                   [Pro]
 *   Bot     ───────────                             0%
 *   Auto    ██████████──                           57%
 *   API     ████████████                          100%
 *   ─────────────────────────────────────────────────
 *   Bot 6 天后重置 · 月账期 3 天后重置        24 分钟前
 *   按需 $27.30 / $500                       [⟳] [切换]
 *
 * 为什么是这个形状 —— 上一版是**整宽等宽四列**，论证是「四个百分比在所有卡片上落在
 * 同一横坐标，眼睛竖着扫就能比」。那个论证本身没错，错在没人给内容区设上限：`.main`
 * 只有 padding，1440px 窗口下每列宽到 264px，于是每根 2px 的条子成了 264×2 的发丝线，
 * 四十个号就是 160 根完美对齐的横线 —— 那不是「像格子」，那就是格子。
 *
 * 所以改成：**卡片定宽（320-420px）、能排几列排几列**，每个桶收成「标签 · 短条 · 数值」
 * 同一行。条子降到 ~180px，一屏能看的号反而多了两三倍。注意 §14.18 否掉的是「四**列**
 * 挤进半幅」——列窄了、底下那行重置时刻放不下、于是对不齐；内联行**没有「底下那一行」**，
 * 三段都在同一行里，那个失败模式在这里不成立。
 *
 * 总额度不在卡上（见 `cardBuckets`）：它是结论，而结论由左上那颗点在说。
 */
import type { KeyboardEvent, MouseEvent, ReactNode } from "react";
import type { AccountUsage } from "../ipc/types";
import {
  cardBuckets,
  meterColor,
  meterWidth,
  onDemandParts,
  pctText,
  resetInShort,
  shortDateTime,
  type BucketView,
} from "./usage";

export type RailTone = "ok" | "warn" | "bad" | "none";

export function AccountLine({
  tone = "none",
  title,
  badges,
  note,
  actions,
  quota,
  resets,
  spend,
  stamp,
  dimmed,
  highlighted,
  current,
  onOpen,
  openLabel,
}: {
  /** 决定标题前那颗圆点的颜色。它替总额度那一条说话。 */
  tone?: RailTone;
  /** 平台身份（Cursor 此刻是邮箱；别的平台可以是用户名）。窄卡上会截断，全称给 `title`。 */
  title: ReactNode;
  /**
   * 紧跟在身份后面的徽章：订阅档、以及「当前登录」这类身份标。
   *
   * **贴着身份放，不推到最右边。** 档位是这个号的属性；甩到行尾之后，
   * 眼睛要横穿整行才能把「谁」和「什么档」接起来。
   */
  badges?: ReactNode;
  /** 状态一类的小字，落在末行开头。**不放备注、不放来源** —— 见 AccountCard 的注释。 */
  note?: ReactNode;
  /** 末行右下角的动作。整卡已经是「详情」那个按钮，所以这里只放别的事。 */
  actions?: ReactNode;
  /** 仪表区，一般是 `<QuotaStrip>`。 */
  quota?: ReactNode;
  /** 末行左上：两个倒计时，一般是 `<AccountResets>`。 */
  resets?: ReactNode;
  /** 末行左下：按需计费那一句（`<AccountSpend>`）。 */
  spend?: ReactNode;
  /** 末行右上：取数时刻。这些数字的脚注，不是又一个指标。 */
  stamp?: ReactNode;
  /** 失效的号：压暗内容但**不隐藏**，hover 时还原 —— 用户得看见它、还得能删它。 */
  dimmed?: boolean;
  /** 抽屉正打开、或批量里刚选中的那一张。 */
  highlighted?: boolean;
  /** Cursor 此刻登着的就是它。和 `highlighted` 分开：关掉抽屉也还在。 */
  current?: boolean;
  onOpen?: () => void;
  openLabel?: string;
}) {
  const classes = ["acct", `tone-${tone}`];
  if (dimmed) classes.push("is-dead");
  if (highlighted) classes.push("is-open");
  if (current) classes.push("is-in-use");
  if (onOpen) classes.push("is-clickable");

  // 「详情」不再是一枚文字键或箭头：整张卡就是那个按钮，靠指针和 hover 的一层提亮说话。
  const open = onOpen
    ? {
        role: "button",
        tabIndex: 0,
        "aria-label": openLabel,
        onClick: onOpen,
        onKeyDown: (e: KeyboardEvent<HTMLElement>) => {
          // 卡片内还有刷新 / 切换 / 删除等真按钮。它们的 Enter / Space 会冒泡到 article；
          // 只认 article 自己拿到焦点的键盘事件，不能按一个按钮顺手又打开抽屉。
          if (e.target !== e.currentTarget) return;
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onOpen();
          }
        },
      }
    : {};

  const hasFoot = Boolean(note || resets || spend || stamp || actions);

  return (
    <article className={classes.join(" ")} aria-current={current ? "true" : undefined} {...open}>
      <div className="acct-head">
        <span className="acct-dot" />
        {/* 窄卡上邮箱会截尾。这是收窄换来的，接受它，但全称得悬停看得到。 */}
        <span className="acct-identity truncate" title={typeof title === "string" ? title : undefined}>
          {title}
        </span>
        {badges}
      </div>
      {quota ? <div className="acct-quota">{quota}</div> : null}
      {hasFoot ? (
        // 上一行整宽给倒计时；下一行左边按需计费、右边「取数时刻 + 动作」。
        // 时刻紧挨着刷新键放，是因为它俩说的是同一件事：这些数字有多新、按一下就能更新。
        <div className="acct-foot">
          {note || resets ? (
            <span className="qf-full truncate">
              {note ? (
                <>
                  {note}
                  {resets ? <i className="qf-sep">·</i> : null}
                </>
              ) : null}
              {resets}
            </span>
          ) : null}
          {/* 这两格永远成对出现，缺内容也留着 —— 少一个，网格自动落位就会把右边那格
              顶到左列去，动作键于是跑到卡片中间。 */}
          <span className="qf-l truncate">{spend}</span>
          <span className="qf-r">
            {stamp ? <span className="qf-stamp">{stamp}</span> : null}
            {actions ? (
              // 卡片整体可点，所以按钮要把点击拦下来，否则按「刷新」会顺带把抽屉也打开。
              <span
                className="acct-actions"
                onClick={(e: MouseEvent) => e.stopPropagation()}
                role="presentation"
              >
                {actions}
              </span>
            ) : null}
          </span>
        </div>
      ) : null}
    </article>
  );
}

/* ── 仪表区 ───────────────────────────────────────────────────────────────── */

/**
 * 三个桶各占一行：**标签 · 条 · 数值**。
 *
 * Auto / API 是分开计量的，任一打满那类模型就停了，所以两个都得摆。数字过了阈值才染色
 * （和条子同一套阈值）—— 三个数字全染色会糊成一片；一点没用的那个反而要压暗，
 * 它不需要注意力。总额度不在这儿，见 `cardBuckets`。
 */
export function QuotaStrip({ usage }: { usage: AccountUsage }) {
  return (
    <div className="qs">
      {cardBuckets(usage).map((b) => (
        <QuotaCell key={b.key} bucket={b} />
      ))}
    </div>
  );
}

/** 任意平台的「标签 · 条 · 数值」行。ChatGPT 两个窗口走这里，不借用 Cursor 的桶 key。 */
export function QuotaRows({
  rows,
}: {
  rows: Array<{
    key: string;
    label: string;
    hint: string;
    percent: number | null;
    note?: string;
  }>;
}) {
  return (
    <div className="qs">
      {rows.map((b) => (
        <QuotaCell key={b.key} bucket={b} />
      ))}
    </div>
  );
}

/** 只有一个总百分比时的降级形态；不能伪造 Cursor 的分桶数据。 */
export function QuotaSummary({
  label,
  percentUsed,
}: {
  label: string;
  percentUsed: number;
}) {
  return (
    <div className="qs">
      <QuotaCell
        bucket={{
          label,
          percent: percentUsed,
          hint: `${label}已用 ${pctText(percentUsed)}`,
        }}
      />
    </div>
  );
}

function QuotaCell({
  bucket,
}: {
  bucket: Pick<BucketView, "label" | "hint" | "percent" | "note">;
}) {
  const p = bucket.percent;
  const known = p != null && Number.isFinite(p);
  const tone = !known ? " is-idle" : p! > 90 ? " is-bad" : p! >= 70 ? " is-warn" : p! <= 0 ? " is-idle" : "";

  return (
    <div className="qc" title={bucket.hint}>
      <span className="qc-k">{bucket.label}</span>
      <span className="qc-track">
        <i
          style={{
            width: `${meterWidth(p)}%`,
            background: known ? meterColor(p!) : "transparent",
          }}
        />
      </span>
      <span className={bucket.note ? `qc-v is-note${tone}` : `qc-v${tone}`}>
        {bucket.note ?? (known ? `${Math.round(p!)}%` : "—")}
      </span>
    </div>
  );
}

/* ── 末行 ─────────────────────────────────────────────────────────────────── */

/**
 * 两个重置的倒计时：`月额 17 天后重置 · Bot 明天 16:00 重置`。
 *
 * 四个桶分属两套周期（Bot 按周、其余共用月账期），所以两个都要给。
 * **卡片上只给倒计时**，绝对时刻悬停可见或退进抽屉 —— 倒计时回答「还能等多久」，那是扫列表时的问题；
 * 「几点回来看」要的是精确值，点开卡片看详情。
 */
export function AccountResets({
  usage,
  weeklyLabel = "Bot",
}: {
  usage: AccountUsage;
  /** 前面那句状态已经点过 Bot 的名了（「Bot 已耗尽」），这里就别再写一遍。 */
  weeklyLabel?: string | null;
}) {
  const items: Array<{ label?: string | null; at?: number | null }> = [];
  if (usage.cycleEnd && Number.isFinite(usage.cycleEnd)) {
    items.push({ label: "月额", at: usage.cycleEnd });
  }
  if (usage.bot?.resetAt && Number.isFinite(usage.bot.resetAt)) {
    items.push({ label: weeklyLabel, at: usage.bot.resetAt });
  }
  return <FootResets items={items} />;
}

/** 末行倒计时。各平台自己点名窗口，这里只负责「标签 + 多久后重置」和中间的点。 */
export function FootResets({
  items,
}: {
  items: Array<{ label?: string | null; at?: number | null }>;
}) {
  const shown = items.filter((i): i is { label?: string | null; at: number } => i.at != null && Number.isFinite(i.at));
  if (shown.length === 0) return null;
  return (
    <>
      {shown.map((i, idx) => (
        <span key={`${i.label ?? ""}-${i.at}`}>
          {idx > 0 ? <i className="qf-sep">·</i> : null}
          <ResetItem k={i.label} at={i.at} />
        </span>
      ))}
    </>
  );
}

function ResetItem({ k, at }: { k?: string | null; at: number }) {
  return (
    <span className="qf-i" title={`重置于 ${shortDateTime(at)}`}>
      {k ? <span className="qf-k">{k}</span> : null}
      <span className="qf-in">{resetInShort(at)}</span>
    </span>
  );
}

/**
 * 按需计费那一句：`按需 $21.15 · 不封顶`。
 *
 * 和上一行同一副骨架 —— 标签弱、值强、中间一个点。整句一个灰度的话，两行小字就成了
 * 一段没有重音的话，眼睛不知道该落在哪。
 */
export function AccountSpend({ usage }: { usage: AccountUsage }) {
  const { k, v, sub } = onDemandParts(usage);
  return (
    <span className="qf-i">
      <span className="qf-k">{k}</span>
      {v ? <span className="qf-in">{v}</span> : null}
      {sub ? (
        <>
          <i className="qf-sep">·</i>
          <span className="qf-k">{sub}</span>
        </>
      ) : null}
    </span>
  );
}

/** 额度那一段里的一句话，比如「还没查过用量」。 */
export function QuotaBlank({ children }: { children: ReactNode }) {
  return <span className="acct-blank">{children}</span>;
}
