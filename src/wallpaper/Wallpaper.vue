<script setup lang="ts">
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { nextTick, onMounted, ref, watch } from "vue";

// 每块显示器一个独立壁纸窗口，本窗口只负责把自己铺满
const win = getCurrentWebviewWindow();
const label = win.label;
const videoSrc = ref("");
const imageSrc = ref("");
// 实际暂停 = 用户暂停 OR 铺满暂停，二者独立叠加
const userPaused = ref(false);
let fullscreenPaused = false;

// ---- 循环无缝衔接：A/B 双 <video> 交替 + 上层交叉淡入 ----
// 单元素 loop 重启时浏览器要 seek 回 0 并重新填充解码管线，
// 循环点会出现几帧停顿（素材首尾衔接再好也救不回来）。
//
// 分层策略（防闪黑的关键）：
//   A 永远在最底层且不透明；B 在上层，通过透明度切换显示谁。
//   交换时新活动元素先在「被盖住/透明」状态起播并预热解码，
//   等它真正出帧（playing 事件）后才切换上层透明度做 350ms 淡入淡出。
//   这样任何时刻屏幕上都至少有一层不透明画面——若新元素解码冷启动慢，
//   旧画面会多停留（素材首尾衔接时观感是短暂静止，而不是闪黑）。
const FADE_MS = 350; // 与下方 CSS 的 transition 时长保持一致
const SWAP_AHEAD_S = 0.5; // 活动元素剩余多少秒时起播备用元素（timeupdate 约 250ms 一次）
const elA = ref<HTMLVideoElement | null>(null);
const elB = ref<HTMLVideoElement | null>(null);
// B 在上层：true = B 不透明盖住 A（B 为当前画面），false = B 透明露出 A（A 为当前画面）
const bOpaque = ref(false);
let activeEl: HTMLVideoElement | null = null;
let standbyEl: HTMLVideoElement | null = null;
let parkTimer: number | null = null;

function applyPause() {
  const paused = userPaused.value || fullscreenPaused;
  // 暂停时两个元素都停（交接等待期也可能暂停）；恢复只唤醒活动元素，
  // 备用元素保持归零待命
  if (paused) {
    for (const el of [elA.value, elB.value]) el?.pause();
    return;
  }
  activeEl?.play().catch(() => {});
}

/** 备用元素归位：暂停并回到 0 待命（被盖住/透明状态下进行，无视觉影响） */
function park(el: HTMLVideoElement) {
  el.pause();
  try {
    el.currentTime = 0;
  } catch {
    // 元数据未就绪时个别情况会抛错，归位失败不影响下次交换
  }
}

/** 主备交换：新元素先起播预热，确认出帧后再切换上层透明度 */
function swapActive() {
  if (!activeEl || !standbyEl) return;
  const prev = activeEl;
  const next = standbyEl;
  activeEl = next;
  standbyEl = prev;

  let started = false;
  const reveal = () => {
    // 媒体切换等场景下本回调可能已过期
    if (started || next !== activeEl) return;
    started = true;
    if (parkTimer !== null) window.clearTimeout(parkTimer);
    parkTimer = window.setTimeout(() => park(prev), FADE_MS);
    bOpaque.value = next === elB.value;
  };
  // 已具备起播条件则立即切换，否则等 playing（不设超时兜底：
  // 新元素起播再慢也只是旧画面多停留一会，强切反而会闪黑）
  if (next.readyState >= HTMLMediaElement.HAVE_FUTURE_DATA) {
    reveal();
  } else {
    next.addEventListener("playing", reveal, { once: true });
  }
  next.play().catch(() => {});
}

function onTimeUpdate(e: Event) {
  const el = e.target as HTMLVideoElement;
  if (el !== activeEl || !standbyEl) return;
  if (userPaused.value || fullscreenPaused) return;
  const d = el.duration;
  if (!d || !Number.isFinite(d)) return;
  if (d - el.currentTime <= SWAP_AHEAD_S) swapActive();
}

