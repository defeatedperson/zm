<script setup lang="ts">
/// 画廊主页（拆分后）：屏幕状态条、封面网格、空态。
/// 卡片/卡片菜单/优化弹窗分别拆至 components/gallery/，此处只做编排。
import { computed, ref } from "vue";
import { applyTo, maxDisplay, pickAndAdd, store } from "./store";
import type { GalleryEntry } from "./gallery";
import ScreenBar from "./components/gallery/ScreenBar.vue";
import GalleryCard from "./components/gallery/GalleryCard.vue";
import CardMenu from "./components/gallery/CardMenu.vue";
import OptimizeDialog from "./components/gallery/OptimizeDialog.vue";

// ---- 卡片菜单（右键 / ⋯ 按钮触发；坐标即弹出的位置） ----
const menu = ref<{ path: string; x: number; y: number } | null>(null);

const menuEntry = computed(
  () => store.entries.find((e) => e.path === menu.value?.path) ?? null,
);

function openMenu(entry: GalleryEntry, x: number, y: number) {
  menu.value = {
    path: entry.path,
    x: Math.min(x, window.innerWidth - 200),
    y: Math.min(y, window.innerHeight - 260),
  };
}

function closeMenu() {
  menu.value = null;
}

/** 路径 -> 使用该壁纸的屏幕数量（卡片「使用中」徽标） */
const usageByPath = computed(() => {
  const map = new Map<string, number>();
  for (const s of store.screens) {
    if (!s.wallpaperPath) continue;
    map.set(s.wallpaperPath, (map.get(s.wallpaperPath) ?? 0) + 1);
  }
  return map;
});

// 优化/修复弹窗：菜单上抛事件后经此 ref 打开
const optimizeDialog = ref<InstanceType<typeof OptimizeDialog>>();

function openOptimize(entry: GalleryEntry, repair: boolean) {
  optimizeDialog.value?.open(entry, repair);
}
</script>

<template>
  <div class="gallery-page">
    <ScreenBar />

    <div v-if="store.entries.length > 0" class="grid-scroll">
      <section class="grid">
        <GalleryCard
          v-for="entry in store.entries"
          :key="entry.path"
          :entry="entry"
          :usage-count="usageByPath.get(entry.path) ?? 0"
          :max-display="maxDisplay"
          @apply="(e) => applyTo(e, null)"
          @menu="openMenu"
        />
      </section>
    </div>

    <section v-else class="empty">
      <p>画廊是空的，先添加一些视频或图片吧</p>
      <button class="primary" :disabled="store.busy" @click="pickAndAdd">添加素材</button>
    </section>

    <!-- 卡片菜单 -->
    <CardMenu
      v-if="menu && menuEntry"
      :entry="menuEntry"
      :x="menu.x"
      :y="menu.y"
      @close="closeMenu"
      @optimize="openOptimize"
    />

    <!-- 优化/修复弹窗 -->
    <OptimizeDialog ref="optimizeDialog" />
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

.empty {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 16px;
  color: #6f7480;
}
</style>
