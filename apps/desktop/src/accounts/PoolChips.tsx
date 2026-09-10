/**
 * 账号卡上的「所在池」小标：进了切号池、进了网关号池，各一枚。
 *
 * 只标**进了**的：一列几十张卡，每张都挂两枚「未加入」等于没标。网关那枚按处境换色 ——
 * 正在接力是绿的，接力时跳过是琥珀的（它进了名单却用不了，得有人看见）。
 */
import { Icon } from "../ui/primitives";
import { inGatewayRoster, type PoolMembership } from "./pools";

export function PoolChips({ membership }: { membership: PoolMembership }) {
  const gw = membership.gateway;
  const showGateway = inGatewayRoster(gw);
  if (!membership.switcher && !showGateway) return null;
  return (
    <>
      {membership.switcher ? (
        <span className="use-chip" title={membership.switcher.isCurrent ? "在切号池里，Cursor 当前登录的就是它" : "在切号池里"}>
          <Icon name="switcher" size={10} />
          切号池
        </span>
      ) : null}
      {showGateway ? (
        <span className={`use-chip${gw === "current" ? " is-live" : gw === "skipped" ? " is-warn" : ""}`} title={gw === "current" ? "网关正在用它接力" : gw === "skipped" ? "在网关号池里，但此刻拿不到凭证，接力时跳过" : "在网关号池里，等着接力"}>
          <Icon name="gateway" size={10} />
          网关
        </span>
      ) : null}
    </>
  );
}
