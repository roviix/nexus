/**
 * 多选 → 自动配置：勾要做的几步，跑起来后一行一个号地填进度。
 *
 * 一个弹窗管「选」和「看」两件事：这批活是分钟级的（一个号四步、十来个请求），关掉弹窗
 * 再回来看不到进度的话，用户只能靠卡片一个个核对有没有配上。所以跑起来之后不换界面，
 * 把勾选那一段收起来，原地长出结果列表。
 *
 * 每一步的结论都要摆出来，包括**跳过**：「已经有 key 了」和「铸失败了」是两回事，只报一句
 * 「配置完成」的话，那些正当的失败（Apple 内购号开不了按需、团队号只有管理员能改）就被吞掉了。
 */

import { useMemo, useState } from "react";
import {
  PROVISION_STEPS,
  PROVISION_STEP_LABEL,
  batchSummary,
  estimateTargets,
  planIsEmpty,
  reportFailed,
  togglePlanStep,
} from "../../accounts/provision";
import type { Account, ProvisionPlan, ProvisionReport, ProvisionStepReport } from "../../ipc/types";
import { maskEmail } from "../../ui/format";
import { Icon, Modal, Spinner } from "../../ui/primitives";

export function ProvisionModal({
  accounts,
  initial,
  running,
  reports,
  masked,
  onClose,
  onStart,
}: {
  /** 要配的号，按列表顺序。 */
  accounts: Account[];
  initial: ProvisionPlan;
  running: boolean;
  /** 已经跑完的那几个号，按完成顺序（后端逐个推事件）。 */
  reports: ProvisionReport[];
  masked: boolean;
  onClose: () => void;
  onStart: (plan: ProvisionPlan) => void;
}) {
  const [plan, setPlan] = useState<ProvisionPlan>(initial);
  const targets = useMemo(() => estimateTargets(accounts), [accounts]);
  const started = running || reports.length > 0;
  const done = !running && reports.length > 0;

  return (
    <Modal
      compact
      title={`自动配置 ${accounts.length} 个账号`}
      subtitle={
        started
          ? "按顺序来：铸 Key 打头当保命绳。关掉弹窗不会停下，但看不到剩下的进度。"
          : "按顺序挨着做，一个号出事不影响其余。已经配好的那几步会自己跳过。"
      }
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn" onClick={onClose} disabled={running}>
            {done ? "关闭" : "取消"}
          </button>
          {done ? null : (
            <button
              type="button"
              className="btn btn-primary"
              disabled={running || accounts.length === 0 || planIsEmpty(plan)}
              onClick={() => onStart(plan)}
            >
              {running ? <Spinner /> : <Icon name="check" size={13} />}
              {running ? "配置中…" : "开始"}
            </button>
          )}
        </>
      }
    >
      {started ? null : (
        <div className="copy-sect">
          <div className="copy-sect-k">要做的步骤</div>
          <div className="choice-list" role="group" aria-label="要做的步骤">
            {PROVISION_STEPS.map((step) => {
              const on = plan[step.id];
              const n = targets[step.id];
              return (
                <button
                  key={step.id}
                  type="button"
                  role="checkbox"
                  aria-checked={on}
                  className={`choice${on ? " is-on" : ""}`}
                  onClick={() => setPlan(togglePlanStep(plan, step.id))}
                >
                  <span className="choice-dot" aria-hidden />
                  <span className="choice-copy">
                    <span className="choice-title">
                      {step.label}
                      {/* 摊到几个号上。按需那一步取决于上游此刻的状态，估不出来就不硬写一个数。 */}
                      <span className="faint">
                        {n == null ? " · 看情况" : n === 0 ? " · 这批都不用" : ` · ${n} 个号`}
                      </span>
                    </span>
                    <span className="choice-sample">{step.hint}</span>
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      )}

      {started ? (
        <div className="copy-sect">
          <div className="copy-sect-k">
            进度
            <span className="faint">
              {` · ${reports.length} / ${accounts.length}`}
            </span>
          </div>
          <div className="stack" style={{ gap: 2 }}>
            {reports.map((r) => (
              <div key={r.id} className="prov-row">
                <span className="prov-email mono">{masked ? maskEmail(r.email) : r.email}</span>
                <span className="prov-steps">
                  {r.steps.map((s) => (
                    <StepPill key={s.step} report={s} />
                  ))}
                </span>
                {reportFailed(r) ? <i className="status-dot is-logged_out" aria-label="有步骤没成功" /> : null}
              </div>
            ))}
            {running ? (
              <div className="prov-row is-waiting">
                <Spinner />
                <span className="faint">还剩 {Math.max(0, accounts.length - reports.length)} 个</span>
              </div>
            ) : null}
          </div>
          {done ? <div className="faint tiny" style={{ marginTop: 8 }}>{batchSummary(reports)}</div> : null}
        </div>
      ) : null}
    </Modal>
  );
}

/**
 * 一步的结果。三态各自一个样子：做成了（实心）、跳过了（淡，不是错）、失败了（红）。
 * 跳过的原因和上游的话都在 `title` 里——一行摆五步，正文放不下，但要查得到。
 */
function StepPill({ report }: { report: ProvisionStepReport }) {
  const { step, state, message } = report;
  const label = PROVISION_STEP_LABEL[step] ?? step;
  return (
    <span className={`prov-pill is-${state}`} title={message ? `${label}：${message}` : label}>
      {state === "done" ? <Icon name="check" size={10} /> : null}
      {state === "failed" ? <Icon name="close" size={10} /> : null}
      {label}
    </span>
  );
}
