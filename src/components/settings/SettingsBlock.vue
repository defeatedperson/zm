<script setup lang="ts">
/// 设置卡片壳（从 SettingsView 拆出）：可选标题 + 内容插槽，统一卡片样式。
defineProps<{
  /** 区块标题；开关行等自带标题的场景可不传 */
  title?: string;
}>();
</script>

<template>
  <div class="settings-block">
    <h3 v-if="title">{{ title }}</h3>
    <slot />
  </div>
</template>

<style scoped>
.settings-block {
  background: #1b1d24;
  border: 1px solid #2b2e37;
  border-radius: 10px;
  padding: 16px 18px;
}

.settings-block h3 {
  font-size: 13px;
  color: #9aa0ad;
  font-weight: 600;
}
</style>
