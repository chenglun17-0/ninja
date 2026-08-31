export const meta = {
  name: "ninja_implement_phase",
  description: "Ninja 单阶段实施：盘点 → 实施 → 独立验证，失败即停（PLAN.md 三角色合同）",
  phases: [{ title: "盘点" }, { title: "实施" }, { title: "验证" }],
};

const REPO = "/Users/jal/my_repos/ninja";
const PHASE_IDS = ["q0", "q1", "q2", "q3", "q4"];
const PHASES = {
  q0: {
    title: "嵌入底座",
    accept:
      "钉版构建 vendored libghostty（含 include/ghostty.h 嵌入 API）；Rust FFI（bindgen）链入宿主；最小嵌入：一个 AppKit 窗口挂一个 ghostty surface，跑 shell、能输入能渲染（真渲染真 PTY，不是截图演示）。能力审计报告逐项给出「有 API / 无 / 绕法」：网格与 hyperlink 读取、屏幕快照、surface 之上合成层、配置加载与运行时改、键位拦截，且结论有实测证据支撑。hit 数据源（网格/hyperlink）无且无绕法 → 停（ok:false）。",
  },
  q1: {
    title: "壳",
    accept:
      "surface 的 window/tab/split 上下文回调接布局树；焦点/关闭/resize 全链。标签分屏日常用法在嵌入引擎上成立；⌘W 多 pane 只关一面、⌘⇧Enter 放大焦点面，语义清楚且有实测。",
  },
  q2: {
    title: "配置",
    accept:
      "加载 Ghostty 配置（主题、字体、键位）+ 热重载；ninja.toml 只管插件/宿主特有项；缺省主题 One Dark Pro。用户既有 ghostty 配置的常用子集直接生效；主题/字体/键位实测（像素或回读取证）。",
  },
  q3: {
    title: "插件系统 + 三门禁",
    accept:
      "hit、layer、input、theme.set 接上；示例插件（ninja-preview/ninja-theme）只走公开协议（进程外 JSON，不链宿主内部 API、不链 ghostty.h），协议契约测试在。三大门禁全部通过：空载内存对照 Ghostty 本尊（同量级）、第一个插件（点击路径→终端内看文本/代码）、关掉即轻（禁用后内存回空载、无残留进程）。Ghostty 语义坑停在宿主适配器，不改协议。",
  },
  q4: {
    title: "分发",
    accept:
      "签名的 macOS .app + DMG；默认零插件（分发物不含预览/主题插件）；本机可用真实签名身份（无身份即失败，不出 adhoc）。别人能装上当日常终端用；仓库描述与实际一致。仓库仍不公开。",
  },
};

const phaseId = args && typeof args.phase === "string" ? args.phase : "";
if (PHASE_IDS.indexOf(phaseId) < 0) {
  throw new Error("args.phase must be one of q0..q4");
}
const spec = PHASES[phaseId];
const docs = `${REPO}/PRODUCT.md ${REPO}/PLAN.md`;

const HISTORY = [
  "历史在 git，只当参考与证据、不当基线（PLAN.md：实现按当前 PLAN 从文档重做，不从旧树继续打补丁；环境事实可直接复用）：",
  "- git show 1240428:docs/Q0-CAPABILITY-AUDIT.md —— 上一轮（旧树）嵌入审计全文，环境坑记录。",
  "- git show 1240428:vendor/ghostty/fetch.sh 、build.sh 、xcrun-shim/xcrun 、patches/0001* —— 旧树钉版构建件（当前树 vendor/ 是 q0 新做的，优先看当前树）。",
  "- tag v1-engine 是已退役的自研引擎路线，与本题无关。",
].join("\n");

const ENV = [
  "本机现状（q0 完成后）：workspace 有 crates/ninja（宿主）与 crates/ghostty-sys（FFI）；vendor/ghostty 已钉版构建（ghostty a887df42 + zig 0.15.2，out/lib/libghostty-internal.a 就绪，src/ 不入库）；cargo build/test 直接可用。",
  "GUI 取证一律按 PLAN「E2E 虚拟屏幕」跑：scripts/e2e/virtual-display hold <w> <h> 0 & → 从其 stdout JSON 取 displayID → NINJA_E2E_SCREEN=<displayID> 跑宿主 → 用完 kill。拿不到 displayID 就中止取证，不许落主屏。截图按窗口 ID（screencapture -l）。收尾杀掉自己的 hold 进程（serial 已按 pid 唯一，残留不再堵新建，但仍要收尾干净）。",
  "Xcode 26.6 默认 SDK tbd 缺 arm64-macos：构建经 vendor/ghostty/build.sh 内置 xcrun-shim 指到 CLT SDK，勿动。MetalToolchain 已装。",
  "TCC 屏幕录制授权此前已给；若被拦如实记录（文本级结论不受影响）。",
].join("\n");

