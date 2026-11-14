<script setup lang="ts">
/// 壁纸窗口页：把当前媒体铺满本屏，切换时做 300ms 交叉淡化。
///
/// 图层架构（2026-11-13 切换动画）：
/// - 稳态 = 单元素、不带任何 opacity/transition 类 —— video 保持在
///   WebView2 硬件叠加直出路径上，与旧版完全一致（性能零回退）。
/// - 切换 = 叠加第二个元素：新视频自然起播（不经 currentTime/seek），
///   首帧就绪后 300ms 淡入；opacity 过渡只存在于新元素，结束后旧元素
///   移除、新元素清掉全部类回归直出。
///   视觉上是交叉淡化：合成公式 新×a + 旧×(1-a)，与「不透明旧层上盖
///   透明度 a 的新层」完全等价——所以旧层不需要淡出、不需要加任何样式。
///   旧层在过渡期保持原样继续播放，pause 冻结与移除在收尾的同一帧内
///   完成（中途对正在显示的 video 做 pause/样式变更会把它从直出路径
///   拉进合成路径，亚像素取整差异表现为 ~1px 位置抖动，实测）。
///   代价是 300ms 内两路视频并发解码（4K60 高规格素材理论上有压力，
///   换零抖动值得）。
///   这与文件底部的「反面教材」（JS 给 currentTime 赋值走 seek 路径 →
///   readyState 掉档黑帧 + opacity 踢出叠加层）不冲突：那个坑的根源是
///   对**正在播放的同一元素**做 seek 打点（循环场景结构性无解）；切换
///   场景是新元素从 0 自然起播、过渡有界、稳态零样式。
/// - 可打断：过渡中再切换 = 先立即收敛到当前可见层（样式瞬间跳到终态），
///   再从收敛层开新一轮淡化。
/// - 暂停叠加：用户暂停/全屏暂停期间切换，新视频停在首帧完成淡入并保持
///   暂停；恢复播放时播的是新素材。
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { nextTick, onBeforeUnmount, onMounted, ref } from "vue";

const win = getCurrentWebviewWindow();
const label = win.label;

// ---- 图层状态机 ----
// loading：新元素加载中（隐藏，等首帧）；fading：0→1 淡入中；
// settled：稳态（无样式类，直出路径）。旧层过渡期不参与任何状态变化
type Phase = "loading" | "fading" | "settled";
interface Layer {
  id: number;
  kind: "video" | "image";
  src: string;
  phase: Phase;
}

const layers = ref<Layer[]>([]);
let layerSeq = 0;

// 实际暂停 = 用户暂停 OR 铺满暂停，二者独立叠加
const userPaused = ref(false);
let fullscreenPaused = false;

/** 过渡时长；须与 CSS 里 .anim 的 transition 时长保持一致（定时器多留 60ms 收尾余量） */
const FADE_MS = 300;
let settleTimer: ReturnType<typeof setTimeout> | undefined;

// 图层 id -> 元素（v-for 的函数 ref 收集；元素卸载时 Vue 以 null 回调清理）
const elRefs = new Map<number, HTMLVideoElement | HTMLImageElement>();

function setLayerEl(id: number, el: unknown) {
  if (el) {
    elRefs.set(id, el as HTMLVideoElement);
    watchFirstFrame(id);
  } else {
    elRefs.delete(id);
  }
}

/** 新图层首帧就绪监听。视频双保险：rVFC（帧真正上屏，播放中首选信号）
 *  + loadeddata/rAF（暂停起播时 rVFC 可能不再回调，以「当前帧数据就绪」
 *  为准）。两者谁先到谁生效（startFade 有 phase 守卫），画面以 opacity 0
 *  起步，就绪前不可见，无黑帧风险。图片用 complete/load。 */
function watchFirstFrame(id: number) {
  const layer = layers.value.find((l) => l.id === id);
  if (!layer || layer.phase !== "loading") return;
  const el = elRefs.get(id);
  if (!el) return;
  if (layer.kind === "image") {
    const img = el as HTMLImageElement;
    if (img.complete) {
      startFade(id);
    } else {
      img.addEventListener("load", () => startFade(id), { once: true });
    }
    return;
  }
  const v = el as HTMLVideoElement;
  if (typeof v.requestVideoFrameCallback === "function") {
    v.requestVideoFrameCallback(() => startFade(id));
  }
  v.addEventListener(
    "loadeddata",
    () => requestAnimationFrame(() => startFade(id)),
    { once: true },
  );
}

