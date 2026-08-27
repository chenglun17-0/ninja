export const meta = {
  name: "ninja_implement_phase",
  description: "Ninja 单阶段实施：盘点 → 实施 → 独立验证，失败即停",
  phases: [{ title: "盘点" }, { title: "实施" }, { title: "验证" }],
};

const REPO = "/Users/jal/my_repos/ninja";
const PHASE_IDS = ["p0", "p1", "p2", "p3", "p4", "p5", "p6", "p7"];
const PHASES = {
  p0: {
    title: "核与仓库",
    accept: "Cargo workspace 链上 libghostty-vt 公开 C API；不碰内部 ghostty.h；空载路径无插件运行时。",
  },
  p1: {
    title: "单终端面",
    accept: "一个 AppKit 窗口里 PTY + vt + Metal 能跑 bash；IME、resize reflow、选区复制粘贴可用。",
  },
  p2: {
    title: "标签分屏",
    accept: "空载宿主有多窗口、标签、分屏、菜单栏与默认 TOML；内存仍与 Ghostty 同量级。这是空载门禁。",
  },
  p3: {
    title: "ADE 协议",
    accept: "ninja-protocol 五类消息可测；空载不建 socket、不拉插件进程。",
  },
  p4: {
    title: "命中分发",
    accept: "点击路径/URL/OSC-8 发 hit；无插件走系统默认，不弹安装提示。",
  },
  p5: {
    title: "层与文本预览",
    accept: "只通过公开协议完成点击路径后终端内看文本/代码；Esc 关层。这是第一个插件门禁。",
  },
  p6: {
    title: "关掉即轻",
    accept: "禁用预览插件后内存回到 p2 空载，无残留进程。这是关掉即轻门禁。",
  },
  p7: {
    title: "签名分发",
    accept: "可安装的签名 macOS .app，默认零插件。仓库仍不公开。",
  },
};

const phaseId = args && typeof args.phase === "string" ? args.phase : "";
if (PHASE_IDS.indexOf(phaseId) < 0) {
  throw new Error("args.phase must be one of p0..p7");
}
const spec = PHASES[phaseId];
const docs = `${REPO}/PRODUCT.md ${REPO}/STACK.md ${REPO}/PLAN.md`;
const inventorySchema = {
  type: "object",
  properties: {
    ready: { type: "boolean" },
    blockedBy: { type: "string" },
    gap: { type: "string" },
    plan: { type: "string" },
  },
  required: ["ready", "blockedBy", "gap", "plan"],
};
const implementSchema = {
  type: "object",
  properties: {
    done: { type: "boolean" },
    summary: { type: "string" },
    files: { type: "string" },
    residualRisk: { type: "string" },
  },
  required: ["done", "summary", "files", "residualRisk"],
};
const verifySchema = {
  type: "object",
  properties: {
    pass: { type: "boolean" },
    defects: { type: "string" },
    evidence: { type: "string" },
  },
  required: ["pass", "defects", "evidence"],
};

const rules = [
  "只做本阶段。不要做 Agent、图片/PDF 预览、插件市场、Linux、内部 include/ghostty.h。",
  "ADE 协议是进程外 JSON；插件不得链宿主内部 API。",
  "空载不得加载插件运行时。",
  "验证必须跑命令或读代码取证，不得相信实施者的 summary。",
].join(" ");

phase("盘点");
const inventory = await agent(
  `仓库 ${REPO}。只读 ${docs} 和代码。阶段 ${phaseId} ${spec.title}。验收：${spec.accept} ${rules}
若上一阶段未完成，ready=false，blockedBy 写缺失阶段。不要改文件。`,
  { label: `inventory:${phaseId}`, schema: inventorySchema },
);

if (inventory === null) {
  return { phase: phaseId, ok: false, stage: "inventory", reason: "missing", inventory: null, implement: null, verify: [] };
}
if (!inventory.ready) {
  return {
    phase: phaseId,
    ok: false,
    stage: "inventory",
    reason: inventory.blockedBy || inventory.gap,
    inventory,
    implement: null,
    verify: [],
  };
}

phase("实施");
const implementPrompt = (feedback) =>
  `仓库 ${REPO}。阶段 ${phaseId} ${spec.title}。验收：${spec.accept}
盘点 gap：${inventory.gap}
盘点 plan：${inventory.plan}
${rules}
${feedback ? `验证反馈，只修这些：${feedback}` : "按盘点实施，不要顺手做下一阶段。"}
完成后列出改动文件与残留风险。`;

let implement = await agent(implementPrompt(""), {
  label: `implement:${phaseId}:1`,
  thread: `ninja-impl:${phaseId}`,
  schema: implementSchema,
});
if (implement === null) {
  return { phase: phaseId, ok: false, stage: "implement", reason: "missing", inventory, implement: null, verify: [] };
}

async function runVerify(round) {
  const result = await agent(
    `仓库 ${REPO}。你是独立验证员，不是实施者。阶段 ${phaseId} ${spec.title}。验收：${spec.accept}
实施者声称：${implement.summary}
改动文件：${implement.files}
${rules}
对照 ${docs}。用编译、测试或运行证据判断 pass。有缺陷就 fail，不要给情面。`,
    { label: `verify:${phaseId}:${round}`, schema: verifySchema },
  );
  return { round, result };
}

phase("验证");
const verify = [];
const first = await runVerify(1);
verify.push(first);
if (first.result === null) {
  return { phase: phaseId, ok: false, stage: "verify", reason: "missing", inventory, implement, verify };
}
if (first.result.pass === true) {
  return { phase: phaseId, ok: true, stage: "done", inventory, implement, verify };
}

implement = await agent(implementPrompt(first.result.defects), {
  label: `implement:${phaseId}:2`,
  thread: `ninja-impl:${phaseId}`,
  schema: implementSchema,
});
if (implement === null) {
  return { phase: phaseId, ok: false, stage: "revise", reason: "missing", inventory, implement: null, verify };
}

const second = await runVerify(2);
verify.push(second);
if (second.result === null) {
  return { phase: phaseId, ok: false, stage: "verify", reason: "missing", inventory, implement, verify };
}

return {
  phase: phaseId,
  ok: second.result.pass === true,
  stage: second.result.pass === true ? "done" : "failed",
  reason: second.result.pass === true ? "" : second.result.defects,
  inventory,
  implement,
  verify,
};
