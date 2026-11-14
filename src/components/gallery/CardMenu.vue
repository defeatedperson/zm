<script setup lang="ts">
/// 卡片右键菜单（从 GalleryView 拆出）：应用、单屏应用、重新生成封面、
/// 一键修复、优化、移除（两击确认）。菜单坐标由父级算好传入。
/// 应用/封面/移除直接调用 store；「修复/优化」上抛父级（弹窗状态在 OptimizeDialog）。
import { computed, ref } from "vue";
import {
  applyTo,
  regenThumbnail,
  removeGalleryItem,
  screenShortName,
  store,
} from "../../store";
import type { GalleryEntry } from "../../gallery";

const props = defineProps<{
  entry: GalleryEntry;
  x: number;
  y: number;
}>();

const emit = defineEmits<{
  close: [];
  optimize: [entry: GalleryEntry, repair: boolean];
}>();

/** 正在使用中的素材（优化/移除均禁用） */
const isInUse = computed(() =>
  store.screens.some((s) => s.wallpaperPath === props.entry.path),
);

// 两击确认删除：第一次点击进入待确认状态
const pendingRemove = ref<string | null>(null);

async function removeFromMenu() {
  const entry = props.entry;
  if (pendingRemove.value !== entry.path) {
    pendingRemove.value = entry.path;
    setTimeout(() => {
      if (pendingRemove.value === entry.path) pendingRemove.value = null;
    }, 3000);
    return;
  }
  pendingRemove.value = null;
  emit("close");
  await removeGalleryItem(entry);
}
</script>

<template>
  <div class="menu-overlay" @click.stop="emit('close')" />
  <div class="menu" :style="{ left: `${x}px`, top: `${y}px` }" @click.stop>
    <button class="menu-item" @click="applyTo(entry, null); emit('close')">
      应用到全部屏幕
    </button>
    <div class="menu-divider" />
    <button
      v-for="s in store.screens"
      :key="s.label"
      class="menu-item"
      @click="applyTo(entry, s.label); emit('close')"
    >
      应用到 {{ screenShortName(s.name) }}
      <span v-if="s.wallpaperPath === entry.path" class="menu-check">●</span>
    </button>
    <div class="menu-divider" />
    <button
      v-if="entry.kind === 'video'"
      class="menu-item"
      @click="regenThumbnail(entry.path); emit('close')"
    >
      重新生成封面
    </button>
    <button
      v-if="entry.meta?.kfRisk"
      class="menu-item repair"
      :title="`${entry.meta.kfRisk}。将按原始分辨率与帧率重建编码，修复后可正常应用`"
      @click="emit('optimize', entry, true); emit('close')"
    >
      一键修复
    </button>
    <button
      v-if="entry.kind === 'video'"
      class="menu-item"
      :disabled="isInUse || !entry.meta"
      :title="
        isInUse
          ? '该视频正在使用中，请先在顶部状态条停止对应屏幕（✕）'
          : !entry.meta
            ? '无法读取视频规格'
            : ''
      "
      @click="emit('optimize', entry, false); emit('close')"
    >
      优化
    </button>
    <button
      class="menu-item danger"
      :disabled="isInUse"
      :title="isInUse ? '该视频正在使用中，请先停止对应的屏幕壁纸' : ''"
      @click="removeFromMenu"
    >{{ pendingRemove === entry.path ? "再点一次确认移除" : "移除" }}</button>
  </div>
</template>

<style scoped>
.menu-overlay {
  position: fixed;
  inset: 0;
  z-index: 90;
}

.menu {
  position: fixed;
  z-index: 91;
  min-width: 180px;
  background: #1e2028;
  border: 1px solid #33363f;
  border-radius: 10px;
  padding: 6px;
  box-shadow: 0 10px 32px rgba(0, 0, 0, 0.5);
  display: flex;
  flex-direction: column;
}

.menu-item {
  border: none;
  background: none;
  text-align: left;
  padding: 8px 12px;
  border-radius: 6px;
  font-size: 13px;
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.menu-item:hover:not(:disabled) {
  background: #2a2e38;
}

.menu-item.danger {
  color: #f07178;
}

.menu-item.danger:hover:not(:disabled) {
  background: rgba(240, 113, 120, 0.12);
}

/* 一键修复：强调色引导用户走修复路径 */
.menu-item.repair {
  color: #6fcf97;
  font-weight: 600;
}

.menu-item.repair:hover {
  background: rgba(111, 207, 151, 0.1);
}

.menu-check {
  color: #4ade80;
  font-size: 11px;
}

.menu-divider {
  height: 1px;
  background: #2b2e37;
  margin: 4px 6px;
}
</style>
