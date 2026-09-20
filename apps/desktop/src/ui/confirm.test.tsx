/**
 * 确认框守的是「没人点确认就不做」这条线。钉住三件事：
 * 没挂 host 时回 false（和取消同义）；点确认 / 取消各回各的；两个问题排队、一次只显示一个。
 */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ConfirmHost, confirm } from "./confirm";

// React 18+ 的 act 环境开关：不开会有告警噪音。
(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

function buttons(): HTMLButtonElement[] {
  return Array.from(document.querySelectorAll<HTMLButtonElement>(".modal-foot button"));
}

describe("confirm", () => {
  it("没挂 host 时直接当作取消", async () => {
    await expect(confirm("x")).resolves.toBe(false);
  });

  it("点确认回 true，点取消回 false", async () => {
    await act(async () => root.render(<ConfirmHost />));

    let p = confirm("移出？", { okLabel: "移出" });
    await act(async () => {});
    expect(document.querySelector(".confirm-message")?.textContent).toBe("移出？");
    await act(async () => buttons().find((b) => b.textContent === "移出")!.click());
    await expect(p).resolves.toBe(true);

    p = confirm("删除？");
    await act(async () => {});
    await act(async () => buttons().find((b) => b.textContent === "取消")!.click());
    await expect(p).resolves.toBe(false);
    expect(document.querySelector(".modal")).toBeNull();
  });

  it("两个问题排队，一次只显示一个", async () => {
    await act(async () => root.render(<ConfirmHost />));
    const first = confirm("第一个");
    const second = confirm("第二个");
    await act(async () => {});
    expect(document.querySelectorAll(".modal").length).toBe(1);
    expect(document.querySelector(".confirm-message")?.textContent).toBe("第一个");

    await act(async () => buttons().find((b) => b.textContent === "确认")!.click());
    await expect(first).resolves.toBe(true);
    expect(document.querySelector(".confirm-message")?.textContent).toBe("第二个");

    await act(async () => buttons().find((b) => b.textContent === "取消")!.click());
    await expect(second).resolves.toBe(false);
  });
});
