<script setup lang="ts">
/// 导航图标：Element Plus 图标库（参考项目同款），填充式 currentColor。
/// 新增功能页时在此补一个分支。
import { Clock, Grid, Setting } from "@element-plus/icons-vue";
import type { ViewId } from "../store";

defineProps<{ kind: ViewId }>();
</script>

<template>
  <Grid v-if="kind === 'gallery'" class="nav-ico" />
  <Clock v-else-if="kind === 'timed'" class="nav-ico" />
  <Setting v-else-if="kind === 'settings'" class="nav-ico" />
</template>

<style scoped>
/* 显式尺寸 + 禁缩：按钮内容盒再窄也不把图标压没 */
.nav-ico {
  width: 20px;
  height: 20px;
  flex: none;
}
</style>
