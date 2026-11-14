<script setup lang="ts">
/// 画廊顶部屏幕状态条（从 GalleryView 拆出）：
/// 每块屏一枚胶囊（在播高亮、单屏清除），多屏在播时附「全部清除」。
import { computed } from "vue";
import { clearAll, clearScreen, screenShortName, store } from "../../store";

const playingCount = computed(
  () => store.screens.filter((s) => s.wallpaperPath).length,
);
</script>

<template>
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
</template>

<style scoped>
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
</style>