const rules = [
  "只做本阶段，不自动开下一阶段；禁止顺手做 Agent、图片/PDF 预览、插件市场、Linux、把浏览器/工作区做进宿主、Ghostty 重度 fork。",
  "终端核是 vendored libghostty 嵌入（include/ghostty.h，静态链），像 cmux 一样站在 Ghostty 上做应用；ninja 的产品面是 ADE 插件协议。",
  "嵌入 API pre-1.0：钉 commit，一切破坏性升级显式做；zig 版本随钉点。",
  "ADE 协议（q3 起）是进程外 JSON：插件不链宿主内部 API，也不链 ghostty.h；Ghostty 语义坑停在宿主适配器，不改协议。",
  "空载不得加载插件运行时、不得创建插件 socket、不得拉插件进程。",
  "验证必须跑命令或读代码取证，不得相信实施者的自我声明。",
].join(" ");

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
    gate: { type: "string", enum: ["pass", "blocked"] },
    summary: { type: "string" },
    files: { type: "string" },
    evidence: { type: "string" },
    residualRisk: { type: "string" },
  },
  required: ["done", "gate", "summary", "files", "evidence", "residualRisk"],
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

phase("盘点");
log(`阶段 ${phaseId} ${spec.title}：盘点`);
const inventory = await agent(
  `仓库 ${REPO}。只读盘点（允许只读命令如 ls / git show / git log / cargo metadata，但不得安装、下载、写任何文件）。\n合同文档：${docs}\n本阶段：${phaseId} ${spec.title}。验收标准：${spec.accept}\n${HISTORY}\n${ENV}\n${rules}\n对照验收标准写出差距（gap）与实施计划（plan）。上一阶段未完成则 ready=false 并在 blockedBy 写明（PLAN「进度」节与 git log 可查）。不要改文件。`,
  { label: `inventory:${phaseId}`, schema: inventorySchema },
);
if (inventory === null) {
  return { phase: phaseId, ok: false, stage: "inventory", reason: "missing", inventory: null, implement: null, verify: [] };
}
if (!inventory.ready) {
  return { phase: phaseId, ok: false, stage: "inventory", reason: inventory.blockedBy || inventory.gap, inventory, implement: null, verify: [] };
}

phase("实施");
log(`阶段 ${phaseId} ${spec.title}：实施`);
const implementPrompt = (feedback) =>
  `仓库 ${REPO}。实施阶段 ${phaseId} ${spec.title}。\n验收标准：${spec.accept}\n盘点差距：${inventory.gap}\n盘点计划：${inventory.plan}\n${HISTORY}\n${ENV}\n${rules}\n目标树与所有权以 PLAN.md 为准（宿主 crate 是 crates/ninja，FFI 是 crates/ghostty-sys）。取证证据落 docs/q${phaseId.slice(1)}-evidence/（脚本+日志，可复跑）。\n不修改 PRODUCT.md / PLAN.md 的合同内容（「进度」节的阶段状态行除外）。完成后 git add -A 提交到 master（提交信息一行概括本阶段，含证据；不要裹挟无关文件）。\n${feedback ? `验证反馈，只修这些：${feedback}` : "按盘点计划实施，不要顺手做下一阶段。"}\n最后报告：改动文件、取证证据（命令+产物路径）、残留风险；若命门条件触发（如 q0 的 hit 数据源无且无绕法）gate=blocked 并说明。`;

let implement = await agent(implementPrompt(""), {
  label: `implement:${phaseId}:1`,
  thread: "implementer",
  schema: implementSchema,
});
if (implement === null) {
  return { phase: phaseId, ok: false, stage: "implement", reason: "missing", inventory, implement: null, verify: [] };
}
if (implement.gate === "blocked") {
  return { phase: phaseId, ok: false, stage: "gate", reason: implement.residualRisk || implement.summary, inventory, implement, verify: [] };
}

async function runVerify(round) {
  const result = await agent(
    `仓库 ${REPO}。你是独立验证员，与实施者不同会话；不得采信实施者的自我声明，只认你亲手跑出来的证据。\n阶段 ${phaseId} ${spec.title}。验收标准：${spec.accept}\n实施者声称：${implement.summary}\n改动文件：${implement.files}\n实施者证据：${implement.evidence}\n${ENV}\n${rules}\n对照 ${docs}。用编译、测试或运行取证（可复跑实施者的取证脚本，也可自己另取）；验收标准有一条不满足即 pass=false 并逐条列缺陷。取证跑在虚拟屏上（见 ENV）；收尾杀掉自己的 virtual-display hold。有缺陷就 fail，不给情面。`,
    { label: `verify:${phaseId}:${round}`, schema: verifySchema },
  );
  return { round, result };
}

phase("验证");
log(`阶段 ${phaseId} ${spec.title}：验证（第 1 轮）`);
const verify = [];
const first = await runVerify(1);
verify.push(first);
if (first.result === null) {
  return { phase: phaseId, ok: false, stage: "verify", reason: "missing", inventory, implement, verify };
}
if (first.result.pass === true) {
  return { phase: phaseId, ok: true, stage: "done", inventory, implement, verify };
}

log(`阶段 ${phaseId}：验证未过，实施者修一轮`);
implement = await agent(implementPrompt(first.result.defects), {
  label: `implement:${phaseId}:2`,
  thread: "implementer",
  schema: implementSchema,
});
if (implement === null) {
  return { phase: phaseId, ok: false, stage: "revise", reason: "missing", inventory, implement: null, verify };
}
if (implement.gate === "blocked") {
  return { phase: phaseId, ok: false, stage: "gate", reason: implement.residualRisk || implement.summary, inventory, implement, verify };
}

log(`阶段 ${phaseId}：验证（第 2 轮）`);
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
