export const meta = {
  name: "ninja_implement_phase",
  description: "Ninja 单阶段实施：盘点 → 实施 → 独立验证，失败即停",
  phases: [{ title: "盘点" }, { title: "实施" }, { title: "验证" }],
};

const REPO = "/Users/jal/my_repos/ninja";
const PHASE_IDS = ["q0", "q1", "q2", "q3", "q4"];
const PHASES = {
  q0: {
    title: "引擎底座与能力审计",
    accept:
      "vendored 钉版 libghostty（含 include/ghostty.h 嵌入 API）+ Rust FFI 链入宿主 + 最小嵌入（AppKit 窗口挂 surface 跑 bash、能输入能渲染）+ 能力审计报告（网格/hyperlink、屏幕快照、surface 之上合成层、配置加载与运行时改、键位拦截——逐项 有API/无/绕法）。hit 数据源无且无绕法 → ok:false 停。",
  },
  q1: {
    title: "壳重接",
    accept:
      "surface 的 window/tab/split 上下文回调接现有多窗/标签/分屏布局树；焦点/关闭/resize 全链；面板入口不变。标签分屏日常用法在嵌入引擎上成立，⌘W/⌘⇧Enter 语义保持。",
  },
  q2: {
    title: "配置系统",
    accept:
      "加载 Ghostty 配置（含主题、字体、键位）+ 热重载；ninja.toml 收缩为宿主/插件特有；ODP 缺省主题。用户既有 ghostty 配置常用子集直接生效，主题/字体/键位实测。",
  },
  q3: {
    title: "插件系统 + 三门禁",
    accept:
      "hit / layer / input / theme.set 接到嵌入宿主；三插件只走公开协议不动。三大门禁全部在嵌入宿主上通过；空载内存对照 Ghostty 本尊。Ghostty 语义坑停在宿主适配器，不改协议。",
  },
  q4: {
    title: "单一宿主与分发",
    accept:
      "打包脚本打嵌入宿主（bundle 内可执行文件仍叫 ninja）；DMG 重出；PRODUCT/PLAN/DISTRIBUTION 与实际一致。安装即日常可用，仓库里只剩一条宿主路径。",
  },
};

const phaseId = args && typeof args.phase === "string" ? args.phase : "";
if (PHASE_IDS.indexOf(phaseId) < 0) {
  throw new Error("args.phase must be one of q0..q4");
}
const spec = PHASES[phaseId];
const docs = `${REPO}/PRODUCT.md ${REPO}/PLAN.md`;
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
  "只做本阶段。不要做 Agent、图片/PDF 预览、插件市场、Linux、cmux 式宿主功能（内置浏览器/工作区）、Ghostty 重度 fork。",
  "像 cmux 一样站在 Ghostty 上，只做插件系统。终端核是 libghostty 嵌入；产品面是 ADE 协议。",
  "嵌入 API pre-1.0：钉 commit（vendor/ghostty），一切破坏性升级显式做；工具链版本随钉点（zig 0.15.2）。",
  "ADE 协议是进程外 JSON；插件（ninja-preview/ninja-theme）不得链宿主内部 API，也不得链 ghostty.h。协议契约与 golden 不因嵌入 API 改动。Ghostty 语义坑停在宿主适配器。",
  "空载不得加载插件运行时。新阶段功能必须走 libghostty 嵌入路径（crates/ninja-embed）。",
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
