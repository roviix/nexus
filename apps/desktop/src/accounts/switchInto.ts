/**
 * 把一个账号写进 Cursor 登录态。
 *
 * Cursor 在跑且不换机器码时走热切：deep link 交给它自己吃进 token，不退出、
 * 不打断正在进行的任务。只有设置里开了「切换时同时切机器码」才会退出重启，
 * 那一步才值得问一句。
 *
 * **切号会把这个号带进切号池**（切号器操作的是「档」：登录态快照 + 一套固定机器码，
 * 没有档就没法切）。这件事不藏着：按钮文案在号不在池里时写成「加入切号池并切号」
 * （见 `AccountDrawer`），进了池以后随时可在抽屉的「所在池」一行或切号页移出；
 * 删除账号时也会连带把它从切号池移出（`accounts_remove`）。
 */
import { accounts, app, switcher } from "../ipc/api";
import type { Account } from "../ipc/types";
import { confirm } from "../ui/confirm";

export async function confirmColdSwitchIfNeeded(email: string): Promise<boolean> {
  const [overview, status] = await Promise.all([switcher.overview(), app.status()]);
  if (overview.cursorRunning && status.switchMachineIds) {
    return confirm(`切到 ${email} 会退出并重启 Cursor，未保存的改动请先保存。`, {
      title: "将重启 Cursor",
      okLabel: "继续切换",
    });
  }
  return true;
}

/** 加入切号池（已在池里则刷新登录态）再切过去。用户取消冷切时直接返回。 */
export async function switchAccountIntoCursor(account: Account): Promise<void> {
  if (!(await confirmColdSwitchIfNeeded(account.email))) return;
  const profile = await accounts.addToSwitchBook(account.id);
  await switcher.switchTo(profile.id);
}
