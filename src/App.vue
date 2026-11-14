<script setup lang="ts">
/// 应用壳层（拆分后）：左侧竖条（AppRail）+ 右侧内容区（TitleBar + 页头 + 视图）。
/// 业务状态与操作在 store.ts；窗口操作在 window.ts；
/// 导入弹窗与全局右键菜单拆至 components/，页面内容在 GalleryView / SettingsView。
import { computed, onBeforeUnmount, onMounted } from "vue";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { initWindowState } from "./window";
import { dropFiles, initApp, NAV_ITEMS, pickAndAdd, setDragOver, store } from "./store";
import AppRail from "./components/AppRail.vue";
import TitleBar from "./components/TitleBar.vue";
import ResizeHandles from "./components/ResizeHandles.vue";
import ImportDialog from "./components/ImportDialog.vue";
import GlobalContextMenu from "./components/GlobalContextMenu.vue";
import DragDropOverlay from "./components/DragDropOverlay.vue";
import GalleryView from "./GalleryView.vue";
import SettingsView from "./SettingsView.vue";

const current = computed(
  () => NAV_ITEMS.find((n) => n.id === store.view) ?? NAV_ITEMS[0],
);

// 文件拖放导入：wry 在 WebView2 层拦截拖放并直接给出完整路径数组，
// 与 HTML5 dataTransfer（拿不到本地路径）无关。enter/leave 控制遮罩，drop 落地导入
let unlistenDrag: (() => void) | null = null;

onMounted(async () => {
  void initApp();
  initWindowState();
  try {
    unlistenDrag = await getCurrentWebviewWindow().onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type === "enter") {
        setDragOver(payload.paths);
      } else if (payload.type === "leave") {
        setDragOver(null);
      } else if (payload.type === "drop") {
        setDragOver(null);
        dropFiles(payload.paths);
      }
      // over：仅位置更新，保持 enter 时的遮罩状态
    });
  } catch {
    // 纯浏览器预览（无 Tauri IPC）时不可用，跳过
  }
});

onBeforeUnmount(() => {
  unlistenDrag?.();
  unlistenDrag = null;
});
</script>

<template>
  <main class="shell">
    <AppRail />

    <section class="content">
      <TitleBar />

      <div class="page-head" :class="{ constrained: store.view === 'settings' }">
        <div>
          <h1>{{ current.label }}</h1>
          <p class="subtitle">{{ current.hint }}</p>
        </div>
        <div v-if="store.view === 'gallery'" class="actions">
          <button class="primary" :disabled="store.busy" @click="pickAndAdd">
            添加素材
          </button>
        </div>
      </div>

      <div class="page">
        <Transition name="view" mode="out-in">
          <GalleryView v-if="store.view === 'gallery'" />
          <SettingsView v-else />
        </Transition>
      </div>
    </section>

    <ResizeHandles />

    <!-- 导入进度弹窗：导入期间不可关闭，结束后自动消失 -->
    <ImportDialog />

    <!-- 拖放导入遮罩：文件拖入窗口时提示「松开导入」 -->
    <DragDropOverlay />

    <!-- 全局右键菜单：暂停 / 刷新 / 复制 -->
    <GlobalContextMenu />
  </main>
</template>

<style>
:root {
  font-family: "Segoe UI", "Microsoft YaHei", Inter, sans-serif;
  color-scheme: dark;

  /* Element Plus 主题对齐：主色与现有手写 UI 一致，
     light-N / dark-2 供 EP 组件的 hover / active / disabled 衍生色使用 */
  --el-color-primary: #3b6ef5;
  --el-color-primary-light-3: #6a92f8;
  --el-color-primary-light-5: #8fa9fa;
  --el-color-primary-light-7: #b5c7fc;
  --el-color-primary-light-8: #c9d6fd;
  --el-color-primary-light-9: #dde5fe;
  --el-color-primary-dark-2: #2f5fe0;
  /* 弹窗观感对齐原手写弹窗 */
  --el-dialog-bg-color: #1b1d24;
  --el-dialog-border-radius: 12px;
  /* 进度条轨道 */
  --el-progress-bg-color: #14161b;
}

* {
  margin: 0;
  padding: 0;
  box-sizing: border-box;
}

html,
body,
#app {
  width: 100%;
  height: 100%;
}

/* 全局细滚动条（WebView2 = Chromium 内核） */
::-webkit-scrollbar {
  width: 10px;
  height: 10px;
}

::-webkit-scrollbar-thumb {
  background: #33363f;
  border-radius: 999px;
  border: 2px solid transparent;
  background-clip: padding-box;
}

::-webkit-scrollbar-thumb:hover {
  background: #454a57;
  background-clip: padding-box;
}

::-webkit-scrollbar-track,
::-webkit-scrollbar-corner {
  background: transparent;
}

.shell {
  width: 100%;
  height: 100%;
  display: flex;
  background: #101116;
  color: #e8eaf0;
  overflow: hidden;
}

/* ---- 右侧内容区 ---- */
.content {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  background: linear-gradient(180deg, #17181d 0%, #101116 100%);
}

.page-head {
  flex-shrink: 0;
  padding: 2px 28px 0;
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
}

/* 设置页标题与设置卡片同宽居中；去掉水平 padding，否则标题会比卡片缩进 */
.page-head.constrained {
  width: 100%;
  max-width: 640px;
  margin: 0 auto;
  padding-left: 0;
  padding-right: 0;
}

.page-head h1 {
  font-size: 20px;
  font-weight: 600;
  letter-spacing: 1px;
}

.subtitle {
  margin-top: 4px;
  color: #8a8f9c;
  font-size: 12px;
}

.actions {
  display: flex;
  gap: 10px;
  flex-shrink: 0;
}

.page {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  padding: 0 28px 18px;
  overflow: hidden;
}

button {
  border: 1px solid #33363f;
  background: #21242c;
  color: #dfe2ea;
  border-radius: 8px;
  padding: 9px 18px;
  font-size: 13px;
  font-family: inherit;
  cursor: pointer;
  transition: background 0.15s, border-color 0.15s;
}

button:hover:not(:disabled) {
  background: #2a2e38;
  border-color: #454a57;
}

button:disabled {
  opacity: 0.5;
  cursor: wait;
}

button.primary {
  background: #3b6ef5;
  border-color: #3b6ef5;
  color: #fff;
  font-weight: 600;
}

button.primary:hover:not(:disabled) {
  background: #2f5fe0;
}

/* 视图切换过渡：内容区淡入淡出 + 轻微位移 */
.view-enter-active,
.view-leave-active {
  transition: opacity 0.18s ease, transform 0.18s ease;
}

.view-enter-from {
  opacity: 0;
  transform: translateY(10px);
}

.view-leave-to {
  opacity: 0;
  transform: translateY(-8px);
}

@media (prefers-reduced-motion: reduce) {
  .view-enter-active,
  .view-leave-active {
    transition: none;
  }
}
</style>
