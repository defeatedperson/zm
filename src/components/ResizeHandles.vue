<script setup lang="ts">
/// 8 向透明缩放热区：无边框窗口没有系统边缘缩放，mousedown 手动发起。
/// 最大化时整体不渲染（系统不接受最大化窗口的边缘缩放）。
import { maximized, RESIZE_EDGES, startResize } from "../window";
</script>

<template>
  <template v-if="!maximized">
    <div
      v-for="edge in RESIZE_EDGES"
      :key="edge.dir"
      class="rs"
      :class="edge.cls"
      @mousedown.prevent="startResize(edge.dir)"
    />
  </template>
</template>

<style scoped>
.rs {
  position: fixed;
  z-index: 60;
}

.rs-n {
  top: 0;
  left: 10px;
  right: 10px;
  height: 5px;
  cursor: ns-resize;
}

.rs-s {
  bottom: 0;
  left: 10px;
  right: 10px;
  height: 5px;
  cursor: ns-resize;
}

.rs-e {
  right: 0;
  top: 10px;
  bottom: 10px;
  width: 5px;
  cursor: ew-resize;
}

.rs-w {
  left: 0;
  top: 10px;
  bottom: 10px;
  width: 5px;
  cursor: ew-resize;
}

.rs-ne,
.rs-nw {
  top: 0;
  width: 10px;
  height: 10px;
  z-index: 61;
}

.rs-ne {
  right: 0;
  cursor: nesw-resize;
}

.rs-nw {
  left: 0;
  cursor: nesw-resize;
}

.rs-se,
.rs-sw {
  bottom: 0;
  width: 10px;
  height: 10px;
  z-index: 61;
}

.rs-se {
  right: 0;
  cursor: nwse-resize;
}

.rs-sw {
  left: 0;
  cursor: nesw-resize;
}
</style>
