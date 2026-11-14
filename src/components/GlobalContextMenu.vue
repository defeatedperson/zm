<script setup lang="ts">
/// 全局右键菜单（从 App.vue 拆出）：仅 暂停/恢复、刷新、复制。
/// 输入框内保留系统原生菜单（粘贴等）；已有自定义菜单的场景（卡片右键）
/// 事件已被 preventDefault，自动跳过。
/// 菜单渲染在 body 层，非 scoped 样式 + 独立类名（与画廊卡片菜单互不干扰）。
import { onBeforeUnmount, onMounted, ref } from "vue";
import { setStatus, store, toggleUserPaused } from "../store";

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

onMounted(() => {
  document.addEventListener("contextmenu", onGlobalContextMenu);
});

onBeforeUnmount(() => {
  document.removeEventListener("contextmenu", onGlobalContextMenu);
});
</script>

<template>
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
</template>

<style scoped>
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
</style>