/** 首帧就绪：开始淡入、排定收敛。旧层不做任何改动（见文件头注释：
 *  对显示中的 video 改样式/冻结都可能引起亚像素抖动） */
function startFade(id: number) {
  const arr = layers.value;
  const idx = arr.findIndex((l) => l.id === id);
  if (idx === -1 || arr[idx].phase !== "loading") return;
  arr[idx].phase = "fading";
  clearTimeout(settleTimer);
  settleTimer = setTimeout(settle, FADE_MS + 60);
  applyPause(); // 暂停态下新视频停在首帧淡入
  reportState();
}

/** 过渡收敛：只保留「最后一个可见层」并回归稳态。
 *  旧层先 pause 再移除——两者发生在同一次渲染帧内，pause 可能触发的
 *  重栅格化来不及上屏就被移除了，不可见。
 *  移除 transition 类后 opacity 计算值不变（已是终态），无视觉跳变；
 *  transition 属性消失使 video 重新获得硬件叠加直出资格 */
function settle() {
  clearTimeout(settleTimer);
  settleTimer = undefined;
  const arr = layers.value;
  let keep = -1;
  for (let i = arr.length - 1; i >= 0; i--) {
    if (arr[i].phase !== "loading") {
      keep = i;
      break;
    }
  }
  if (keep === -1) return; // 全在加载（各回调自会推进），不强行收敛
  for (let i = 0; i < arr.length; i++) {
    if (i === keep) continue;
    const el = elRefs.get(arr[i].id);
    if (el instanceof HTMLVideoElement) el.pause();
  }
  const kept = arr[keep];
  kept.phase = "settled";
  layers.value = [kept];
  applyPause();
  reportState();
}

/** 新媒体进入：空窗口直接稳态挂载（无动画）；已有画面则叠层交叉淡化 */
function applyMedia(path: string, kind: "video" | "image") {
  const src = convertFileSrc(path);
  const cur = layers.value[layers.value.length - 1];
  // 同素材重复应用（托盘重新应用等）：无操作，不打断正在播放的画面
  if (cur && cur.phase === "settled" && cur.src === src) return;
  // 过渡中再切换：先立即收敛到当前可见层，再开新一轮
  if (layers.value.some((l) => l.phase !== "settled")) settle();
  const id = ++layerSeq;
  if (layers.value.length === 0) {
    layers.value = [{ id, kind, src, phase: "settled" }];
  } else {
    layers.value = [...layers.value, { id, kind, src, phase: "loading" }];
  }
  void nextTick(() => {
    // ref 回调在渲染时已挂首帧监听；这里覆盖元素被复用等极端场景
    watchFirstFrame(id);
    // 初次挂载也要按当前暂停状态起播（autoplay 只负责默认播放）
    applyPause();
    reportState();
  });
}

/** 清空（恢复静态壁纸）：立即移除全部图层，无淡出 */
function clearMedia() {
  clearTimeout(settleTimer);
  settleTimer = undefined;
  layers.value = [];
  reportState();
}

/** 加载失败：移除该层。淡入层失败则自然回退——旧层未冻结仍在播，
 *  画面无中断；全部层都没了（初次挂载失败）回到空白 */
function onLayerError(id: number) {
  if (!layers.value.some((l) => l.id === id)) return;
  layers.value = layers.value.filter((l) => l.id !== id);
  if (layers.value.length === 0) {
    clearTimeout(settleTimer);
    settleTimer = undefined;
  }
  reportState();
}

function phaseClass(p: Phase) {
  switch (p) {
    case "loading":
      return "hide";
    case "fading":
      return "anim";
    default:
      return "";
  }
}

function applyPause() {
  const paused = userPaused.value || fullscreenPaused;
  const currentId = layers.value[layers.value.length - 1]?.id;
  for (const [id, el] of elRefs) {
    if (!(el instanceof HTMLVideoElement)) continue;
    if (paused) {
      el.pause();
    } else if (id === currentId) {
      // 只主动播当前层；过渡中的旧层保持原状（播或冻由它进入过渡前的
      // 状态决定，收尾时统一 pause+移除）
      el.play().catch(() => {});
    }
  }
}

