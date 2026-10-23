import fs from 'fs';
import path from 'path';

const shotsDir = path.resolve('docs/images/account_shots');
const outHtmlPathWorkspace = path.resolve('docs/account-cards-prototype.html');
const outHtmlPathArtifact = '/Users/zhongshangwu/.gemini/antigravity/brain/6430926c-b3e3-4592-a603-abae501a9f45/account-cards-prototype.html';

function getBase64(fileName) {
  const filePath = path.join(shotsDir, fileName);
  if (!fs.existsSync(filePath)) return '';
  const buf = fs.readFileSync(filePath);
  return `data:image/png;base64,${buf.toString('base64')}`;
}

const platforms = [
  {
    id: "chatgpt",
    name: "ChatGPT",
    desc: "OpenAI Codex / Spark 订阅号 (本地网关核心号源之一)",
    icon: "https://upload.wikimedia.org/wikipedia/commons/0/04/ChatGPT_logo.svg",
    vendorLogo: "ChatGPT",
    cardImg: getBase64("chatgpt_card.png"),
    drawerImg: getBase64("chatgpt_drawer.png"),
    pageImg: getBase64("chatgpt_page.png"),
    mockCard: {
      title: "arvid.pfeffer@outlook.com",
      plan: "Pro",
      planClass: "plan plan-pro",
      gatewayState: "网关",
      rows: [
        { label: "Codex 5 小时", reset: "17:30 重置", val: "28%", pct: 28, tone: "ok" },
        { label: "Codex 7 天", reset: "6 天后重置", val: "55%", pct: 55, tone: "ok" },
        { label: "Spark 5 小时", reset: "17:30 重置", val: "100%", pct: 100, tone: "bad" },
        { label: "Spark 7 天", reset: "6 天后重置", val: "96%", pct: 96, tone: "bad" }
      ],
      footLeft: "订阅还剩 12 天 · 23 天前添加",
      footRight: "21 分钟前更新"
    },
    currentSurface: [
      { area: "Header", item: "身份与标识", desc: "邮箱 / 账号标识（截短+悬停全称）" },
      { area: "Header", item: "套餐徽标", desc: "Pro / Plus / Team / Free 等级胶囊" },
      { area: "Header", item: "网关状态", desc: "「网关」参与轮询接力或停用状态" },
      { area: "Body", item: "Codex 5h 滚动窗口", desc: "百分比进度条 + 行内重置倒计时（例如 17:30 重置）" },
      { area: "Body", item: "Codex 7d 滚动窗口", desc: "百分比进度条 + 行内重置倒计时（例如 6 天后重置）" },
      { area: "Body", item: "Spark 模型分桶", desc: "独立 5h 与 7d 额度条，支持不同模型/特性的多桶展示" },
      { area: "Footer", item: "订阅与添加状态", desc: "订阅剩余天数 / 添加时间 / 刷新按钮" }
    ],
    upstreamData: [
      { field: "email", type: "String", source: "OAuth / JWT", desc: "用户主登录邮箱", onCard: "显示在 Header", redesign: "核心身份标识，建议保留" },
      { field: "accountRef", type: "String", source: "wham/accounts", desc: "上游内部账号标识 (chatgpt_account_id)", onCard: "隐藏 (抽屉展示)", redesign: "技术字段，留在二级抽屉即可" },
      { field: "userId", type: "UUID", source: "wham/accounts", desc: "上游用户全局唯一标识 UUID", onCard: "隐藏 (抽屉展示)", redesign: "技术排障用，二级抽屉展示" },
      { field: "organizationId / Title", type: "String", source: "wham/accounts", desc: "工作区或组织名称 (如 Personal / Org)", onCard: "隐藏 (抽屉展示)", redesign: "多组织号源时可在抽屉切换" },
      { field: "planType", type: "String", source: "Billing", desc: "套餐档位：pro / plus / team / free", onCard: "Header 徽标", redesign: "核心元数据，胶囊徽标保留" },
      { field: "primary (5h Window)", type: "Object", source: "/wham/usage", desc: "usedPercent (已用百分比), resetAtMs (重置时刻), windowMinutes (300)", onCard: "Body 进度条 + 重置时间", redesign: "高频关注指标，高优先级" },
      { field: "secondary (7d Window)", type: "Object", source: "/wham/usage", desc: "usedPercent (已用百分比), resetAtMs (重置时刻), windowMinutes (10080)", onCard: "Body 进度条 + 重置时间", redesign: "周限额度，核心指标" },
      { field: "additional (Feature Buckets)", type: "Array", source: "/wham/usage", desc: "Spark / Codex 各 feature 的 rateLimit 分桶", onCard: "Body 展开行", redesign: "当前全展开较长，可考虑折叠或二级页面" },
      { field: "credits", type: "Object", source: "/wham/usage", desc: "Codex 点数 (hasCredits, balance, resetAvailable)", onCard: "仅在有点数时透出", redesign: "有点数时替代或补充滚动窗口" },
      { field: "billing.expiresAt", type: "ISO Date", source: "Stripe/Billing", desc: "订阅账期截止日期与是否续费 (willRenew)", onCard: "Footer 简短剩余天数", redesign: "保持低噪，仅到期提醒" },
      { field: "accessExpiresAt / hasRefresh", type: "ISO Date", source: "OAuth", desc: "短票有效期及 Refresh Token 续期状态", onCard: "状态点健康度", redesign: "掉登录时置灰警示" },
      { field: "traffic (90d)", type: "Object", source: "Local SQLite", desc: "本机网关 90 天请求次数、总 Token 数、报错数", onCard: "抽屉详情", redesign: "网关运维指标，二级抽屉" }
    ],
    designCritique: {
      pros: "信息完备，滚动窗口和重置时间已内联整合，消除了多余 footer 重复文案。",
      cons: "当账户包含 Codex + Spark 等多个模型分桶时，单张卡片纵向高度偏高（4+ 条进度条），在卡片列表中有视觉压迫感。",
      suggestion: "外层卡片可默认只露最重要的主窗口（如 5h + 7d），次要模型（Spark 等）或高级分桶通过点击进入二级页面或右侧抽屉查看。"
    }
  },
  {
    id: "claude",
    name: "Claude",
    desc: "Anthropic Claude Max / Pro 订阅号与 API 凭证",
    icon: "https://upload.wikimedia.org/wikipedia/commons/e/e0/Anthropic_logo.svg",
    vendorLogo: "Claude",
    cardImg: getBase64("claude_card.png"),
    drawerImg: getBase64("claude_drawer.png"),
    pageImg: getBase64("claude_page.png"),
    mockCard: {
      title: "ada@anthropic.com",
      plan: "Max",
      planClass: "plan plan-team",
      gatewayState: "网关",
      rows: [
        { label: "每周额度", reset: "后天 12:44 重置", val: "64%", pct: 64, tone: "ok" }
      ],
      footLeft: "20 天前添加",
      footRight: "12 分钟前更新"
    },
    currentSurface: [
      { area: "Header", item: "身份与标识", desc: "Anthropic 账号邮箱" },
      { area: "Header", item: "套餐徽标", desc: "Max / Pro / Free" },
      { area: "Body", item: "周期额度", desc: "「每周额度」/「每月额度」百分比进度条 + 重置时刻" },
      { area: "Footer", item: "状态与时间", desc: "添加时间 · 更新时间 · 刷新动作" }
    ],
    upstreamData: [
      { field: "email", type: "String", source: "OAuth / Token", desc: "Anthropic 用户邮箱", onCard: "Header 标识", redesign: "保留" },
      { field: "planType", type: "String", source: "OAuth / Token", desc: "订阅级别 (Max, Pro, Team)", onCard: "Header 徽标", redesign: "保留" },
      { field: "authKind", type: "Enum", source: "Config", desc: "oauth (订阅号) / api_key (开发者 Key)", onCard: "徽标区分", redesign: "可更清晰呈现为「订阅」或「API Key」" },
      { field: "creditUsagePercent", type: "Number", source: "oauth_usage", desc: "当前账期用量百分比 (0-100%)", onCard: "Body 进度条", redesign: "核心指标，进度条呈现" },
      { field: "periodType", type: "String", source: "oauth_usage", desc: "账期类型：weekly (周) / monthly (月)", onCard: "Body 标签", redesign: "文案可直接精炼为「周限」或「月限」" },
      { field: "periodEnd", type: "ISO Date", source: "oauth_usage", desc: "本期额度重置精确时间戳", onCard: "行内倒计时", redesign: "保留" },
      { field: "remainingRequests / Tokens", type: "Number", source: "oauth_usage", desc: "剩余请求数 / Token 数 (若上游返回)", onCard: "抽屉", redesign: "若有数值建议克制外露于抽屉" },
      { field: "retryAfterMs", type: "Number", source: "Headers", desc: "触发速率限制时的等待冷却毫秒数", onCard: "异常状态条", redesign: "被限流时转化为醒目倒计时" }
    ],
    designCritique: {
      pros: "结构紧凑，卡片高度克制，单条进度条一目了然。",
      cons: "「每周额度」文案仍稍显普通，若能同 Antigravity 一致简化为「Weekly」或「周限」更具现代感。",
      suggestion: "可作为单指标卡片的标准排版参考；抽屉内补充模型列表与网关路由规则。"
    }
  },
  {
    id: "antigravity",
    name: "Antigravity (反重力)",
    desc: "Google AI Pro / Ultra 订阅与 Gemini / Claude 额度通道",
    icon: "https://upload.wikimedia.org/wikipedia/commons/2/2d/Google-favicon-2015.png",
    vendorLogo: "Google",
    cardImg: getBase64("antigravity_card.png"),
    drawerImg: getBase64("antigravity_drawer.png"),
    pageImg: getBase64("antigravity_page.png"),
    mockCard: {
      title: "ada@gmail.com",
      plan: "Google AI Pro",
      planClass: "plan plan-pro",
      gatewayState: "网关",
      rows: [
        { label: "Gemini 5h", reset: "2 小时后重置", val: "28%", pct: 28, tone: "ok" },
        { label: "Gemini Weekly", reset: "20 小时后重置", val: "0%", pct: 0, tone: "ok" },
        { label: "Others 5h", reset: "1 小时后重置", val: "59%", pct: 59, tone: "ok" },
        { label: "Others Weekly", reset: "20 小时后重置", val: "0%", pct: 0, tone: "ok" }
      ],
      footLeft: "20 天前添加",
      footRight: "6 分钟前更新"
    },
    currentSurface: [
      { area: "Header", item: "身份与标识", desc: "Google 账号邮箱" },
      { area: "Header", item: "套餐徽标", desc: "Google AI Pro / Ultra / 体验版" },
      { area: "Body", item: "Gemini 5h / Weekly", desc: "Gemini 专属模型用量双周期（5 小时滚动 + 周上限）" },
      { area: "Body", item: "Others 5h / Weekly", desc: "第三方接入模型用量双周期（Claude 等）" },
      { area: "Footer", item: "自定义项目/状态", desc: "仅在非默认 GCP 项目时展示 ProjectID，无多余噪音" }
    ],
    upstreamData: [
      { field: "email", type: "String", source: "Google OAuth", desc: "Google 主账号邮箱", onCard: "Header 标识", redesign: "核心标识" },
      { field: "projectId", type: "String", source: "Google Cloud", desc: "GCP 配额项目 ID (默认 aicode-consumer...)", onCard: "非默认时在 Footer 显示", redesign: "默认项目隐藏，设计非常克制" },
      { field: "tier / tierName", type: "String", source: "Google AI API", desc: "订阅级别：Google AI Pro / Ultra / Free", onCard: "Header 徽标", redesign: "保留" },
      { field: "percentRemaining / Used", type: "Number", source: "Google AI API", desc: "总额度使用率与剩余率", onCard: "各分桶使用", redesign: "保留" },
      { field: "modelBuckets", type: "Array", source: "Google AI API", desc: "分模型配额 (gemini-3.1-pro, claude-sonnet-4-6 等的 remainingFraction, resetTime)", onCard: "归类为 Gemini 与 Others", redesign: "用户认可的两大分类结构" },
      { field: "resetsAt", type: "ISO Date", source: "Google AI API", desc: "周期重置时间点 (5h 滚动窗口与 UTC 周期)", onCard: "行内倒计时", redesign: "保留" },
      { field: "accessExpiresAt", type: "ISO Date", source: "Google Session", desc: "访问令牌到期时间 (通常 20 小时内)", onCard: "抽屉详情展示", redesign: "在抽屉内显示自动续期说明" }
    ],
    designCritique: {
      pros: "已完成文字去噪，归结为 Gemini 与 Others 两组 (5h & Weekly)，排版对称规整。",
      cons: "4 条进度条整体稍微偏饱满；且 Weekly 若总是 0% 可以视觉弱化。",
      suggestion: "可提供紧凑态（仅显示 5h）与展开态切换，或者在 Weekly 尚未消耗时采用更低对比度的刻度条。"
    }
  },
  {
    id: "grok",
    name: "Grok Build (xAI)",
    desc: "xAI Grok 订阅与生图/生视频模型能力通道",
    icon: "https://upload.wikimedia.org/wikipedia/commons/5/57/X_logo_2023_%28white%29.svg",
    vendorLogo: "xAI",
    cardImg: getBase64("grok_card.png"),
    drawerImg: getBase64("grok_drawer.png"),
    pageImg: getBase64("grok_page.png"),
    mockCard: {
      title: "you@x.ai",
      plan: "SuperGrok",
      planClass: "plan plan-pro",
      gatewayState: "当前主用",
      rows: [
        { label: "每周额度", reset: "3 天后重置", val: "37%", pct: 37, tone: "ok" }
      ],
      footLeft: "生图 · 20 天前添加",
      footRight: "5 分钟前更新"
    },
    currentSurface: [
      { area: "Header", item: "身份与状态", desc: "x.ai 邮箱 · 当前主用徽标" },
      { area: "Header", item: "套餐徽标", desc: "SuperGrok / Grok Premium / Free" },
      { area: "Body", item: "周期额度", desc: "每周额度进度条 + 行内重置倒计时" },
      { area: "Footer", item: "媒体资格标识", desc: "「生图」胶囊徽标透出具备 Imagine 资格" }
    ],
    upstreamData: [
      { field: "email", type: "String", source: "xAI Web / OAuth", desc: "x.ai 账号邮箱", onCard: "Header 标识", redesign: "保留" },
      { field: "subscriptionTier", type: "String", source: "xAI Billing", desc: "订阅级别 (SuperGrok / Free)", onCard: "Header 徽标", redesign: "保留" },
      { field: "authKind", type: "Enum", source: "Config", desc: "oauth (订阅) / api_key (控制台 Key)", onCard: "抽屉", redesign: "区分个人订阅与官方 Key" },
      { field: "mediaEligible", type: "Boolean", source: "xAI Probe / Setting", desc: "是否具备 Imagine 生图与生视频资格", onCard: "Footer 「生图」小胶囊", redesign: "对多模态通道非常关键，建议常驻或在 Header 显性标注" },
      { field: "creditUsagePercent", type: "Number", source: "xAI Billing", desc: "周期额度使用率 (0-100%)", onCard: "Body 进度条", redesign: "保留" },
      { field: "periodEnd", type: "ISO Date", source: "xAI Billing", desc: "重置时间戳", onCard: "行内倒计时", redesign: "保留" },
      { field: "manifestModels", type: "Array", source: "xAI API", desc: "该号支持的具体模型 (grok-4.6, grok-imagine-video 等)", onCard: "抽屉内展示", redesign: "二级页面或抽屉可列出模型卡" }
    ],
    designCritique: {
      pros: "视觉层次清晰，生图徽标能直观告诉用户该号源能否承担绘图请求。",
      cons: "「每周额度」文案可进一步精简，进度条样式可进一步扁平化。",
      suggestion: "在抽屉中加强多模态探测结果的开关与试用演示。"
    }
  },
  {
    id: "kiro",
    name: "Kiro",
    desc: "Kiro AI 积分制订阅账号与点数池",
    icon: "https://upload.wikimedia.org/wikipedia/commons/f/fa/Apple_logo_black.svg",
    vendorLogo: "Kiro",
    cardImg: getBase64("kiro_card.png"),
    drawerImg: getBase64("kiro_drawer.png"),
    pageImg: getBase64("kiro_page.png"),
    mockCard: {
      title: "ada@kiro.dev",
      plan: "Kiro Pro",
      planClass: "plan plan-pro",
      gatewayState: "当前主用",
      rows: [
        { label: "请求配额", reset: "8 天后重置", val: "420 / 1,000", pct: 42, tone: "ok" }
      ],
      footLeft: "20 天前添加",
      footRight: "8 分钟前更新"
    },
    currentSurface: [
      { area: "Header", item: "身份与状态", desc: "用户邮箱 · 当前主用 · Kiro 图标" },
      { area: "Header", item: "套餐徽标", desc: "Kiro Pro / Team / Free" },
      { area: "Body", item: "数值积分与进度", desc: "进度条 + 具体数值 note (420 / 1,000) + 行内重置倒计时" },
      { area: "Footer", item: "试用或超额状态", desc: "若有试用剩余或超额开启，在 Footer 低调透出" }
    ],
    upstreamData: [
      { field: "email", type: "String", source: "Kiro Social / Auth", desc: "用户邮箱", onCard: "Header 标识", redesign: "保留" },
      { field: "authMethod", type: "String", source: "Kiro", desc: "登录方式：builder-id / social (Google/Github)", onCard: "抽屉", redesign: "保留" },
      { field: "baseUsed / baseLimit", type: "Number", source: "Kiro API", desc: "基础套餐已用积分与总限额", onCard: "已用点数比值", redesign: "保留" },
      { field: "used / limit", type: "Number", source: "Kiro API", desc: "综合已用总积分与上限", onCard: "Body note 420 / 1,000", redesign: "用户满意已展示具体点数" },
      { field: "percent", type: "Number", source: "Kiro API", desc: "使用百分比", onCard: "进度条宽度与颜色", redesign: "保留" },
      { field: "resetAt", type: "ISO Date", source: "Kiro API", desc: "积分额度重置日期", onCard: "行内倒计时", redesign: "已修复重复重置 Bug" },
      { field: "trialUsed / trialLimit", type: "Number", source: "Kiro API", desc: "试用期额度点数", onCard: "Footer (若有)", redesign: "可在抽屉更细致呈现" },
      { field: "bonusUsed / bonusLimit", type: "Number", source: "Kiro API", desc: "奖励赠送积分点数", onCard: "抽屉", redesign: "抽屉补充" },
      { field: "overageEnabled", type: "Boolean", source: "Kiro API", desc: "是否允许超额计费", onCard: "Footer 超额开启", redesign: "防扣费核心警示" }
    ],
    designCritique: {
      pros: "成功将纯百分比升级为具体数值（420 / 1,000），信息可信度大大提升；重置文案修复。",
      cons: "「请求配额」文案依然稍重，可以设计为「积分」或「Credits」。",
      suggestion: "数值排版可以做更精细的微字体设计（如 420/1,000 的数字等宽 monospace 显示）。"
    }
  },
  {
    id: "qoder",
    name: "Qoder (Qcoder)",
    desc: "阿里通义灵码 / Qoder 国际版与国内版编程助手账号",
    icon: "https://upload.wikimedia.org/wikipedia/commons/e/e0/Anthropic_logo.svg",
    vendorLogo: "Qoder",
    cardImg: getBase64("qoder_card.png"),
    drawerImg: getBase64("qoder_drawer.png"),
    pageImg: getBase64("qoder_page.png"),
    mockCard: {
      title: "ada@example.com",
      plan: "Pro",
      planClass: "plan plan-pro",
      gatewayState: "当前主用",
      extraBadge: "国际版",
      rows: [
        { label: "套餐内", reset: "11 天后重置", val: "42%", pct: 42, tone: "ok" },
        { label: "加购", reset: null, val: "2%", pct: 2, tone: "ok" }
      ],
      footLeft: "••••pt-8f…c2a1 · 2026/9/1添加",
      footRight: "2 分钟前更新"
    },
    currentSurface: [
      { area: "Header", item: "身份与渠道", desc: "邮箱 · 当前主用 · 国际版/国内版标识 · Pro 徽标" },
      { area: "Body", item: "套餐内分桶", desc: "进度条 + 精简文案「套餐内」+ 规范化重置倒计时" },
      { area: "Body", item: "加购分桶", desc: "进度条 + 精简文案「加购」" },
      { area: "Footer", item: "PAT 凭证掩码", desc: "个人访问令牌缩略提示 (••••pt-8f...c2a1)" }
    ],
    upstreamData: [
      { field: "email", type: "String", source: "Qoder Auth", desc: "用户邮箱", onCard: "Header 标识", redesign: "保留" },
      { field: "backend", type: "Enum", source: "Config", desc: "global (国际版) / cn (国内版)", onCard: "Header 胶囊", redesign: "网络环境与合规强相关，保留" },
      { field: "displayName / userId", type: "String", source: "Qoder Auth", desc: "昵称与用户 ID", onCard: "抽屉", redesign: "抽屉展示" },
      { field: "plan", type: "String", source: "Qoder API", desc: "套餐：Pro / Enterprise / Free", onCard: "Header 徽标", redesign: "保留" },
      { field: "unlimited", type: "Boolean", source: "Qoder API", desc: "是否为无限制席位", onCard: "无限制时替换进度条", redesign: "无限席位时展示优雅无限符号" },
      { field: "buckets", type: "Array", source: "Qoder API", desc: "分档额度：plan (套餐内), addon (加购), org (组织)", onCard: "Body 多条进度条", redesign: "已成功去掉多余的“额度”后缀" },
      { field: "resetAt", type: "ISO Date", source: "Qoder API", desc: "重置时间点 (注意上游远期占位时间戳)", onCard: "行内倒计时", redesign: "已添加 90 天上限安全清洗" },
      { field: "keyHint / hasToken", type: "String", source: "Local Store", desc: "Personal Access Token 凭证掩码", onCard: "Footer", redesign: "抽屉内支持更新 PAT" }
    ],
    designCritique: {
      pros: "成功剔除了「2912158 天后重置」与「套餐额度」啰嗦词汇；分桶结构清晰干净。",
      cons: "国际版/国内版徽标色彩目前为中性灰，可以考虑与品牌色搭配提升品质感。",
      suggestion: "加购额度若为 0 可考虑隐藏或置灰，避免占用卡片垂直高度。"
    }
  },
  {
    id: "zcode",
    name: "Zcode (智谱 ZAI)",
    desc: "智谱 GLM 编码套餐与无配额上游凭证通道",
    icon: "https://upload.wikimedia.org/wikipedia/commons/e/e0/Anthropic_logo.svg",
    vendorLogo: "Zcode",
    cardImg: getBase64("zcode_card.png"),
    drawerImg: getBase64("zcode_drawer.png"),
    pageImg: getBase64("zcode_page.png"),
    mockCard: {
      title: "lin@z.ai",
      plan: "个人版",
      planClass: "plan plan-free",
      gatewayState: "API Key",
      rows: [],
      footLeft: "••••sk-z9ab · 20 天前添加",
      footRight: "5 分钟前检查"
    },
    currentSurface: [
      { area: "Header", item: "身份与标识", desc: "用户邮箱 · Zcode 图标" },
      { area: "Header", item: "套餐与类型", desc: "个人版 · API Key 胶囊" },
      { area: "Body", item: "用量区", desc: "无上游配额，完全留白消除假占位块" },
      { area: "Footer", item: "凭证掩码", desc: "Key 提示 (sk-z9ab) · 添加时间 · 状态检查" }
    ],
    upstreamData: [
      { field: "email", type: "String", source: "Zcode Auth", desc: "智谱账号邮箱", onCard: "Header 标识", redesign: "保留" },
      { field: "provider", type: "Enum", source: "Config", desc: "zai (智谱 ZAI) / bigmodel (开放平台)", onCard: "抽屉", redesign: "保留" },
      { field: "plan", type: "Enum", source: "Config", desc: "coding-plan (永久 key) / start-plan (体验 JWT)", onCard: "Header 徽标", redesign: "保留" },
      { field: "family", type: "String", source: "Client", desc: "套餐族：zai-individual-coding-plan", onCard: "抽屉", redesign: "抽屉展示" },
      { field: "keyHint", type: "String", source: "Store", desc: "API Key 前 8 位或掩码", onCard: "Footer", redesign: "保留" },
      { field: "hasApiKey / hasJwt", type: "Boolean", source: "Store", desc: "凭证类型", onCard: "API Key 徽标", redesign: "保留" },
      { field: "upstreamQuota", type: "None", source: "N/A", desc: "智谱官方接口暂无用户级滚动配额返回", onCard: "无占位（已移除呆板文本）", redesign: "推荐设计轻量紧凑卡片" }
    ],
    designCritique: {
      pros: "果断移除了之前生硬的「智谱 GLM 模型推理」占位框，卡片变得小巧精致，不欺骗用户。",
      cons: "因为完全没有中间用量条，卡片上下两截显得过于空旷脆弱。",
      suggestion: "可引入「轻量卡片模式（Compact Card）」或在中间转而展示「本机网关最近调用统计（如 1.2k 请求 · 320k tokens）」。"
    }
  },
  {
    id: "provider",
    name: "供应商 (Custom API Key)",
    desc: "DeepSeek / 硅基流动 / OpenAI / Moonshot 等自定义中转与官方 Key",
    icon: "https://upload.wikimedia.org/wikipedia/commons/e/e0/Anthropic_logo.svg",
    vendorLogo: "DeepSeek",
    cardImg: getBase64("provider_card.png"),
    drawerImg: getBase64("provider_drawer.png"),
    pageImg: getBase64("provider_page.png"),
    mockCard: {
      title: "DeepSeek",
      plan: "Anthropic",
      planClass: "plan plan-free",
      gatewayState: "当前主用",
      extraBadge: "2 模型",
      rows: [],
      footLeft: "api.deepseek.com · ••••3f9a",
      footRight: "当前主用"
    },
    currentSurface: [
      { area: "Header", item: "供应商名称", desc: "DeepSeek / 自定义名称" },
      { area: "Header", item: "协议与模型数", desc: "兼容协议 (Anthropic/OpenAI) · 模型数徽标 (2 模型)" },
      { area: "Header", item: "状态", desc: "当前主用 · Key 图标" },
      { area: "Body", item: "用量区", desc: "已移除「2 个声明模型」文案块，彻底清爽" },
      { area: "Footer", item: "端点与 Key", desc: "Base URL (api.deepseek.com) · 掩码 Key (••••3f9a)" }
    ],
    upstreamData: [
      { field: "name", type: "String", source: "User Input", desc: "供应商自定义名称", onCard: "Header 标识", redesign: "核心标识" },
      { field: "baseUrl", type: "URL", source: "Config", desc: "API 请求基础地址", onCard: "Footer", redesign: "保留" },
      { field: "apiFormat", type: "Enum", source: "Config", desc: "协议格式：openai / anthropic / gemini", onCard: "Header 徽标", redesign: "提示协议适配" },
      { field: "models", type: "Array<String>", source: "Config", desc: "所声明支持的模型列表 (如 deepseek-chat)", onCard: "Header 「2 模型」徽标", redesign: "移至徽标后视觉极为克制" },
      { field: "keyTail", type: "String", source: "Config", desc: "API Key 尾号", onCard: "Footer", redesign: "保留" },
      { field: "ping (durationMs)", type: "Number", source: "Health Check", desc: "连通性测试延迟 (毫秒数)", onCard: "抽屉", redesign: "可作为网络健康绿点常驻" }
    ],
    designCritique: {
      pros: "成功将呆板的「2 个声明模型」框移除，并将模型数量收进 Header 徽标，卡片大幅精简。",
      cons: "同样存在无配额时的垂直留白空间，与 Zcode 类似。",
      suggestion: "可作为「Key 卡片组件规范」，与带有滚动配额的「订阅卡片组件规范」形成明确的两种设计变体。"
    }
  },
  {
    id: "cursor",
    name: "Cursor (基准对照参考)",
    desc: "Cursor IDE 官方账号与切号号池 (经典高密度设计形态)",
    icon: "https://upload.wikimedia.org/wikipedia/commons/e/e0/Anthropic_logo.svg",
    vendorLogo: "Cursor",
    cardImg: getBase64("cursor_card.png"),
    drawerImg: getBase64("cursor_drawer.png"),
    pageImg: getBase64("cursor_page.png"),
    mockCard: {
      title: "arvid.pfeffer@outlook.com",
      plan: "Pro",
      planClass: "plan plan-pro",
      gatewayState: "当前登录",
      extraBadge: "切号",
      rows: [
        { label: "快速请求", reset: null, val: "0 / 500", pct: 100, tone: "bad" },
        { label: "按需额度", reset: null, val: "$0 / $0", pct: 0, tone: "ok" }
      ],
      footLeft: "月额 17 天后重置 · Bot 明天 16:00 重置",
      footRight: "5 分钟前更新"
    },
    currentSurface: [
      { area: "Header", item: "身份与状态", desc: "邮箱 · 当前登录 · 切号池胶囊 · Pro 徽标" },
      { area: "Body", item: "快速请求进度条", desc: "0/500 点数进度条 + 颜色状态" },
      { area: "Body", item: "按需计费进度条", desc: "$0 / $0 金额进度条" },
      { area: "Footer", item: "重置倒计时", desc: "月额重置 · Bot 重置双倒计时" }
    ],
    upstreamData: [
      { field: "email", type: "String", source: "Cursor Auth", desc: "登录邮箱", onCard: "Header", redesign: "基准" },
      { field: "membership", type: "String", source: "Cursor API", desc: "pro / business / free", onCard: "Header 徽标", redesign: "基准" },
      { field: "creditPoints", type: "Number", source: "Cursor API", desc: "快速请求剩余额度 (500)", onCard: "Body 点数", redesign: "基准" },
      { field: "onDemandCents", type: "Number", source: "Stripe", desc: "按需超额消费美分", onCard: "Body 金额", redesign: "基准" },
      { field: "cycleEnd", type: "Epoch", source: "Stripe", desc: "月度账期重置时刻", onCard: "Footer", redesign: "基准" }
    ],
    designCritique: {
      pros: "功能最完善成熟，涵盖了切号、双池、账单、额度的完整生命周期。",
      cons: "作为较早版本，Footer 采用了两行细小文本，层级较多。",
      suggestion: "作为本次其他通道卡片统一风格的对照基准（Baseline）。"
    }
  }
];

