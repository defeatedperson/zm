<script setup lang="ts">
/// 拖放导入遮罩：文件拖入主窗口时全屏高亮提示「松开导入」。
/// 显示状态由 store.dragOver 驱动（onDragDropEvent 的 enter/leave 控制），
/// pointer-events: none——不吞指针事件，拖放的落地由原生层交给 wry 处理。
import { store } from "../store";
</script>

<template>
  <Transition name="drop-fade">
    <div v-if="store.dragOver" class="drop-overlay">
      <div class="drop-card">
        <svg
          class="drop-icon"
          width="40"
          height="40"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.6"
          stroke-linecap="round"
          stroke-linejoin="round"
        >
          <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
          <polyline points="7 9 12 14 17 9" />
          <line x1="12" y1="14" x2="12" y2="2" />
        </svg>
        <p class="drop-title">松开导入 {{ store.dragCount }} 个素材</p>
        <p class="drop-sub">支持视频（mp4 / webm / m4v）与图片</p>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.drop-overlay {
  position: fixed;
  inset: 0;
  z-index: 110;
  background: rgba(16, 17, 22, 0.55);
  backdrop-filter: blur(2px);
  display: flex;
  align-items: center;
  justify-content: center;
  pointer-events: none;
}

.drop-card {
  padding: 36px 56px;
  border: 2px dashed #3b6ef5;
  border-radius: 16px;
  background: rgba(27, 29, 36, 0.9);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
}

.drop-icon {
  color: #6f9bff;
}

.drop-title {
  font-size: 16px;
  font-weight: 600;
  color: #dfe2ea;
}

.drop-sub {
  font-size: 12px;
  color: #8a8f9c;
}

.drop-fade-enter-active,
.drop-fade-leave-active {
  transition: opacity 0.12s ease;
}

.drop-fade-enter-from,
.drop-fade-leave-to {
  opacity: 0;
}
</style>