// 向后端上报页面真实布局（视口/当前元素矩形/固有分辨率），用于诊断铺满问题
async function reportState() {
  await nextTick();
  const last = layers.value[layers.value.length - 1];
  const el = last ? elRefs.get(last.id) : undefined;
  let rect = "none";
  let intrinsic = "none";
  if (el instanceof HTMLVideoElement || el instanceof HTMLImageElement) {
    const r = el.getBoundingClientRect();
    rect = `${Math.round(r.left)},${Math.round(r.top)} ${Math.round(r.width)}x${Math.round(r.height)}`;
    intrinsic =
      el instanceof HTMLVideoElement
        ? `${el.videoWidth}x${el.videoHeight}`
        : `${el.naturalWidth}x${el.naturalHeight}`;
  }
  win
    .emit("wallpaper:debug", {
      label,
      viewport: `${window.innerWidth}x${window.innerHeight}@${window.devicePixelRatio}`,
      videoRect: rect,
      intrinsic,
      mode: last ? last.kind : "empty",
      layers: layers.value.map((l) => l.phase).join(","),
    })
    .catch(() => {});
}

onMounted(async () => {
  reportState();
  // 拉取本屏的媒体分配：窗口是懒创建的，emit 可能早于本页监听注册而丢失，
  // 就绪后主动拉一次（含全屏暂停状态补齐）
  try {
    const media = await invoke<{
      path: string;
      kind: string;
      fullscreenPaused: boolean;
      userPaused: boolean;
    } | null>("get_wallpaper_media", { label });
    if (media) {
      userPaused.value = media.userPaused;
      fullscreenPaused = media.fullscreenPaused;
      applyMedia(media.path, media.kind === "image" ? "image" : "video");
    }
  } catch {
    // 拉取失败保持空白，后续事件仍可正常驱动
  }
  // 用窗口级监听：只接收发给本窗口（label 匹配）的事件，
  // 全局 listen 会把 emit_to(其他屏) 的事件也收进来，导致全屏误清/误设
  await win.listen<string>("wallpaper:set-video", (event) => {
    applyMedia(event.payload, "video");
  });
  await win.listen<string>("wallpaper:set-image", (event) => {
    applyMedia(event.payload, "image");
  });
  await win.listen("wallpaper:clear", () => {
    clearMedia();
  });
  // 铺满检测：true = 暂停在当前帧（不隐藏窗口），false = 恢复播放；图片无需处理
  await win.listen<boolean>("wallpaper:fullscreen", (event) => {
    fullscreenPaused = event.payload;
    applyPause();
  });
  // 用户手动暂停（右键菜单/托盘），与铺满暂停叠加
  await win.listen<boolean>("wallpaper:user-paused", (event) => {
    userPaused.value = event.payload;
    applyPause();
  });
});

onBeforeUnmount(() => {
  clearTimeout(settleTimer);
});
</script>

<template>
  <!-- 桌面壁纸窗口：完全禁用右键菜单 -->
  <div class="wallpaper" @contextmenu.prevent>
    <template v-for="l in layers" :key="l.id">
      <!-- 单元素 + 原生 loop：循环由浏览器 media pipeline 内部回卷 demuxer 完成，
           不经过 JS 的 currentTime 赋值，因此没有「seek 清帧 → 不绘制」的窗口。
           preload=auto 让整个文件进缓冲，loop 回卷就是纯内存操作，不再走网络源 -->
      <video
        v-if="l.kind === 'video'"
        :ref="(el) => setLayerEl(l.id, el)"
        class="layer"
        :class="phaseClass(l.phase)"
        :src="l.src"
        muted
        loop
        autoplay
        playsinline
        preload="auto"
        @error="onLayerError(l.id)"
      ></video>
      <img
        v-else
        :ref="(el) => setLayerEl(l.id, el)"
        class="layer"
        :class="phaseClass(l.phase)"
        :src="l.src"
        alt=""
        draggable="false"
        @error="onLayerError(l.id)"
      />
    </template>
  </div>
</template>

<style>
* {
  margin: 0;
  padding: 0;
}

/* position: fixed 直接锚定视口，不依赖 html/body/#app 的百分比高度链 */
.wallpaper {
  position: fixed;
  inset: 0;
  overflow: hidden;
  background: #000;
}

/* 媒体层：铺满视口。稳态层（settled）不带任何附加类，video 停留在
   WebView2 硬件叠加直出路径上；下面两个过渡类只作用于淡入中的新元素，
   收敛后类全部移除即回归直出。旧元素全程零类零改动（防抖动，见文件头） */
.layer {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  object-fit: cover;
}

/* 交叉淡化（等价实现）：不透明旧层上盖淡入的新层。.hide = 加载期隐藏；
   .anim = 过渡期（时长与 FADE_MS 保持一致） */
.hide {
  opacity: 0;
}

.anim {
  transition: opacity 0.3s ease;
}
</style>
