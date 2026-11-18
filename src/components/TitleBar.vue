<script setup lang="ts">
/// 无边框窗口的自定义标题条：左侧模式 chip（单个循环 / 定时切换，点击跳
/// 定时页）、中间整段留白 = 拖拽区（Tauri 内置脚本已支持双击拖拽区切换最大
/// 化，勿再挂 @dblclick，否则一次双击 toggle 两次互相抵消），右上角最小化 /
/// 最大化 / 关闭。
/// data-tauri-drag-region 只认事件目标自身，所以拖拽区做成独立留白段，
/// 与窗口按钮互为兄弟节点，按钮点击不会被拖拽吞掉。
import { computed } from "vue";
import { appWindow, maximized, toggleMax } from "../window";
import { store } from "../store";

const timedActive = computed(() => store.timedLabels.length > 0);
const goTimed = () => {
  store.view = "timed";
};
</script>

<template>
  <header class="chrome">
    <button
      class="mode-chip"
      :class="{ active: timedActive }"
      :title="timedActive ? '部分屏幕处于定时切换模式，点击调整' : '当前为单个循环模式，点击配置定时切换'"
      @click="goTimed"
    >
      <span class="dot"></span>
      {{ timedActive ? "定时切换" : "单个循环" }}
    </button>
    <div class="chrome-drag" data-tauri-drag-region></div>
    <div class="chrome-controls">
      <button
        class="win-btn"
        aria-label="最小化"
        @click="appWindow?.minimize()"
      >
        <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
          <path d="M0 5h10" stroke="currentColor" stroke-width="1.25" />
        </svg>
      </button>
      <button
        class="win-btn"
        :aria-label="maximized ? '还原' : '最大化'"
        :title="maximized ? '还原' : '最大化'"
        @click="toggleMax"
      >
        <svg
          v-if="!maximized"
          width="10"
          height="10"
          viewBox="0 0 10 10"
          fill="none"
        >
          <rect
            x="1.5"
            y="1.5"
            width="7"
            height="7"
            stroke="currentColor"
            stroke-width="1.25"
          />
        </svg>
        <svg v-else width="10" height="10" viewBox="0 0 10 10" fill="none">
          <path
            d="M1 3h6v6H1z M3 3V1h6v6H7"
            stroke="currentColor"
            stroke-width="1.25"
          />
        </svg>
      </button>
      <button
        class="win-btn close"
        aria-label="关闭"
        @click="appWindow?.close()"
      >
        <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
          <path
            d="M0 0l10 10M10 0L0 10"
            stroke="currentColor"
            stroke-width="1.25"
          />
        </svg>
      </button>
    </div>
  </header>
</template>

<style scoped>
.chrome {
  flex: 0 0 44px;
  display: flex;
  align-items: center;
  user-select: none;
}

/* 模式 chip：标题条左侧的当前模式指示，点击直达定时切换页 */
.mode-chip {
  flex: none;
  margin-left: 14px;
  padding: 3px 10px;
  border-radius: 999px;
  border: 1px solid #33363f;
  background: #1b1e26;
  color: #9aa0ad;
  font-size: 11px;
  line-height: 1;
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  transition: color 0.12s, border-color 0.12s, background 0.12s;
}

.mode-chip:hover {
  color: #dfe2ea;
  border-color: #454a57;
  background: #21242c;
}

.mode-chip .dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: #6f7480;
}

.mode-chip.active {
  color: #8fb0ff;
  border-color: #2f5fe0;
  background: rgba(59, 110, 245, 0.08);
}

.mode-chip.active .dot {
  background: #3b6ef5;
}

.chrome-drag {
  flex: 1;
  align-self: stretch;
}

.chrome-controls {
  display: flex;
  align-items: center;
  gap: 6px;
  padding-right: 8px;
}

.win-btn {
  width: 28px;
  height: 28px;
  /* 重置全局 button 的 padding：否则内容盒为 0，图标作为 flex 子项被压没 */
  padding: 0;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: #9aa0ad;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: background 0.12s, color 0.12s;
}

.win-btn svg {
  flex: none;
}

.win-btn:hover {
  background: rgba(255, 255, 255, 0.08);
  color: #dfe2ea;
}

.win-btn.close:hover {
  background: #da3633;
  color: #fff;
}
</style>
