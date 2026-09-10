/**
 * 外观：跟随系统 / 浅色 / 深色。
 *
 * 上一版是三张 84px 的窗口小样，理由是「主题是看的，不是读的」。理由没错，但它把设置页
 * 那条 54px 的行撑到了近 80px —— 这一页的版式只有一种（左图标 / 中名字 / 右控件），
 * 一行独高就是破绽。现在收成站内通用的分段控件（`.tabs`），每格前面留一枚 12px 的色片：
 * 「看」这件事交给色片，行高回到和别的设置行一样。点下去立刻生效，没有「保存」。
 */
import { THEME_OPTIONS, useTheme } from "../../ui/theme";

export function Appearance() {
  const { pref, setPref } = useTheme();
  return (
    <div className="tabs" role="radiogroup" aria-label="外观">
      {THEME_OPTIONS.map((o) => (
        <button
          key={o.id}
          type="button"
          role="radio"
          aria-checked={o.id === pref}
          className="tab"
          title={o.hint}
          onClick={() => setPref(o.id)}
        >
          <i className={`look-chip is-${o.id}`} aria-hidden />
          {o.label}
        </button>
      ))}
    </div>
  );
}
