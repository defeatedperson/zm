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

// ---- 循环：单元素原生 loop ----
// 循环完全交给浏览器。Chromium 的 loop 是在 media pipeline 内部回卷 demuxer，
// 不经过 HTMLMediaElement 的 seek 路径：video layer 与 frame submitter 全程
// 存活、readyState 不掉档、帧连续提交，所以循环点天然无缝，不需要任何 JS 介入。
//
// 反面教材（曾经踩过的坑）：A/B 双 <video> 交替 + 上层交叉淡化。
// JS 给 currentTime 赋值会走 SeekMediaTo()，把 readyState 打回 HAVE_METADATA；
// 而 HTMLVideoElement 的绘制入口在 readyState < HAVE_CURRENT_DATA 时直接
// 什么都不画，于是露出容器黑底。再叠上 opacity 过渡把 video 踢出硬件叠加层，
// 循环点必然出现 1~3 帧黑闪。这是结构性的，靠调时序消不掉。
const videoEl = ref<HTMLVideoElement | null>(null);

function applyPause() {
  const el = videoEl.value;
  if (!el) return;
  // 实际暂停 = 用户暂停 OR 铺满暂停，二者独立叠加
  if (userPaused.value || fullscreenPaused) {
    el.pause();
    return;
  }
  el.play().catch(() => {});
}

// 向后端上报页面真实布局（视口/媒体元素矩形/固有分辨率），用于诊断铺满问题
async function reportState() {
  await nextTick();
  const v = videoEl.value;
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
  await nextTick();
  if (!url) {
    // v-if 卸载后 videoEl 自动为 null，这里只需补一次布局上报
    reportState();
    return;
  }
  // 新视频已挂载：按当前暂停状态起播，autoplay 只负责首次加载
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
    <!-- 单元素 + 原生 loop：循环由浏览器 media pipeline 内部回卷 demuxer 完成，
         不经过 JS 的 currentTime 赋值，因此没有「seek 清帧 → 不绘制」的窗口。
         preload=auto 让整个文件进缓冲，loop 回卷就是纯内存操作，不再走网络源 -->
    <video
      v-if="videoSrc"
      ref="videoEl"
      class="video"
      :src="videoSrc"
      muted
      loop
      autoplay
      playsinline
      preload="auto"
      @loadedmetadata="reportState"
    ></video>
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

/* 单层视频直接铺满。不要在这里加任何 opacity / transition：
   那会让 video 掉出硬件叠加层直出路径，白白增加合成开销 */
.video {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  object-fit: cover;
}
</style>
