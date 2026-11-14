<script setup lang="ts">
/// 画廊卡片（从 GalleryView 拆出）：封面、使用中徽标、名称与规格行。
/// 点击 = 应用到全部屏幕；右键 / 右上角 ⋯ = 打开卡片菜单（事件上抛父级统一管理）。
import { computed } from "vue";
import { convertFileSrc } from "@tauri-apps/api/core";
import { store } from "../../store";
import type { GalleryEntry } from "../../gallery";

const props = defineProps<{
  entry: GalleryEntry;
  /** 正在使用该素材的屏幕数量（使用中徽标） */
  usageCount: number;
  /** 全部显示器中的最大物理分辨率（超规格警示用） */
  maxDisplay: { w: number; h: number };
}>();

const emit = defineEmits<{
  apply: [entry: GalleryEntry];
  menu: [entry: GalleryEntry, x: number, y: number];
}>();

/** 是否超过所有显示器的分辨率上限 */
const isOverSpec = computed(
  () =>
    !!props.entry.meta &&
    (props.entry.meta.width > props.maxDisplay.w ||
      props.entry.meta.height > props.maxDisplay.h),
);

/** 帧率显示：30000/1001 → 30，29.97 保留一位小数 */
function fpsText(): string {
  const meta = props.entry.meta;
  if (!meta || meta.fpsNum <= 0) return "";
  const fps = meta.fpsNum / meta.fpsDen;
  const rounded = Math.round(fps);
  return Math.abs(fps - rounded) < 0.05 ? `${rounded}` : fps.toFixed(1);
}

function formatSize(bytes: number): string {
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024).toFixed(0)} KB`;
}

const thumbSrc = computed(() =>
  props.entry.thumbPath ? convertFileSrc(props.entry.thumbPath) : "",
);
</script>

<template>
  <article
    class="card"
    @click="emit('apply', entry)"
    @contextmenu.prevent="emit('menu', entry, $event.clientX, $event.clientY)"
  >
    <div class="thumb">
      <img
        v-if="entry.thumbPath"
        :src="thumbSrc"
        :alt="entry.name"
        loading="lazy"
        decoding="async"
        draggable="false"
      />
      <div v-else-if="store.generating.has(entry.path)" class="placeholder">生成封面中…</div>
      <div v-else class="placeholder">
        {{ store.thumbFailed.has(entry.path) ? "无法生成封面" : "暂无封面" }}
      </div>
      <div v-if="usageCount > 0" class="badge">使用中 ×{{ usageCount }}</div>
    </div>
    <div class="card-body">
      <span class="name" :title="entry.name">{{ entry.name }}</span>
      <span class="meta">
        {{ formatSize(entry.sizeBytes) }}
        <span v-if="entry.kind === 'image'" class="kind-tag">静态</span>
        <template v-else-if="entry.meta">
          <span
            v-if="entry.meta.kfRisk"
            class="kind-tag risk"
            :title="`${entry.meta.kfRisk}，直接播放会在循环时冻结。请右键 → 一键修复`"
          >⚠ 结构异常</span>
          <span
            class="kind-tag"
            :class="{ warn: isOverSpec }"
            :title="isOverSpec ? '分辨率超过显示器上限，建议优化以降低资源占用' : ''"
          >
            {{ entry.meta.width }}×{{ entry.meta.height }}
            <template v-if="fpsText()"> · {{ fpsText() }}fps</template>
          </span>
        </template>
        <span v-else class="kind-tag muted">规格探测中…</span>
      </span>
    </div>
    <button
      class="menu-btn"
      title="更多操作"
      @click.stop="emit('menu', entry, $event.clientX, $event.clientY)"
    >⋯</button>
  </article>
</template>

<style scoped>
.card {
  position: relative;
  background: #1b1d24;
  border: 1px solid #2b2e37;
  border-radius: 10px;
  overflow: hidden;
  cursor: pointer;
  transition: border-color 0.15s;
  /* 长列表优化：跳过离屏卡片的布局与渲染，DOM 保留（可搜索） */
  content-visibility: auto;
  contain-intrinsic-size: auto 320px 232px;
}

.card:hover {
  border-color: #3b6ef5;
}

.thumb {
  position: relative;
  aspect-ratio: 16 / 9;
  background: #101116;
}

.thumb img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}

.placeholder {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #565b66;
  font-size: 12px;
}

.badge {
  position: absolute;
  top: 8px;
  left: 8px;
  background: rgba(59, 110, 245, 0.9);
  color: #fff;
  font-size: 11px;
  padding: 2px 8px;
  border-radius: 999px;
}

.card-body {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 10px 12px 12px;
}

.name {
  font-size: 13px;
  color: #dfe2ea;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.meta {
  font-size: 11px;
  color: #6f7480;
}

.kind-tag {
  margin-left: 4px;
  padding: 1px 6px;
  border-radius: 999px;
  background: #262932;
  color: #9aa0ad;
}

.kind-tag.warn {
  background: rgba(240, 180, 113, 0.14);
  color: #f0b471;
}

.kind-tag.muted {
  opacity: 0.6;
}

/* 结构异常徽标：红底醒目，与“使用中”徽标同级别注意力 */
.kind-tag.risk {
  background: rgba(232, 90, 90, 0.18);
  color: #ff8a8a;
  font-weight: 600;
}

.menu-btn {
  position: absolute;
  top: 8px;
  right: 8px;
  width: 28px;
  height: 28px;
  padding: 0;
  border-radius: 6px;
  background: rgba(16, 17, 22, 0.72);
  border-color: transparent;
  color: #cdd2dd;
  font-size: 15px;
  line-height: 1;
  opacity: 0;
  transition: opacity 0.15s;
}

.card:hover .menu-btn,
.menu-btn:focus {
  opacity: 1;
}
</style>
