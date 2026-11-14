<script setup lang="ts">
/// 优化/修复弹窗（从 GalleryView 拆出）：四态状态机 select → progress → done / error。
/// 通过 open(entry, repair) 打开（父级 ref 调用）；转码事件监听随组件生命周期管理。
/// 弹窗容器用 ElDialog：进度阶段禁止关闭（Esc/遮罩点击均被 before-close 拦截）。
import { computed, onBeforeUnmount, onMounted, reactive } from "vue";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { maxDisplay, refresh, store } from "../../store";
import { optimizeCancel, optimizeStart, type GalleryEntry } from "../../gallery";

// ---- 优化弹窗（四态：select 方案 / progress 进度 / done 完成 / error 失败） ----
const RES_TIERS = [
  { value: 2160, label: "4k" },
  { value: 1440, label: "2k" },
  { value: 1080, label: "1080p" },
  { value: 720, label: "720p" },
];
const FPS_TIERS = [60, 30, 24];

const optimize = reactive({
  entry: null as GalleryEntry | null,
  phase: "select" as "select" | "progress" | "done" | "error",
  /** 修复模式：预选原始分辨率+帧率，用于重建异常素材的编码结构 */
  repair: false,
  srcW: 0,
  srcH: 0,
  srcFps: 0,
  targetRes: 0, // 0 = 原始；否则为档位值（横屏=高度，竖屏=宽度）
  targetFps: 0, // 0 = 保持原始帧率
  percent: 0,
  error: "",
});

/** 对外入口：打开弹窗（repair = 一键修复模式，锁定原始规格） */
function open(entry: GalleryEntry, repair = false) {
  const meta = entry.meta;
  if (!meta) return;
  optimize.entry = entry;
  optimize.repair = repair;
  optimize.srcW = meta.width;
  optimize.srcH = meta.height;
  optimize.srcFps =
    meta.fpsDen > 0 ? Math.round((meta.fpsNum / meta.fpsDen) * 10) / 10 : 0;
  // 修复模式固定「原始+原始」；普通模式默认选「输出宽高均能覆盖最大显示器
  // 的最大降档档位」（从大到小取第一个合格项，兼顾画质与降载；
  // 无档位或全部覆盖不了时保持原始）
  optimize.targetRes = 0;
  if (!repair) {
    const { w: maxW, h: maxH } = maxDisplay.value;
    for (const o of resOptions.value) {
      if (o.value === 0) continue;
      const { w, h } = scaledDimsFor(o.value);
      if (w >= maxW && h >= maxH) {
        optimize.targetRes = o.value;
        break;
      }
    }
  }
  optimize.targetFps = 0;
  optimize.phase = "select";
  optimize.percent = 0;
  optimize.error = "";
}

defineExpose({ open });

function closeOptimize() {
  if (optimize.phase === "progress") return; // 进度中不允许关闭
  optimize.entry = null;
}

/** 按档位计算等比输出尺寸（偶数对齐） */
function scaledDimsFor(tierValue: number): { w: number; h: number } {
  const m = optimize.entry?.meta;
  if (!m || tierValue <= 0) {
    return { w: m?.width ?? 0, h: m?.height ?? 0 };
  }
  const portrait = m.height > m.width;
  const srcL = portrait ? m.width : m.height;
  const s = tierValue / srcL;
  return {
    w: Math.floor((m.width * s) / 2) * 2,
    h: Math.floor((m.height * s) / 2) * 2,
  };
}

/** 目标分辨率下拉选项：所有降档位（横屏=高度，竖屏=宽度） */
const resOptions = computed(() => {
  const m = optimize.entry?.meta;
  if (!m) return [];
  const srcL = Math.min(m.width, m.height); // 档位作用于短边
  return [
    { value: 0, label: "原始" },
    ...RES_TIERS.filter((t) => t.value < srcL).map((t) => ({
      value: t.value,
      label: t.label,
    })),
  ];
});

/** 目标帧率下拉选项：低于源帧率的档位 */
const fpsOptions = computed(() => {
  if (optimize.srcFps <= 0) return [{ value: 0, label: "原始" }];
  const valid = FPS_TIERS.filter((f) => f < optimize.srcFps - 0.5);
  return [
    { value: 0, label: "原始" },
    ...valid.map((f) => ({ value: f, label: `${f}fps` })),
  ];
});

