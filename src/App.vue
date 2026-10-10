<script setup lang="ts">
/// 应用壳层：头部导航、视图切换（画廊/设置）、底部状态栏、全局右键菜单。
/// 业务状态与操作在 store.ts，页面内容在 GalleryView / SettingsView。
import { onBeforeUnmount, onMounted, ref } from "vue";
import { initApp, pickAndAdd, setStatus, store, toggleUserPaused } from "./store";
import GalleryView from "./GalleryView.vue";
import SettingsView from "./SettingsView.vue";

onMounted(() => {
  void initApp();
  document.addEventListener("contextmenu", onGlobalContextMenu);
});

onBeforeUnmount(() => {
  document.removeEventListener("contextmenu", onGlobalContextMenu);
});

// ---- 全局右键菜单：仅 刷新 + 复制 ----
// 输入框内保留系统原生菜单（粘贴等）；卡片等已有自定义菜单的场景自动跳过
const ctxMenu = ref<{ x: number; y: number; canCopy: boolean; text: string } | null>(
  null,
);

function onGlobalContextMenu(e: MouseEvent) {
  if (e.defaultPrevented) return;
  const target = e.target as HTMLElement | null;
  if (target?.closest("input, textarea, [contenteditable]")) return;
  e.preventDefault();
  const text = window.getSelection()?.toString() ?? "";
  ctxMenu.value = {
    x: Math.min(e.clientX, window.innerWidth - 170),
    y: Math.min(e.clientY, window.innerHeight - 110),
    canCopy: text.length > 0,
    text,
  };
}

function closeCtxMenu() {
  ctxMenu.value = null;
}

async function copySelection() {
  const menu = ctxMenu.value;
  closeCtxMenu();
  if (!menu?.canCopy) return;
  try {
    await navigator.clipboard.writeText(menu.text);
    setStatus("已复制到剪贴板");
  } catch (e) {
    setStatus(`复制失败：${e}`, true);
  }
}

function reloadApp() {
  closeCtxMenu();
  window.location.reload();
}
</script>

<template>
  <main class="shell">
    <header class="header" :class="{ constrained: store.view === 'settings' }">
      <div>
        <h1>{{ store.view === "gallery" ? "幻梦桌面" : "设置" }}</h1>
        <p class="subtitle">
          {{ store.view === "gallery" ? "点击封面应用到全部屏幕，右键卡片可指定单屏" : "存储位置与偏好设置" }}
        </p>
      </div>
      <div class="actions">
        <template v-if="store.view === 'gallery'">
          <button class="primary" :disabled="store.busy" @click="pickAndAdd">添加素材</button>
          <button :disabled="store.busy" @click="store.view = 'settings'">设置</button>
        </template>
        <button v-else @click="store.view = 'gallery'">返回画廊</button>
      </div>
    </header>

    <Transition name="view" mode="out-in">
      <GalleryView v-if="store.view === 'gallery'" />
      <SettingsView v-else />
    </Transition>

    <footer class="status" :class="{ error: store.statusError }">{{ store.status }}</footer>

    <!-- 导入进度弹窗：导入期间不可关闭，结束后自动消失 -->
    <div v-if="store.importing" class="import-mask">
      <div class="import-dialog">
        <h3>正在导入素材</h3>
        <p class="import-name" :title="store.importName">{{ store.importName }}</p>
        <div class="import-track">
          <div class="import-bar" :style="{ width: `${store.importPercent}%` }" />
        </div>
        <p class="import-note">
          {{ store.importPercent }}% · 第 {{ store.importCurrent }} / {{ store.importTotal }} 个文件
        </p>
      </div>
    </div>

    <!-- 全局右键菜单：暂停 / 刷新 / 复制 -->
    <div v-if="ctxMenu" class="ctx-overlay" @click.stop="closeCtxMenu" />
    <div
      v-if="ctxMenu"
      class="ctx-menu"
      :style="{ left: `${ctxMenu.x}px`, top: `${ctxMenu.y}px` }"
      @click.stop
    >
      <button
        class="ctx-item"
        :class="{ highlight: store.userPaused }"
        @click="toggleUserPaused(); closeCtxMenu()"
      >
        {{ store.userPaused ? "恢复播放" : "暂停壁纸" }}
      </button>
      <div class="ctx-divider" />
      <button class="ctx-item" @click="reloadApp">刷新</button>
      <button class="ctx-item" :disabled="!ctxMenu.canCopy" @click="copySelection">
        复制
      </button>
    </div>
  </main>
</template>

<style>
:root {
  font-family: "Segoe UI", "Microsoft YaHei", Inter, sans-serif;
  color-scheme: dark;
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
  flex-direction: column;
  background: linear-gradient(180deg, #17181d 0%, #101116 100%);
  color: #e8eaf0;
  padding: 24px 28px 16px;
  overflow: hidden;
}

.header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  flex-shrink: 0;
}

/* 设置页时头部与设置卡片同宽居中 */
.header.constrained {
  width: 100%;
  max-width: 640px;
  margin: 0 auto;
}

.header h1 {
  font-size: 22px;
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

.status {
  margin-top: 12px;
  padding-top: 10px;
  border-top: 1px solid #23262e;
  color: #8a8f9c;
  font-size: 12px;
  flex-shrink: 0;
}

.status.error {
  color: #f07178;
}

/* 全局右键菜单（独立类名，避免与画廊卡片菜单样式互相干扰） */
.ctx-overlay {
  position: fixed;
  inset: 0;
  z-index: 90;
}

.ctx-menu {
  position: fixed;
  z-index: 91;
  min-width: 160px;
  background: #1e2028;
  border: 1px solid #33363f;
  border-radius: 10px;
  padding: 6px;
  box-shadow: 0 10px 32px rgba(0, 0, 0, 0.5);
  display: flex;
  flex-direction: column;
}

.ctx-item {
  border: none;
  background: none;
  text-align: left;
  padding: 8px 12px;
  border-radius: 6px;
  font-size: 13px;
  color: #dfe2ea;
}

.ctx-item:hover:not(:disabled) {
  background: #2a2e38;
}

.ctx-item:disabled {
  opacity: 0.4;
  cursor: default;
}

.ctx-divider {
  height: 1px;
  background: #2b2e37;
  margin: 4px 6px;
}

/* 暂停状态下高亮「恢复播放」入口 */
.ctx-item.highlight {
  color: #6f9bff;
}

/* 导入进度弹窗 */
.import-mask {
  position: fixed;
  inset: 0;
  z-index: 100;
  background: rgba(10, 11, 14, 0.6);
  display: flex;
  align-items: center;
  justify-content: center;
}

.import-dialog {
  width: 420px;
  max-width: calc(100vw - 48px);
  background: #1b1d24;
  border: 1px solid #2b2e37;
  border-radius: 12px;
  padding: 20px 22px;
}

.import-dialog h3 {
  font-size: 15px;
  font-weight: 600;
}

.import-name {
  margin-top: 8px;
  font-size: 13px;
  color: #9aa0ad;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.import-track {
  margin-top: 14px;
  height: 8px;
  border-radius: 999px;
  background: #14161b;
  overflow: hidden;
}

.import-bar {
  height: 100%;
  border-radius: 999px;
  background: #3b6ef5;
  transition: width 0.15s ease;
}

.import-note {
  margin-top: 10px;
  font-size: 11px;
  color: #6f7480;
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
