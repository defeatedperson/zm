<script setup lang="ts">
/// 导入进度弹窗（从 App.vue 拆出）：导入期间不可关闭，结束后自动消失。
/// 状态完全由 store.importing 驱动，本组件无本地状态。
import { store } from "../store";
</script>

<template>
  <el-dialog
    :model-value="store.importing"
    :show-close="false"
    :close-on-click-modal="false"
    :close-on-press-escape="false"
    width="420px"
    align-center
    class="import-dialog"
    append-to-body
  >
    <h3 class="title">正在导入素材</h3>
    <p class="name" :title="store.importName">{{ store.importName }}</p>
    <el-progress
      class="track"
      :percentage="store.importPercent"
      :show-text="false"
      :stroke-width="8"
    />
    <p class="note">
      {{ store.importPercent }}% · 第 {{ store.importCurrent }} / {{ store.importTotal }} 个文件
    </p>
  </el-dialog>
</template>

<style>
/* 非 scoped：el-dialog 内容 teleport 到 body，scoped 样式作用不到 */
.import-dialog {
  border: 1px solid #2b2e37;
}

.import-dialog .title {
  font-size: 15px;
  font-weight: 600;
  color: #dfe2ea;
}

.import-dialog .name {
  margin-top: 8px;
  font-size: 13px;
  color: #9aa0ad;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.import-dialog .track {
  margin-top: 14px;
}

.import-dialog .note {
  margin-top: 10px;
  font-size: 11px;
  color: #6f7480;
}
</style>
