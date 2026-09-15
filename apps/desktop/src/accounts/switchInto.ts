/**
 * 把一个账号写进 Cursor 登录态。
 *
 * Cursor 在跑且不换机器码时走热切：deep link 交给它自己吃进 token，不退出、
 * 不打断正在进行的任务。只有设置里开了「切换时同时切机器码」才会退出重启，
 * 那一步才值得问一句。
 */
import { accounts, app, switcher } from "../ipc/api";
import type { Account } from "../ipc/types";

export async function confirmColdSwitchIfNeeded(email: string): Promise<boolean> {
  const [overview, status] = await Promise.all([switcher.overview(), app.status()]);
  if (
    overview.cursorRunning &&
    status.switchMachineIds &&
    !window.confirm(`切到 ${email} 会退出并重启 Cursor，未保存的改动请先保存。继续？`)
  ) {
    return false;
  }
  return true;
}

/** 加入切号池（已在池里则刷新登录态）再切过去。用户取消冷切时直接返回。 */
export async function switchAccountIntoCursor(account: Account): Promise<void> {
  if (!(await confirmColdSwitchIfNeeded(account.email))) return;
  const profile = await accounts.addToSwitchBook(account.id);
  await switcher.switchTo(profile.id);
}
