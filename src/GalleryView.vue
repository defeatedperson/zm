<script setup lang="ts">
/// 画廊主页：屏幕状态条、封面网格、卡片菜单。
import { computed, onBeforeUnmount, onMounted, reactive, ref } from "vue";
import { convertFileSrc } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import {
  applyTo,
  clearAll,
  clearScreen,
  pickAndAdd,
  refresh,
  removeGalleryItem,
  screenShortName,
  store,
} from "./store";
import {
  optimizeCancel,
  optimizeStart,
  type GalleryEntry,
} from "./gallery";

// 全部显示器中的最大物理分辨率（判断视频是否超规格用）
const maxDisplay = computed(() => {
  let w = 0;
  let h = 0;
  for (const s of store.screens) {
    w = Math.max(w, s.width);
    h = Math.max(h, s.height);
  }
  return { w, h };
});

/** 是否超过所有显示器的分辨率上限 */
const isOverSpec = (entry: GalleryEntry) =>
  !!entry.meta &&
  (entry.meta.width > maxDisplay.value.w || entry.meta.height > maxDisplay.value.h);

/** 帧率显示：30000/1001 → 30，29.97 保留一位小数 */
const fpsText = (entry: GalleryEntry) => {
  if (!entry.meta || entry.meta.fpsNum <= 0) return "";
  const fps = entry.meta.fpsNum / entry.meta.fpsDen;
  const rounded = Math.round(fps);
  return Math.abs(fps - rounded) < 0.05 ? `${rounded}` : fps.toFixed(1);
};

// ---- 卡片右键菜单 ----
const menu = ref<{ path: string; x: number; y: number } | null>(null);
// 两击确认删除：第一次点击进入待确认状态
const pendingRemove = ref<string | null>(null);

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
  srcW: 0,
  srcH: 0,
  srcFps: 0,
  targetRes: 0, // 0 = 原始；否则为档位值（横屏=高度，竖屏=宽度）
  targetFps: 0, // 0 = 保持原始帧率
  percent: 0,
  error: "",
});

/** 正在使用中的素材（优化/移除均禁用） */
const isInUse = (entry: GalleryEntry) =>
  (usageByPath.value.get(entry.path)?.length ?? 0) > 0;

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

/** 是否有可执行的优化项 */
const canOptimize = computed(
  () => optimize.targetRes !== 0 || optimize.targetFps !== 0,
);

function openOptimize(entry: GalleryEntry) {
  const meta = entry.meta;
  if (!meta) return;
  optimize.entry = entry;
  optimize.srcW = meta.width;
  optimize.srcH = meta.height;
  optimize.srcFps =
    meta.fpsDen > 0 ? Math.round((meta.fpsNum / meta.fpsDen) * 10) / 10 : 0;
  // 默认选「输出宽高均能覆盖最大显示器的最大降档档位」（从大到小取第一个合格项，
  // 兼顾画质与降载；无档位或全部覆盖不了时保持原始）
  const { w: maxW, h: maxH } = maxDisplay.value;
  optimize.targetRes = 0;
  for (const o of resOptions.value) {
    if (o.value === 0) continue;
    const { w, h } = scaledDimsFor(o.value);
    if (w >= maxW && h >= maxH) {
      optimize.targetRes = o.value;
      break;
    }
  }
  optimize.targetFps = 0;
  optimize.phase = "select";
  optimize.percent = 0;
  optimize.error = "";
}

function closeOptimize() {
  if (optimize.phase === "progress") return; // 进度中不允许点遮罩关闭
  optimize.entry = null;
}

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

const menuEntry = computed(
  () => store.entries.find((e) => e.path === menu.value?.path) ?? null,
);

/** 路径 -> 使用该壁纸的屏幕列表 */
const usageByPath = computed(() => {
  const map = new Map<string, typeof store.screens>();
  for (const s of store.screens) {
    if (!s.wallpaperPath) continue;
    const list = map.get(s.wallpaperPath) ?? [];
    list.push(s);
    map.set(s.wallpaperPath, list);
  }
  return map;
});

const playingCount = computed(
  () => store.screens.filter((s) => s.wallpaperPath).length,
);

