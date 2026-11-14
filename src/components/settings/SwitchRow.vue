<script setup lang="ts">
/// 设置开关行（从 SettingsView 拆出）：左侧标题+说明，右侧 ElSwitch。
/// v-model 双向绑定；变更由父级持久化（失败时父级负责回显系统实际状态）。
defineProps<{
  title: string;
  desc: string;
  modelValue: boolean;
  /** 开关 title 提示（开启/关闭文案） */
  onText?: string;
  offText?: string;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: boolean];
}>();
</script>

<template>
  <div class="settings-row">
    <div>
      <h3 class="row-title">{{ title }}</h3>
      <p class="row-desc">{{ desc }}</p>
    </div>
    <el-switch
      :model-value="modelValue"
      :title="modelValue ? onText ?? '点击关闭' : offText ?? '点击开启'"
      @update:model-value="emit('update:modelValue', $event as boolean)"
    />
  </div>
</template>

<style scoped>
.settings-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}

.row-title {
  font-size: 13px;
  color: #9aa0ad;
  font-weight: 600;
}

.row-desc {
  margin-top: 8px;
  font-size: 11px;
  color: #6f7480;
}
</style>