/** 当前选择的输出尺寸 */
const chosenDims = computed(() => scaledDimsFor(optimize.targetRes));

/** 勾选方案后的预计解码负载（相对源） */
const loadPercent = computed(() => {
  const src = optimize.srcW * optimize.srcH * (optimize.srcFps || 30);
  const { w, h } = chosenDims.value;
  const outFps = optimize.targetFps || optimize.srcFps || 30;
  if (src <= 0) return 100;
  return Math.max(1, Math.round(((w * h * outFps) / src) * 100));
});

/** 是否有可执行的优化项：保持原始+原始也允许执行——重编码会重建
 *  编码结构（关键帧/GOP），是修复异常素材的正规手段，画质无损降级最小 */
const canOptimize = computed(() => optimize.entry !== null);

async function startOptimize() {
  const entry = optimize.entry;
  if (!entry || !canOptimize.value) return;
  const meta = entry.meta;
  const { w, h } = chosenDims.value;
  // 帧率：选了档位用整数；原始则透传源帧率（未知时传 0 由后端回退）
  const fpsNum = optimize.targetFps || (meta?.fpsNum ?? 0);
  const fpsDen = optimize.targetFps ? 1 : (meta?.fpsDen ?? 0);
  try {
    await optimizeStart(entry.path, w, h, fpsNum, fpsDen);
    optimize.phase = "progress";
    optimize.percent = 0;
  } catch (e) {
    optimize.error = String(e);
    optimize.phase = "error";
  }
}

function cancelOptimize() {
  void optimizeCancel();
}

const win = getCurrentWebviewWindow();

// 转码事件监听：视图切换（v-if）会卸载重挂本组件，
// 必须保存 unlisten 并在卸载时注销，否则监听器随每次切换不断累积
let unlistenProgress: (() => void) | null = null;
let unlistenDone: (() => void) | null = null;
let listenersDisposed = false;

onMounted(async () => {
  const up = await win.listen<{ path: string; percent: number }>(
    "transcode:progress",
    (event) => {
      if (optimize.entry && event.payload.path === optimize.entry.path) {
        optimize.percent = event.payload.percent;
      }
    },
  );
  if (listenersDisposed) {
    up();
    return;
  }
  unlistenProgress = up;
  const ud = await win.listen<{
    path: string;
    ok: boolean;
    cancelled: boolean;
    error: string | null;
  }>("transcode:done", (event) => {
    if (!optimize.entry || event.payload.path !== optimize.entry.path) return;
    if (event.payload.ok) {
      optimize.phase = "done";
      // 修复替换了文件：旧封面的失败标记一并清除，refresh 后立即重新生成
      const okPath = event.payload.path;
      store.thumbFailed = new Set(
        [...store.thumbFailed].filter((p) => p !== okPath),
      );
      void refresh();
    } else if (event.payload.cancelled) {
      optimize.entry = null;
    } else {
      optimize.error = event.payload.error ?? "优化失败";
      optimize.phase = "error";
    }
  });
  if (listenersDisposed) {
    ud();
    return;
  }
  unlistenDone = ud;
});

onBeforeUnmount(() => {
  listenersDisposed = true;
  unlistenProgress?.();
  unlistenDone?.();
  unlistenProgress = null;
  unlistenDone = null;
});
</script>