// ---- 菜单 ----
function openMenu(entry: GalleryEntry, x: number, y: number) {
  pendingRemove.value = null;
  menu.value = {
    path: entry.path,
    x: Math.min(x, window.innerWidth - 200),
    y: Math.min(y, window.innerHeight - 260),
  };
}

function closeMenu() {
  menu.value = null;
  pendingRemove.value = null;
}

// ---- 移除（两击确认）----
async function removeFromMenu() {
  const entry = menuEntry.value;
  if (!entry) return;
  if (pendingRemove.value !== entry.path) {
    pendingRemove.value = entry.path;
    setTimeout(() => {
      if (pendingRemove.value === entry.path) pendingRemove.value = null;
    }, 3000);
    return;
  }
  pendingRemove.value = null;
  closeMenu();
  await removeGalleryItem(entry);
}

// ---- 展示辅助 ----
function formatSize(bytes: number): string {
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024).toFixed(0)} KB`;
}

const thumbSrc = (entry: GalleryEntry) =>
  entry.thumbPath ? convertFileSrc(entry.thumbPath) : "";
</script>

<template>
  <div class="gallery-page">
    <section class="screens">
      <div
        v-for="s in store.screens"
        :key="s.label"
        class="chip"
        :class="{ active: !!s.wallpaperPath }"
        :title="s.wallpaperPath ?? '未使用'"
      >
        <span class="dot" />
        <span class="chip-name">{{ screenShortName(s.name) }}</span>
        <span class="chip-dim">{{ s.width }}×{{ s.height }}</span>
        <button
          v-if="s.wallpaperPath"
          class="chip-clear"
          title="恢复静态壁纸"
          @click="clearScreen(s.label)"
        >✕</button>
      </div>
      <button v-if="playingCount > 1" class="link" @click="clearAll">全部清除</button>
    </section>

    <div v-if="store.entries.length > 0" class="grid-scroll">
      <section class="grid">
      <article
        v-for="entry in store.entries"
        :key="entry.path"
        class="card"
        @click="applyTo(entry, null)"
        @contextmenu.prevent="openMenu(entry, $event.clientX, $event.clientY)"
      >
        <div class="thumb">
          <img
            v-if="entry.thumbPath"
            :src="thumbSrc(entry)"
            :alt="entry.name"
            loading="lazy"
            decoding="async"
            draggable="false"
          />
          <div v-else-if="store.generating.has(entry.path)" class="placeholder">生成封面中…</div>
          <div v-else class="placeholder">
            {{ store.thumbFailed.has(entry.path) ? "无法生成封面" : "暂无封面" }}
          </div>
          <div v-if="usageByPath.get(entry.path)?.length" class="badge">
            使用中 ×{{ usageByPath.get(entry.path)!.length }}
          </div>
        </div>
        <div class="card-body">
          <span class="name" :title="entry.name">{{ entry.name }}</span>
          <span class="meta">
            {{ formatSize(entry.sizeBytes) }}
            <span v-if="entry.kind === 'image'" class="kind-tag">静态</span>
            <span
              v-else-if="entry.meta"
              class="kind-tag"
              :class="{ warn: isOverSpec(entry) }"
              :title="isOverSpec(entry) ? '分辨率超过显示器上限，建议优化以降低资源占用' : ''"
            >
              {{ entry.meta.width }}×{{ entry.meta.height }}
              <template v-if="fpsText(entry)"> · {{ fpsText(entry) }}fps</template>
            </span>
            <span v-else class="kind-tag muted">规格探测中…</span>
          </span>
        </div>
        <button
          class="menu-btn"
          title="更多操作"
          @click.stop="openMenu(entry, $event.clientX, $event.clientY)"
        >⋯</button>
      </article>
      </section>
    </div>

    <section v-else class="empty">
      <p>画廊是空的，先添加一些视频或图片吧</p>
      <button class="primary" :disabled="store.busy" @click="pickAndAdd">添加素材</button>
    </section>

    <!-- 卡片菜单 -->
    <div v-if="menu" class="menu-overlay" @click.stop="closeMenu" />
    <div
      v-if="menu && menuEntry"
      class="menu"
      :style="{ left: `${menu.x}px`, top: `${menu.y}px` }"
      @click.stop
    >
      <button class="menu-item" @click="applyTo(menuEntry, null); closeMenu()">
        应用到全部屏幕
      </button>
      <div class="menu-divider" />
      <button
        v-for="s in store.screens"
        :key="s.label"
        class="menu-item"
        @click="applyTo(menuEntry, s.label); closeMenu()"
      >
        应用到 {{ screenShortName(s.name) }}
        <span v-if="s.wallpaperPath === menuEntry.path" class="menu-check">●</span>
      </button>
      <div class="menu-divider" />
      <button
        v-if="menuEntry.kind === 'video'"
        class="menu-item"
        :disabled="isInUse(menuEntry) || !menuEntry.meta"
        :title="
          isInUse(menuEntry)
            ? '该视频正在使用中，请先在顶部状态条停止对应屏幕（✕）'
            : !menuEntry.meta
              ? '无法读取视频规格'
              : ''
        "
        @click="openOptimize(menuEntry); closeMenu()"
      >
        优化
      </button>
      <button
        class="menu-item danger"
        :disabled="isInUse(menuEntry)"
        :title="isInUse(menuEntry) ? '该视频正在使用中，请先停止对应的屏幕壁纸' : ''"
        @click="removeFromMenu"
      >{{ pendingRemove === menuEntry.path ? "再点一次确认移除" : "移除" }}</button>
    </div>

    <!-- 优化弹窗 -->
    <div
      v-if="optimize.entry"
      class="optimize-mask"
      @click="closeOptimize()"
    >
      <div class="optimize-dialog" @click.stop>
        <h3>优化视频</h3>
        <p class="opt-name" :title="optimize.entry.name">{{ optimize.entry.name }}</p>

        <template v-if="optimize.phase === 'select'">
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
          <p v-if="!canOptimize" class="opt-note">规格已达标，无需优化。</p>
          <p v-else class="opt-note">
            预计解码负载降至 ~{{ loadPercent }}%，耗时取决于素材长度。
          </p>
          <div class="opt-actions">
            <button @click="closeOptimize()">取消</button>
            <button
              class="primary"
              :disabled="!canOptimize"
              @click="startOptimize"
            >开始优化</button>
          </div>
        </template>

        <template v-else-if="optimize.phase === 'progress'">
          <div class="opt-progress-track">
            <div class="opt-progress-bar" :style="{ width: optimize.percent + '%' }" />
          </div>
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
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 根容器补齐滚动约束链：允许收缩到父容器高度内，网格才能内部滚动 */
.gallery-page {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.screens {
  margin-top: 18px;
  flex-shrink: 0;
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.chip {
  display: flex;
  align-items: center;
  gap: 6px;
  background: #1b1d24;
  border: 1px solid #2b2e37;
  border-radius: 999px;
  padding: 5px 12px;
  font-size: 12px;
  color: #9aa0ad;
}

.chip.active {
  border-color: #3b6ef5;
  color: #dfe2ea;
}

.dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #4a4e59;
}

.chip.active .dot {
  background: #4ade80;
}

.chip-name {
  font-weight: 600;
}

.chip-dim {
  color: #6f7480;
}

.chip-clear {
  border: none;
  background: none;
  padding: 0 2px;
  font-size: 11px;
  color: #8a8f9c;
}

.chip-clear:hover {
  color: #f07178;
}

button.link {
  border: none;
  background: none;
  color: #6f9bff;
  padding: 4px 8px;
}

button.link:hover {
  text-decoration: underline;
}

/* 滚动容器：独立于网格本身，规避 grid+overflow+flex 的边界行为 */
.grid-scroll {
  margin-top: 16px;
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding-right: 4px;
}

.grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
  gap: 14px;
  align-content: start;
}

.card {
  position: relative;
  background: #1b1d24;
  border: 1px solid #2b2e37;
  border-radius: 10px;
  overflow: hidden;
  cursor: pointer;
  transition: border-color 0.15s;
  /* 长列表优化：跳过离屏卡片的布局与渲染，DOM 保留（可搜索） */
  content-visibility: auto;
  contain-intrinsic-size: auto 320px 232px;
}

.card:hover {
  border-color: #3b6ef5;
}

.thumb {
  position: relative;
  aspect-ratio: 16 / 9;
  background: #101116;
}

.thumb img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}

.placeholder {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #565b66;
  font-size: 12px;
}

.badge {
  position: absolute;
  top: 8px;
  left: 8px;
  background: rgba(59, 110, 245, 0.9);
  color: #fff;
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 999px;
}

.card-body {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 10px 12px 12px;
}

.name {
  font-size: 13px;
  color: #dfe2ea;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.meta {
  font-size: 11px;
  color: #6f7480;
}

.kind-tag {
  margin-left: 4px;
  padding: 1px 6px;
  border-radius: 999px;
  background: #262932;
  color: #9aa0ad;
}

.kind-tag.warn {
  background: rgba(240, 180, 113, 0.14);
  color: #f0b471;
}

.kind-tag.muted {
  opacity: 0.6;
}

.menu-btn {
  position: absolute;
  top: 8px;
  right: 8px;
  width: 28px;
  height: 28px;
  padding: 0;
  border-radius: 6px;
  background: rgba(16, 17, 22, 0.72);
  border-color: transparent;
  color: #cdd2dd;
  font-size: 15px;
  line-height: 1;
  opacity: 0;
  transition: opacity 0.15s;
}

.card:hover .menu-btn,
.menu-btn:focus {
  opacity: 1;
}

.menu-overlay {
  position: fixed;
  inset: 0;
  z-index: 90;
}

.menu {
  position: fixed;
  z-index: 91;
  min-width: 180px;
  background: #1e2028;
  border: 1px solid #33363f;
  border-radius: 10px;
  padding: 6px;
  box-shadow: 0 10px 32px rgba(0, 0, 0, 0.5);
  display: flex;
  flex-direction: column;
}

.menu-item {
  border: none;
  background: none;
  text-align: left;
  padding: 8px 12px;
  border-radius: 6px;
  font-size: 13px;
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.menu-item:hover {
  background: #2a2e38;
}

.menu-item.danger {
  color: #f07178;
}

.menu-item.danger:hover {
  background: rgba(240, 113, 120, 0.12);
}

.menu-check {
  color: #4ade80;
  font-size: 11px;
}

.menu-divider {
  height: 1px;
  background: #2b2e37;
  margin: 4px 6px;
}

.empty {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 16px;
  color: #6f7480;
}

/* 优化弹窗 */
.optimize-mask {
  position: fixed;
  inset: 0;
  z-index: 100;
  background: rgba(10, 11, 14, 0.6);
  display: flex;
  align-items: center;
  justify-content: center;
}

.optimize-dialog {
  width: 440px;
  max-width: calc(100vw - 48px);
  background: #1b1d24;
  border: 1px solid #2b2e37;
  border-radius: 12px;
  padding: 20px 22px;
}

.optimize-dialog h3 {
  font-size: 15px;
  font-weight: 600;
}

.opt-name {
  margin-top: 8px;
  font-size: 13px;
  color: #9aa0ad;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.opt-line {
  margin-top: 8px;
  font-size: 13px;
  color: #dfe2ea;
}

.opt-check {
  margin-top: 10px;
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  color: #dfe2ea;
  cursor: pointer;
}

.opt-check input {
  accent-color: #3b6ef5;
}

.opt-select-row {
  margin-top: 10px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  font-size: 13px;
  color: #dfe2ea;
}

.opt-select {
  background: #14161b;
  border: 1px solid #262932;
  border-radius: 8px;
  color: #dfe2ea;
  font-family: inherit;
  font-size: 13px;
  padding: 6px 10px;
  min-width: 160px;
}

.opt-select:focus {
  outline: none;
  border-color: #3b6ef5;
}

.opt-preview {
  margin-top: 6px;
  font-size: 11px;
  color: #6f9bff;
  text-align: right;
}

.opt-note {
  margin-top: 10px;
  font-size: 11px;
  color: #6f7480;
}

.opt-actions {
  margin-top: 16px;
  display: flex;
  justify-content: flex-end;
  gap: 10px;
}

.opt-progress-track {
  margin-top: 14px;
  height: 8px;
  border-radius: 999px;
  background: #14161b;
  overflow: hidden;
}

.opt-progress-bar {
  height: 100%;
  border-radius: 999px;
  background: #3b6ef5;
  transition: width 0.2s ease;
}

.opt-done {
  margin-top: 12px;
  font-size: 13px;
  color: #4ade80;
}

.opt-error {
  margin-top: 12px;
  font-size: 13px;
  color: #f07178;
  word-break: break-all;
}
</style>