/** 兜底：timeupdate 未及触发就已播完（理论少见），结尾直接交换 */
function onEnded(e: Event) {
  if ((e.target as HTMLVideoElement) === activeEl) swapActive();
}

// 向后端上报页面真实布局（视口/媒体元素矩形/固有分辨率），用于诊断铺满问题
async function reportState() {
  await nextTick();
  const v = activeEl ?? elA.value;
  let videoRect = "none";
  let intrinsic = "none";
  if (v && videoSrc.value) {
    const r = v.getBoundingClientRect();
    videoRect = `${Math.round(r.left)},${Math.round(r.top)} ${Math.round(r.width)}x${Math.round(r.height)}`;
    intrinsic = `${v.videoWidth}x${v.videoHeight}`;
  }
  win
    .emit("wallpaper:debug", {
      label,
      viewport: `${window.innerWidth}x${window.innerHeight}@${window.devicePixelRatio}`,
      videoRect,
      intrinsic,
      mode: videoSrc.value ? "video" : imageSrc.value ? "image" : "empty",
    })
    .catch(() => {});
}

watch(videoSrc, async (url) => {
  if (parkTimer !== null) {
    window.clearTimeout(parkTimer);
    parkTimer = null;
  }
  await nextTick();
  if (!url) {
    activeEl = null;
    standbyEl = null;
    bOpaque.value = false;
    reportState();
    return;
  }
  // 两个元素都挂载了新视频：A 在底层为活动画面，B 上层透明归零待命
  const a = elA.value;
  const b = elB.value;
  if (!a || !b) return;
  activeEl = a;
  standbyEl = b;
  bOpaque.value = false;
  park(b);
  applyPause();
  reportState();
});

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
      const src = convertFileSrc(media.path);
      if (media.kind === "image") {
        imageSrc.value = src;
      } else {
        videoSrc.value = src;
      }
      reportState();
    }
  } catch {
    // 拉取失败保持空白，后续事件仍可正常驱动
  }
  // 用窗口级监听：只接收发给本窗口（label 匹配）的事件，
  // 全局 listen 会把 emit_to(其他屏) 的事件也收进来，导致全屏误清/误设
  await win.listen<string>("wallpaper:set-video", (event) => {
    imageSrc.value = "";
    videoSrc.value = convertFileSrc(event.payload);
  });
  await win.listen<string>("wallpaper:set-image", (event) => {
    videoSrc.value = "";
    imageSrc.value = convertFileSrc(event.payload);
  });
  await win.listen("wallpaper:clear", () => {
    videoSrc.value = "";
    imageSrc.value = "";
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
</script>

<template>
  <!-- 桌面壁纸窗口：完全禁用右键菜单 -->
  <div class="wallpaper" @contextmenu.prevent>
    <template v-if="videoSrc">
      <!-- A 永远在底层不透明（当前画面或上一画面归零待命）；
           B 在上层，通过透明度决定显示谁，交接时淡入淡出 -->
      <video
        ref="elA"
        class="video"
        :src="videoSrc"
        muted
        playsinline
        preload="auto"
        @timeupdate="onTimeUpdate"
        @ended="onEnded"
        @loadedmetadata="reportState"
      ></video>
      <video
        ref="elB"
        class="video"
        :class="{ under: !bOpaque }"
        :src="videoSrc"
        muted
        playsinline
        preload="auto"
        @timeupdate="onTimeUpdate"
        @ended="onEnded"
        @loadedmetadata="reportState"
      ></video>
    </template>
    <img v-else-if="imageSrc" class="video" :src="imageSrc" alt="" draggable="false" />
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

.video {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  object-fit: cover;
  /* 双视频循环交接淡入淡出时长，需与脚本中的 FADE_MS 保持一致 */
  transition: opacity 0.35s ease;
}

/* 上层 B 待命时透明，露出底层 A */
.video.under {
  opacity: 0;
}
</style>