<template>
  <el-dialog
    :model-value="!!optimize.entry"
    :show-close="false"
    width="440px"
    align-center
    class="optimize-dialog"
    append-to-body
    @update:model-value="!$event && closeOptimize()"
  >
    <template v-if="optimize.entry">
      <h3 class="title">{{ optimize.repair ? "修复视频" : "优化视频" }}</h3>
      <p class="opt-name" :title="optimize.entry.name">{{ optimize.entry.name }}</p>

      <template v-if="optimize.phase === 'select'">
        <p v-if="optimize.repair" class="opt-warn">
          {{ optimize.entry.meta?.kfRisk }}，直接播放会在循环时冻结。
          将保持原始分辨率与帧率重建编码结构，完成后即可正常应用。
        </p>
        <p class="opt-line">
          源：{{ optimize.srcW }}×{{ optimize.srcH }}
          <template v-if="optimize.srcFps"> @{{ optimize.srcFps }}fps</template>
        </p>
        <p class="opt-line">
          显示器最高：{{ maxDisplay.w }}×{{ maxDisplay.h }}
        </p>
        <div class="opt-select-row">
          <span>目标分辨率</span>
          <select v-model.number="optimize.targetRes" class="opt-select">
            <option v-for="o in resOptions" :key="o.value" :value="o.value">
              {{ o.label }}
            </option>
          </select>
        </div>
        <p v-if="optimize.targetRes > 0" class="opt-preview">
          {{ optimize.srcW }}×{{ optimize.srcH }} →
          {{ chosenDims.w }}×{{ chosenDims.h }}
        </p>
        <div class="opt-select-row">
          <span>目标帧率</span>
          <select v-model.number="optimize.targetFps" class="opt-select">
            <option v-for="o in fpsOptions" :key="o.value" :value="o.value">
              {{ o.label }}
            </option>
          </select>
        </div>
        <p v-if="optimize.targetRes === 0 && optimize.targetFps === 0" class="opt-note">
          保持原画质重建编码（修复结构异常 / 降低解码负载的规格不变）。
        </p>
        <p v-else class="opt-note">
          预计解码负载降至 ~{{ loadPercent }}%，耗时取决于素材长度。
        </p>
        <div class="opt-actions">
          <button @click="closeOptimize()">取消</button>
          <button
            class="primary"
            :disabled="!canOptimize"
            @click="startOptimize"
          >{{ optimize.repair ? "开始修复" : "开始优化" }}</button>
        </div>
      </template>

      <template v-else-if="optimize.phase === 'progress'">
        <el-progress
          class="opt-progress"
          :percentage="optimize.percent"
          :show-text="false"
          :stroke-width="8"
        />
        <p class="opt-note">{{ optimize.percent }}% — 正在转码，可随时取消</p>
        <div class="opt-actions">
          <button @click="cancelOptimize">取消任务</button>
        </div>
      </template>

      <template v-else-if="optimize.phase === 'done'">
        <p class="opt-done">✓ 优化完成，原件已移入备份目录</p>
        <div class="opt-actions">
          <button class="primary" @click="closeOptimize()">完成</button>
        </div>
      </template>

      <template v-else>
        <p class="opt-error">{{ optimize.error || "优化失败" }}</p>
        <div class="opt-actions">
          <button @click="closeOptimize()">关闭</button>
        </div>
      </template>
    </template>
  </el-dialog>
</template>

<style>
/* 非 scoped：el-dialog 内容 teleport 到 body，scoped 样式作用不到 */
.optimize-dialog {
  border: 1px solid #2b2e37;
}

.optimize-dialog .title {
  font-size: 15px;
  font-weight: 600;
  color: #dfe2ea;
}

.optimize-dialog .opt-name {
  margin-top: 8px;
  font-size: 13px;
  color: #9aa0ad;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.optimize-dialog .opt-warn {
  margin: 10px 0 2px;
  padding: 10px 12px;
  border: 1px solid rgba(232, 90, 90, 0.4);
  background: rgba(232, 90, 90, 0.08);
  border-radius: 8px;
  color: #ff9d9d;
  font-size: 13px;
  line-height: 1.6;
  text-align: left;
}

.optimize-dialog .opt-line {
  margin-top: 8px;
  font-size: 13px;
  color: #dfe2ea;
}

.optimize-dialog .opt-select-row {
  margin-top: 10px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  font-size: 13px;
  color: #dfe2ea;
}

.optimize-dialog .opt-select {
  background: #14161b;
  border: 1px solid #262932;
  border-radius: 8px;
  color: #dfe2ea;
  font-family: inherit;
  font-size: 13px;
  padding: 6px 10px;
  min-width: 160px;
}

.optimize-dialog .opt-select:focus {
  outline: none;
  border-color: #3b6ef5;
}

.optimize-dialog .opt-preview {
  margin-top: 6px;
  font-size: 11px;
  color: #6f9bff;
  text-align: right;
}

.optimize-dialog .opt-note {
  margin-top: 10px;
  font-size: 11px;
  color: #6f7480;
}

.optimize-dialog .opt-actions {
  margin-top: 16px;
  display: flex;
  justify-content: flex-end;
  gap: 10px;
}

.optimize-dialog .opt-progress {
  margin-top: 14px;
}

.optimize-dialog .opt-done {
  margin-top: 12px;
  font-size: 13px;
  color: #4ade80;
}

.optimize-dialog .opt-error {
  margin-top: 12px;
  font-size: 13px;
  color: #f07178;
  word-break: break-all;
}
</style>