const htmlContent = `<!DOCTYPE html>
<html lang="zh-CN" class="dark">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Nexus 账号管理卡片设计原型与上游数据全景图</title>
  <style>
    :root {
      --bg: #090a0f;
      --bg-surface: #12141c;
      --bg-card: #181b26;
      --bg-hover: #222636;
      --border: #272c3d;
      --border-focus: #3b82f6;
      --text: #f1f5f9;
      --text-muted: #94a3b8;
      --text-faint: #64748b;
      --primary: #10b981;
      --primary-dim: rgba(16, 185, 129, 0.12);
      --accent: #3b82f6;
      --accent-dim: rgba(59, 130, 246, 0.12);
      --warn: #f59e0b;
      --warn-dim: rgba(245, 158, 11, 0.12);
      --bad: #ef4444;
      --bad-dim: rgba(239, 68, 68, 0.12);
      --pill-bg: rgba(255, 255, 255, 0.06);
      --radius: 12px;
      --shadow-sm: 0 1px 2px rgba(0,0,0,0.3);
      --shadow-md: 0 4px 12px rgba(0,0,0,0.4);
      --font-mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    }

    html.light {
      --bg: #f8fafc;
      --bg-surface: #ffffff;
      --bg-card: #f1f5f9;
      --bg-hover: #e2e8f0;
      --border: #cbd5e1;
      --border-focus: #2563eb;
      --text: #0f172a;
      --text-muted: #475569;
      --text-faint: #64748b;
      --primary: #059669;
      --primary-dim: rgba(5, 150, 105, 0.12);
      --accent: #2563eb;
      --accent-dim: rgba(37, 99, 235, 0.12);
      --warn: #d97706;
      --warn-dim: rgba(217, 119, 6, 0.12);
      --bad: #dc2626;
      --bad-dim: rgba(220, 38, 38, 0.12);
      --pill-bg: rgba(0, 0, 0, 0.05);
      --shadow-sm: 0 1px 3px rgba(0,0,0,0.08);
      --shadow-md: 0 4px 16px rgba(0,0,0,0.1);
    }

    * { box-sizing: border-box; margin: 0; padding: 0; }
    body {
      background: var(--bg);
      color: var(--text);
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "PingFang SC", "Hiragino Sans GB", sans-serif;
      line-height: 1.5;
      -webkit-font-smoothing: antialiased;
      padding-bottom: 80px;
    }

    header {
      position: sticky;
      top: 0;
      z-index: 50;
      backdrop-filter: blur(16px);
      background: rgba(9, 10, 15, 0.85);
      border-bottom: 1px solid var(--border);
      padding: 14px 28px;
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 16px;
    }
    html.light header {
      background: rgba(248, 250, 252, 0.88);
    }

    .brand {
      display: flex;
      align-items: center;
      gap: 12px;
    }
    .brand-logo {
      width: 32px;
      height: 32px;
      border-radius: 8px;
      background: linear-gradient(135deg, #10b981, #3b82f6);
      display: flex;
      align-items: center;
      justify-content: center;
      color: white;
      font-weight: 800;
      font-size: 16px;
      box-shadow: 0 2px 8px rgba(16, 185, 129, 0.4);
    }
    .brand-title {
      font-size: 16px;
      font-weight: 700;
      letter-spacing: -0.01em;
    }
    .brand-subtitle {
      font-size: 12px;
      color: var(--text-muted);
    }

    .nav-tabs {
      display: flex;
      background: var(--bg-surface);
      border: 1px solid var(--border);
      border-radius: 8px;
      padding: 3px;
      gap: 2px;
    }
    .nav-tab-btn {
      background: transparent;
      border: none;
      color: var(--text-muted);
      padding: 6px 14px;
      font-size: 13px;
      font-weight: 500;
      border-radius: 6px;
      cursor: pointer;
      transition: all 0.15s ease;
    }
    .nav-tab-btn:hover {
      color: var(--text);
      background: var(--bg-hover);
    }
    .nav-tab-btn.active {
      color: var(--text);
      background: var(--bg-card);
      font-weight: 600;
      box-shadow: var(--shadow-sm);
    }

    .theme-toggle-btn {
      background: var(--bg-surface);
      border: 1px solid var(--border);
      color: var(--text);
      padding: 6px 12px;
      font-size: 12px;
      border-radius: 6px;
      cursor: pointer;
      display: flex;
      align-items: center;
      gap: 6px;
      transition: all 0.15s ease;
    }
    .theme-toggle-btn:hover { background: var(--bg-hover); }

    .main-wrap {
      max-width: 1400px;
      margin: 0 auto;
      padding: 24px 28px;
    }

    .hero-banner {
      background: linear-gradient(135deg, rgba(16, 185, 129, 0.08), rgba(59, 130, 246, 0.08));
      border: 1px solid var(--border);
      border-radius: 16px;
      padding: 24px 28px;
      margin-bottom: 28px;
      display: flex;
      justify-content: space-between;
      align-items: flex-start;
      gap: 20px;
    }
    .hero-banner h1 {
      font-size: 20px;
      font-weight: 700;
      margin-bottom: 6px;
    }
    .hero-banner p {
      color: var(--text-muted);
      font-size: 13.5px;
      max-width: 860px;
      line-height: 1.6;
    }
    .badge-pills {
      display: flex;
      gap: 8px;
      flex-wrap: wrap;
      margin-top: 12px;
    }
    .badge-pill {
      font-size: 11.5px;
      padding: 3px 10px;
      border-radius: 999px;
      background: var(--pill-bg);
      border: 1px solid var(--border);
      color: var(--text-muted);
    }
    .badge-pill.highlight {
      background: var(--primary-dim);
      color: var(--primary);
      border-color: rgba(16, 185, 129, 0.3);
      font-weight: 600;
    }

    /* Sub-bar for platform switching */
    .platform-selector {
      display: flex;
      overflow-x: auto;
      gap: 8px;
      padding-bottom: 12px;
      margin-bottom: 24px;
      border-bottom: 1px solid var(--border);
    }
    .platform-chip {
      background: var(--bg-surface);
      border: 1px solid var(--border);
      border-radius: 10px;
      padding: 8px 14px;
      font-size: 13px;
      color: var(--text-muted);
      cursor: pointer;
      display: flex;
      align-items: center;
      gap: 8px;
      white-space: nowrap;
      transition: all 0.15s ease;
    }
    .platform-chip:hover {
      background: var(--bg-hover);
      color: var(--text);
    }
    .platform-chip.active {
      background: var(--accent-dim);
      color: var(--accent);
      border-color: var(--accent);
      font-weight: 600;
    }

    /* Gallery Grid */
    .gallery-grid {
      display: grid;
      grid-template-columns: repeat(auto-fill, minmax(380px, 1fr));
      gap: 24px;
    }
    .gallery-item {
      background: var(--bg-surface);
      border: 1px solid var(--border);
      border-radius: 16px;
      padding: 20px;
      display: flex;
      flex-direction: column;
      gap: 16px;
      transition: all 0.2s ease;
    }
    .gallery-item:hover {
      border-color: rgba(255, 255, 255, 0.2);
      box-shadow: var(--shadow-md);
      transform: translateY(-2px);
    }
    .gallery-header {
      display: flex;
      justify-content: space-between;
      align-items: center;
    }
    .gallery-title {
      font-size: 16px;
      font-weight: 700;
      display: flex;
      align-items: center;
      gap: 8px;
    }
    .gallery-desc {
      font-size: 12px;
      color: var(--text-muted);
    }
    .gallery-actions {
      display: flex;
      gap: 6px;
    }
    .btn-xs {
      font-size: 11px;
      padding: 4px 8px;
      border-radius: 6px;
      background: var(--bg-card);
      border: 1px solid var(--border);
      color: var(--text-muted);
      cursor: pointer;
    }
    .btn-xs:hover { color: var(--text); background: var(--bg-hover); }

    /* Actual Mock Card styling directly adapted from desktop CSS */
    .mock-acct {
      background: var(--bg-card);
      border: 1px solid var(--border);
      border-radius: 10px;
      padding: 12px 14px;
      display: flex;
      flex-direction: column;
      gap: 10px;
      box-shadow: var(--shadow-sm);
      position: relative;
      cursor: pointer;
      transition: border-color 0.15s ease;
    }
    .mock-acct:hover {
      border-color: rgba(255, 255, 255, 0.25);
    }
    .mock-head {
      display: flex;
      align-items: center;
      gap: 8px;
      font-size: 13px;
    }
    .mock-dot {
      width: 7px;
      height: 7px;
      border-radius: 50%;
      background: #94a3b8;
    }
    .mock-identity {
      font-weight: 600;
      color: var(--text);
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
      flex: 1;
    }
    .mock-plan {
      font-size: 11px;
      font-weight: 600;
      padding: 1px 8px;
      border-radius: 999px;
      border: 1px solid var(--primary);
      color: var(--primary);
      background: var(--primary-dim);
    }
    .mock-plan.plan-team {
      border-color: var(--accent);
      color: var(--accent);
      background: var(--accent-dim);
    }
    .mock-plan.plan-free {
      border-color: var(--text-faint);
      color: var(--text-muted);
      background: var(--pill-bg);
    }
    .mock-pill {
      font-size: 11px;
      padding: 1px 7px;
      border-radius: 999px;
      background: var(--pill-bg);
      color: var(--text-muted);
      border: 1px solid var(--border);
    }
    .mock-pill.current {
      background: var(--primary-dim);
      color: var(--primary);
      border-color: rgba(16, 185, 129, 0.3);
      font-weight: 600;
    }

    .mock-qc {
      display: flex;
      flex-direction: column;
      gap: 4px;
      margin-top: 2px;
    }
    .mock-qc-head {
      display: flex;
      justify-content: space-between;
      align-items: baseline;
      font-size: 12px;
    }
    .mock-qc-label {
      color: var(--text);
      font-weight: 500;
      display: flex;
      align-items: center;
      gap: 6px;
    }
    .mock-qc-sub {
      color: var(--text-faint);
      font-size: 10.5px;
    }
    .mock-qc-val {
      font-size: 12px;
      font-weight: 600;
      font-family: var(--font-mono);
      color: var(--text);
    }
    .mock-qc-val.is-bad { color: var(--bad); }
    .mock-qc-val.is-warn { color: var(--warn); }
    .mock-qc-track {
      height: 4px;
      background: rgba(255, 255, 255, 0.08);
      border-radius: 2px;
      overflow: hidden;
      position: relative;
    }
    html.light .mock-qc-track {
      background: rgba(0, 0, 0, 0.06);
    }
    .mock-qc-bar {
      height: 100%;
      background: var(--primary);
      border-radius: 2px;
    }
    .mock-qc-bar.is-bad { background: var(--bad); }
    .mock-qc-bar.is-warn { background: var(--warn); }

    .mock-foot {
      display: flex;
      justify-content: space-between;
      align-items: center;
      font-size: 11px;
      color: var(--text-faint);
      border-top: 1px solid rgba(255, 255, 255, 0.05);
      padding-top: 8px;
      margin-top: 2px;
    }
    .mock-foot-actions {
      display: flex;
      align-items: center;
      gap: 6px;
    }

    /* Image Preview Container */
    .shot-preview-box {
      border-radius: 10px;
      overflow: hidden;
      border: 1px solid var(--border);
      background: var(--bg);
      position: relative;
    }
    .shot-img {
      width: 100%;
      height: auto;
      display: block;
      cursor: zoom-in;
      transition: transform 0.2s ease;
    }
    .shot-img:hover {
      opacity: 0.95;
    }
    .shot-label {
      position: absolute;
      bottom: 8px;
      right: 8px;
      background: rgba(0, 0, 0, 0.7);
      backdrop-filter: blur(4px);
      padding: 2px 8px;
      border-radius: 4px;
      font-size: 10.5px;
      color: #cbd5e1;
      border: 1px solid rgba(255, 255, 255, 0.1);
    }

    /* Detail Section View */
    .detail-view {
      display: none;
      flex-direction: column;
      gap: 28px;
    }
    .detail-view.active {
      display: flex;
    }

    .detail-hero {
      display: flex;
      justify-content: space-between;
      align-items: center;
      background: var(--bg-surface);
      border: 1px solid var(--border);
      border-radius: 14px;
      padding: 20px 24px;
    }
    .detail-hero-left h2 {
      font-size: 22px;
      font-weight: 700;
    }
    .detail-hero-left p {
      color: var(--text-muted);
      font-size: 13px;
      margin-top: 4px;
    }

    .detail-grid-3 {
      display: grid;
      grid-template-columns: 1fr 1fr;
      gap: 24px;
    }
    @media (max-width: 1024px) {
      .detail-grid-3 { grid-template-columns: 1fr; }
    }

    .detail-card {
      background: var(--bg-surface);
      border: 1px solid var(--border);
      border-radius: 14px;
      padding: 20px;
      display: flex;
      flex-direction: column;
      gap: 16px;
    }
    .detail-card-title {
      font-size: 15px;
      font-weight: 700;
      display: flex;
      align-items: center;
      gap: 8px;
      border-bottom: 1px solid var(--border);
      padding-bottom: 12px;
    }

    /* Data Tables */
    .data-table-wrap {
      overflow-x: auto;
      border-radius: 10px;
      border: 1px solid var(--border);
    }
    table.data-table {
      width: 100%;
      border-collapse: collapse;
      font-size: 12.5px;
      text-align: left;
    }
    table.data-table th {
      background: var(--bg-card);
      padding: 10px 14px;
      color: var(--text-muted);
      font-weight: 600;
      border-bottom: 1px solid var(--border);
      white-space: nowrap;
    }
    table.data-table td {
      padding: 10px 14px;
      border-bottom: 1px solid var(--border);
      color: var(--text);
    }
    table.data-table tr:last-child td {
      border-bottom: none;
    }
    table.data-table tr:hover td {
      background: var(--bg-hover);
    }
    .code-tag {
      font-family: var(--font-mono);
      font-size: 11.5px;
      padding: 2px 6px;
      border-radius: 4px;
      background: rgba(255, 255, 255, 0.08);
      color: #38bdf8;
    }
    html.light .code-tag {
      background: rgba(0, 0, 0, 0.05);
      color: #0284c7;
    }

    /* Design Callout */
    .critique-box {
      border-radius: 10px;
      padding: 14px 16px;
      font-size: 13px;
      line-height: 1.6;
    }
    .critique-box.pros {
      background: rgba(16, 185, 129, 0.08);
      border: 1px solid rgba(16, 185, 129, 0.25);
    }
    .critique-box.cons {
      background: rgba(245, 158, 11, 0.08);
      border: 1px solid rgba(245, 158, 11, 0.25);
    }
    .critique-box.action {
      background: rgba(59, 130, 246, 0.08);
      border: 1px solid rgba(59, 130, 246, 0.25);
    }

    /* Modal Lightbox */
    .lightbox {
      display: none;
      position: fixed;
      inset: 0;
      z-index: 100;
      background: rgba(0, 0, 0, 0.88);
      backdrop-filter: blur(8px);
      align-items: center;
      justify-content: center;
      padding: 20px;
    }
    .lightbox.active {
      display: flex;
    }
    .lightbox img {
      max-width: 90vw;
      max-height: 90vh;
      border-radius: 12px;
      box-shadow: 0 20px 40px rgba(0,0,0,0.6);
      border: 1px solid rgba(255, 255, 255, 0.15);
    }
    .lightbox-close {
      position: absolute;
      top: 24px;
      right: 28px;
      color: white;
      font-size: 28px;
      cursor: pointer;
      font-weight: 300;
    }
  </style>
</head>
<body>

  <header>
    <div class="brand">
      <div class="brand-logo">N</div>
      <div>
        <div class="brand-title">Nexus 账号管理卡片设计原型与上游数据全景图</div>
        <div class="brand-subtitle">面向专业 UI/UX 设计评估 · 原型二次打磨 · 信息架构与上游字段映射</div>
      </div>
    </div>

    <div class="nav-tabs">
      <button class="nav-tab-btn active" onclick="switchNav('gallery')">👁️ 全景画廊 (Gallery)</button>
      <button class="nav-tab-btn" onclick="switchNav('detail')">🔍 逐平台深度拆解</button>
      <button class="nav-tab-btn" onclick="switchNav('matrix')">📊 全平台上游数据全景表</button>
      <button class="nav-tab-btn" onclick="switchNav('guidelines')">💡 设计二次打磨规范建议</button>
    </div>

    <button class="theme-toggle-btn" onclick="toggleTheme()">
      <span id="theme-icon">☀️</span>
      <span id="theme-text">切换明暗</span>
    </button>
  </header>

  <div class="main-wrap">

    <div class="hero-banner">
      <div>
        <h1>Nexus 平台账号卡片规范与原型设计基准</h1>
        <p>
          本原型汇总了当前 Nexus 桌面端覆盖的所有 8 个上游接入平台（以及作为基准对照的 Cursor）的真实渲染截图、
          交互式卡片 HTML/CSS 组件、卡片外露信息架构、以及后端 Rust / 上游 API 能够拉取到的全部账号与用量字段清单。
          旨在为专业 UI 设计团队提供客观详尽的信息支撑，便于推进后续的高质感视觉重构与二级页面交互下钻设计。
        </p>
        <div class="badge-pills">
          <span class="badge-pill highlight">✨ 已优化：消除了冗余文案与重复倒计时</span>
          <span class="badge-pill highlight">✨ 已修复：Kiro 积分透出与 Qoder 远期时间戳 Bug</span>
          <span class="badge-pill">🎯 设计目标：极致克制 · 现代质感 · 统一层级 · 卡片到抽屉二级递进</span>
        </div>
      </div>
    </div>

    <!-- VIEW 1: GALLERY VIEW -->
    <section id="view-gallery" class="view-section">
      <div class="gallery-grid">
        ${platforms.map((p) => `
          <div class="gallery-item">
            <div class="gallery-header">
              <div>
                <div class="gallery-title">${p.name}</div>
                <div class="gallery-desc">${p.desc}</div>
              </div>
              <div class="gallery-actions">
                <button class="btn-xs" onclick="openDetail('${p.id}')">查看深度规范 →</button>
              </div>
            </div>

            <!-- Real High-DPI Screenshot -->
            <div>
              <div style="font-size: 11.5px; color: var(--text-faint); margin-bottom: 6px; font-weight: 600;">实际应用渲染截图 (High-DPI)</div>
              <div class="shot-preview-box">
                <img src="${p.cardImg}" alt="${p.name} 卡片截图" class="shot-img" onclick="openLightbox(this.src)" />
                <span class="shot-label">点击放大 2x</span>
              </div>
            </div>

            <!-- Live Interactive HTML/CSS Mockup Component -->
            <div>
              <div style="font-size: 11.5px; color: var(--text-faint); margin-bottom: 6px; font-weight: 600;">可交互卡片代码原型 (Live CSS Mockup)</div>
              <div class="mock-acct">
                <div class="mock-head">
                  <span class="mock-dot"></span>
                  <span class="mock-identity">${p.mockCard.title}</span>
                  ${p.mockCard.extraBadge ? `<span class="mock-pill">${p.mockCard.extraBadge}</span>` : ''}
                  ${p.mockCard.gatewayState ? `<span class="mock-pill current">${p.mockCard.gatewayState}</span>` : ''}
                  <span class="${p.mockCard.planClass}">${p.mockCard.plan}</span>
                </div>

                ${p.mockCard.rows && p.mockCard.rows.length > 0 ? `
                  <div class="mock-qc-group">
                    ${p.mockCard.rows.map(r => `
                      <div class="mock-qc">
                        <div class="mock-qc-head">
                          <span class="mock-qc-label">
                            <span>${r.label}</span>
                            ${r.reset ? `<span class="mock-qc-sub">${r.reset}</span>` : ''}
                          </span>
                          <span class="mock-qc-val is-${r.tone}">${r.val}</span>
                        </div>
                        <div class="mock-qc-track">
                          <div class="mock-qc-bar is-${r.tone}" style="width: ${r.pct}%;"></div>
                        </div>
                      </div>
                    `).join('')}
                  </div>
                ` : `
                  <div style="padding: 12px 0; color: var(--text-faint); font-size: 12px; text-align: center; border-radius: 6px; background: rgba(255,255,255,0.02);">
                    无上游滚动配额 (纯凭证或按需通道，卡片高度紧凑)
                  </div>
                `}

                <div class="mock-foot">
                  <span>${p.mockCard.footLeft}</span>
                  <div class="mock-foot-actions">
                    <span>${p.mockCard.footRight}</span>
                    <span style="font-size: 13px; cursor: pointer; color: var(--text-muted);" title="刷新">↻</span>
                  </div>
                </div>
              </div>
            </div>

            <!-- Core Evaluation Summary -->
            <div style="font-size: 12px; color: var(--text-muted); border-top: 1px solid var(--border); padding-top: 10px;">
              <strong style="color: var(--text);">设计打磨要点：</strong> ${p.designCritique.suggestion}
            </div>
          </div>
        `).join('')}
      </div>
    </section>

    <!-- VIEW 2: DETAIL VIEW (PER PLATFORM) -->
    <section id="view-detail" class="view-section" style="display: none;">
      <div class="platform-selector">
        ${platforms.map((p, idx) => `
          <div class="platform-chip ${idx === 0 ? 'active' : ''}" id="chip-${p.id}" onclick="selectPlatform('${p.id}')">
            <span>${p.name}</span>
          </div>
        `).join('')}
      </div>

      ${platforms.map((p, idx) => `
        <div class="detail-view ${idx === 0 ? 'active' : ''}" id="detail-${p.id}">
          <div class="detail-hero">
            <div class="detail-hero-left">
              <h2>${p.name} 深度设计规范与上游数据全景</h2>
              <p>${p.desc}</p>
            </div>
            <div class="badge-pills">
              <span class="badge-pill highlight">${p.upstreamData.length} 个上游可读字段</span>
              <span class="badge-pill">支持卡片与抽屉二级下钻</span>
            </div>
          </div>

          <div class="detail-grid-3">
            <!-- Left: Card Screenshot & Live Component -->
            <div class="detail-card">
              <div class="detail-card-title">📱 账号卡片视觉外观与实际渲染</div>
              <div>
                <div style="font-size: 12px; color: var(--text-faint); margin-bottom: 8px;">实际截图 (点击 2x 放大看像素与微字体)</div>
                <div class="shot-preview-box">
                  <img src="${p.cardImg}" alt="${p.name} 卡片" class="shot-img" onclick="openLightbox(this.src)" />
                  <span class="shot-label">点击放大 2x</span>
                </div>
              </div>

              <div>
                <div style="font-size: 12px; color: var(--text-faint); margin-bottom: 8px;">可交互代码原型组件 (可测试悬停与间距)</div>
                <div class="mock-acct">
                  <div class="mock-head">
                    <span class="mock-dot"></span>
                    <span class="mock-identity">${p.mockCard.title}</span>
                    ${p.mockCard.extraBadge ? `<span class="mock-pill">${p.mockCard.extraBadge}</span>` : ''}
                    ${p.mockCard.gatewayState ? `<span class="mock-pill current">${p.mockCard.gatewayState}</span>` : ''}
                    <span class="${p.mockCard.planClass}">${p.mockCard.plan}</span>
                  </div>

                  ${p.mockCard.rows && p.mockCard.rows.length > 0 ? `
                    <div class="mock-qc-group">
                      ${p.mockCard.rows.map(r => `
                        <div class="mock-qc">
                          <div class="mock-qc-head">
                            <span class="mock-qc-label">
                              <span>${r.label}</span>
                              ${r.reset ? `<span class="mock-qc-sub">${r.reset}</span>` : ''}
                            </span>
                            <span class="mock-qc-val is-${r.tone}">${r.val}</span>
                          </div>
                          <div class="mock-qc-track">
                            <div class="mock-qc-bar is-${r.tone}" style="width: ${r.pct}%;"></div>
                          </div>
                        </div>
                      `).join('')}
                    </div>
                  ` : `
                    <div style="padding: 14px 0; color: var(--text-faint); font-size: 12px; text-align: center; border-radius: 6px; background: rgba(255,255,255,0.02);">
                      无上游滚动配额 (纯凭证或按需通道，卡片高度紧凑)
                    </div>
                  `}

                  <div class="mock-foot">
                    <span>${p.mockCard.footLeft}</span>
                    <div class="mock-foot-actions">
                      <span>${p.mockCard.footRight}</span>
                      <span style="font-size: 13px; cursor: pointer; color: var(--text-muted);" title="刷新">↻</span>
                    </div>
                  </div>
                </div>
              </div>

              <!-- Surface Structure Table -->
              <div style="margin-top: 8px;">
                <div style="font-size: 12px; color: var(--text-faint); margin-bottom: 8px; font-weight: 600;">当前卡片外露信息结构 (Surface Info)</div>
                <div class="data-table-wrap">
                  <table class="data-table">
                    <thead>
                      <tr><th>区域</th><th>外露元素</th><th>说明与呈现方式</th></tr>
                    </thead>
                    <tbody>
                      ${p.currentSurface.map(s => `
                        <tr>
                          <td><span class="code-tag">${s.area}</span></td>
                          <td><strong>${s.item}</strong></td>
                          <td style="color: var(--text-muted);">${s.desc}</td>
                        </tr>
                      `).join('')}
                    </tbody>
                  </table>
                </div>
              </div>
            </div>

            <!-- Right: Drawer Screenshot & Design Critique -->
            <div class="detail-card">
              <div class="detail-card-title">📖 二级页面：账号抽屉详情 (Drawer)</div>
              <div>
                <div style="font-size: 12px; color: var(--text-faint); margin-bottom: 8px;">抽屉展开截图 (点击卡片后展示完整元数据)</div>
                <div class="shot-preview-box" style="max-height: 480px; overflow-y: auto;">
                  <img src="${p.drawerImg}" alt="${p.name} 抽屉" class="shot-img" onclick="openLightbox(this.src)" />
                  <span class="shot-label">点击放大 2x</span>
                </div>
              </div>

              <!-- Critique & Recommendations -->
              <div style="display: flex; flex-direction: column; gap: 10px; margin-top: 8px;">
                <div class="critique-box pros">
                  <strong style="color: var(--primary);">✅ 现状优势：</strong> ${p.designCritique.pros}
                </div>
                <div class="critique-box cons">
                  <strong style="color: var(--warn);">⚠️ 视觉或体验痛点：</strong> ${p.designCritique.cons}
                </div>
                <div class="critique-box action">
                  <strong style="color: var(--accent);">💡 设计重构建议：</strong> ${p.designCritique.suggestion}
                </div>
              </div>
            </div>
          </div>

          <!-- Bottom: Full Upstream Data Dictionary Table -->
          <div class="detail-card">
            <div class="detail-card-title">📊 ${p.name} 上游与后端可获取的全部字段清单 (Data Capability Spec)</div>
            <div class="data-table-wrap">
              <table class="data-table">
                <thead>
                  <tr>
                    <th>字段名 (Field)</th>
                    <th>类型</th>
                    <th>数据来源 (Source)</th>
                    <th>含义解释</th>
                    <th>当前卡片暴露状态</th>
                    <th>设计打磨与重构建议</th>
                  </tr>
                </thead>
                <tbody>
                  ${p.upstreamData.map(d => `
                    <tr>
                      <td><span class="code-tag">${d.field}</span></td>
                      <td>${d.type}</td>
                      <td style="color: var(--text-faint);">${d.source}</td>
                      <td>${d.desc}</td>
                      <td>
                        <span class="badge-pill ${d.onCard.includes('隐藏') ? '' : 'highlight'}">
                          ${d.onCard}
                        </span>
                      </td>
                      <td style="color: var(--text-muted);">${d.redesign}</td>
                    </tr>
                  `).join('')}
                </tbody>
              </table>
            </div>
          </div>
        </div>
      `).join('')}
    </section>

    <!-- VIEW 3: FULL UPSTREAM MATRIX -->
    <section id="view-matrix" class="view-section" style="display: none;">
      <div class="detail-card">
        <div class="detail-card-title">🌐 9 大平台全数据维度横向对比矩阵 (Cross-Platform Capability Matrix)</div>
        <p style="color: var(--text-muted); font-size: 13px; margin-bottom: 16px;">
          此表清晰归纳了各平台的额度颗粒度差异。有些平台提供精确到分钟的滚动窗口（如 ChatGPT、Antigravity），
          有些平台提供精确数值积分（如 Kiro），而有些平台是永久 Key 或无上游配额通道（如 Zcode、Provider）。
          设计时必须采取灵活的组件变体，而不是一刀切硬塞进度条。
        </p>
        <div class="data-table-wrap">
          <table class="data-table">
            <thead>
              <tr>
                <th>平台名称</th>
                <th>凭证机制</th>
                <th>套餐/等级</th>
                <th>滚动窗口 (Rolling Window)</th>
                <th>数值积分 (Credits)</th>
                <th>重置时间戳精度</th>
                <th>多模态/模型分桶</th>
                <th>推荐卡片形态</th>
              </tr>
            </thead>
            <tbody>
              <tr>
                <td><strong>ChatGPT</strong></td>
                <td>OAuth / Refresh Token</td>
                <td>Pro / Plus / Team / Free</td>
                <td>5h 滚动 + 7d 周期</td>
                <td>Codex Credits (部分号)</td>
                <td>精确到分钟</td>
                <td>Spark / CodexBengalfox</td>
                <td>标准多桶卡片 (支持折叠)</td>
              </tr>
              <tr>
                <td><strong>Claude</strong></td>
                <td>OAuth / Session</td>
                <td>Max / Pro / Free</td>
                <td>Weekly / Monthly 周期</td>
                <td>无具体点数 (百分比)</td>
                <td>精确到分钟</td>
                <td>单一主配额</td>
                <td>紧凑单条卡片</td>
              </tr>
              <tr>
                <td><strong>Antigravity</strong></td>
                <td>Google OAuth (GCP)</td>
                <td>Google AI Pro / Ultra</td>
                <td>Gemini 5h + Weekly, Others 5h + Weekly</td>
                <td>无具体点数 (百分比)</td>
                <td>精确到分钟 + 周边界</td>
                <td>Gemini 与 Others 分类</td>
                <td>双组对称卡片</td>
              </tr>
              <tr>
                <td><strong>Grok Build</strong></td>
                <td>OAuth / Console Key</td>
                <td>SuperGrok / Premium</td>
                <td>Weekly 周期</td>
                <td>无具体点数 (百分比)</td>
                <td>精确到天</td>
                <td>独立媒体资格 (生图/视频)</td>
                <td>单条卡片 + 媒体徽标</td>
              </tr>
              <tr>
                <td><strong>Kiro</strong></td>
                <td>Social / Builder ID</td>
                <td>Kiro Pro / Team / Free</td>
                <td>月度账期</td>
                <td>精确数值 (420 / 1000) + 试用点数</td>
                <td>精确到天</td>
                <td>基础额度 + 试用 + 超额</td>
                <td>数值积分卡片</td>
              </tr>
              <tr>
                <td><strong>Qoder</strong></td>
                <td>PAT 访问令牌</td>
                <td>Pro / Enterprise / Free</td>
                <td>套餐账期</td>
                <td>百分比 + 团队席位无限</td>
                <td>精确到天 (已做 90d 过滤)</td>
                <td>套餐内 + 加购 + 组织</td>
                <td>双桶清爽卡片</td>
              </tr>
              <tr>
                <td><strong>Zcode</strong></td>
                <td>永久 Key / JWT</td>
                <td>个人编码套餐 / 体验版</td>
                <td>无上游配额</td>
                <td>无</td>
                <td>无</td>
                <td>无</td>
                <td>轻量无用量卡片</td>
              </tr>
              <tr>
                <td><strong>供应商 (Provider)</strong></td>
                <td>自定义 API Key</td>
                <td>Anthropic / OpenAI 兼容</td>
                <td>无上游配额 (按本地网关统计)</td>
                <td>无 (依赖本地 Traffic)</td>
                <td>无 (依赖 Ping 延迟)</td>
                <td>声明模型列表 (2+ 模型)</td>
                <td>协议与模型徽标卡片</td>
              </tr>
              <tr style="background: rgba(255, 255, 255, 0.02);">
                <td><strong>Cursor (基准)</strong></td>
                <td>Session / Refresh</td>
                <td>Pro / Business / Free</td>
                <td>月账期 + Bot 周重置</td>
                <td>快速请求 (0/500) + 按需金额</td>
                <td>精确到天与钟点</td>
                <td>Fast + Slow + Bot</td>
                <td>经典双条卡片</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </section>

    <!-- VIEW 4: DESIGN GUIDELINES -->
    <section id="view-guidelines" class="view-section" style="display: none;">
      <div style="display: grid; grid-template-columns: 1fr 1fr; gap: 24px;">
        <div class="detail-card">
          <div class="detail-card-title">🎨 设计原则 1：视觉克制与信息降噪 (Visual Restraint)</div>
          <div style="font-size: 13.5px; color: var(--text-muted); line-height: 1.7; display: flex; flex-direction: column; gap: 12px;">
            <p><strong>1. 拒绝多余的解释性中文标签：</strong><br/>
              用户理解「套餐内」「加购」足矣，无需追加「额度」字样；对于 Antigravity，用户理解「Gemini 5h」与「Weekly」足矣，无需追加「会话额度」「每周额度」。
            </p>
            <p><strong>2. 消除重复倒计时：</strong><br/>
              倒计时应当直接内嵌在控制该窗口的标题旁（如 <code>Codex 5 小时 · 17:30 重置</code>），而不是在卡片底部重复再列一整行类似 <code>5 小时 17:30 重置 · 7 天 6 天后重置</code> 的文字。
            </p>
            <p><strong>3. 拒绝无数据的虚假占位：</strong><br/>
              当上游没有配额指标时（例如 Zcode 或自定义供应商），绝不强行摆放「智谱 GLM 模型推理」或「2 个声明模型」等居中文本框欺骗用户视线。无配额时直接紧凑收起，卡片自然小巧。
            </p>
          </div>
        </div>

        <div class="detail-card">
          <div class="detail-card-title">📐 设计原则 2：卡片到抽屉的二级递进 (Progressive Disclosure)</div>
          <div style="font-size: 13.5px; color: var(--text-muted); line-height: 1.7; display: flex; flex-direction: column; gap: 12px;">
            <p><strong>1. 卡片负责「快速扫视」：</strong><br/>
              一列卡片展示的核心任务是让用户在 1 秒内回答：「哪个号还能用？」「哪个号快用光了？」「哪个号需要重新授权？」。只外露核心百分比、倒计时、套餐级别和网络健康。
            </p>
            <p><strong>2. 抽屉负责「全面操作与深入排障」：</strong><br/>
              用户点击卡片后滑出的侧栏抽屉，负责展开所有详细技术指标：用户 UUID、组织 ID、GCP 项目、访问令牌过期时间、本地 90 天请求历史与 Token 流量、一键重新授权与复制凭证。
            </p>
            <p><strong>3. 统一且自适应的三种卡片高度变体：</strong><br/>
              • <strong>标准计量卡 (Standard Metered)</strong>：如 ChatGPT、Kiro、Antigravity，带 1-2 条进度条。<br/>
              • <strong>极简凭证卡 (Compact Credential)</strong>：如 Zcode、供应商，高度仅 ~68px，极其精致干净。<br/>
              • <strong>折叠/展开卡 (Collapsible)</strong>：多于 2 个模型分桶时，卡片默认收起次要分桶，支持悬停展开或进入抽屉。
            </p>
          </div>
        </div>
      </div>
    </section>

  </div>

  <!-- Lightbox Modal -->
  <div class="lightbox" id="lightbox" onclick="closeLightbox()">
    <span class="lightbox-close">&times;</span>
    <img id="lightbox-img" src="" alt="放大查看" onclick="event.stopPropagation()" />
  </div>

  <script>
    function toggleTheme() {
      const html = document.documentElement;
      const isDark = html.classList.contains('dark');
      if (isDark) {
        html.classList.remove('dark');
        html.classList.add('light');
        document.getElementById('theme-icon').textContent = '🌙';
      } else {
        html.classList.remove('light');
        html.classList.add('dark');
        document.getElementById('theme-icon').textContent = '☀️';
      }
    }

    function switchNav(navId) {
      document.querySelectorAll('.nav-tab-btn').forEach(btn => btn.classList.remove('active'));
      event.target.classList.add('active');

      document.querySelectorAll('.view-section').forEach(sec => sec.style.display = 'none');
      document.getElementById('view-' + navId).style.display = navId === 'detail' || navId === 'gallery' ? 'block' : 'block';
      if (navId === 'detail') {
        document.querySelectorAll('.detail-view').forEach(v => v.classList.remove('active'));
        const activeChip = document.querySelector('.platform-chip.active');
        const id = activeChip ? activeChip.id.replace('chip-', '') : 'chatgpt';
        document.getElementById('detail-' + id).classList.add('active');
      }
    }

    function openDetail(platId) {
      document.querySelectorAll('.nav-tab-btn').forEach(b => {
        if (b.textContent.includes('深度拆解')) b.classList.add('active');
        else b.classList.remove('active');
      });
      document.querySelectorAll('.view-section').forEach(sec => sec.style.display = 'none');
      document.getElementById('view-detail').style.display = 'block';
      selectPlatform(platId);
      window.scrollTo({ top: 0, behavior: 'smooth' });
    }

    function selectPlatform(platId) {
      document.querySelectorAll('.platform-chip').forEach(c => c.classList.remove('active'));
      const chip = document.getElementById('chip-' + platId);
      if (chip) chip.classList.add('active');

      document.querySelectorAll('.detail-view').forEach(v => v.classList.remove('active'));
      const detail = document.getElementById('detail-' + platId);
      if (detail) detail.classList.add('active');
    }

    function openLightbox(src) {
      const lb = document.getElementById('lightbox');
      const img = document.getElementById('lightbox-img');
      img.src = src;
      lb.classList.add('active');
    }

    function closeLightbox() {
      document.getElementById('lightbox').classList.remove('active');
    }

    document.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') closeLightbox();
    });
  </script>
</body>
</html>
`;

fs.writeFileSync(outHtmlPathWorkspace, htmlContent, 'utf8');
fs.writeFileSync(outHtmlPathArtifact, htmlContent, 'utf8');
console.log('Saved prototype HTML to:');
console.log('1.', outHtmlPathWorkspace, `(${(htmlContent.length / 1024 / 1024).toFixed(2)} MB)`);
console.log('2.', outHtmlPathArtifact);
