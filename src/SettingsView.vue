<script setup lang="ts">
/// 设置页：存储位置、开机自启、关于（后续设置项继续往这里加区块）。
import { onMounted, ref } from "vue";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { openDirInExplorer, setStatus, store } from "./store";
import { getBackupDir, getSettings, openUrlInBrowser, setFullscreenPause } from "./settings";

const autoStart = ref(false);
const fullscreenPause = ref(true);
const backupDir = ref("");

const WEBSITE_URL = "https://zm.xcdream.com";

async function openWebsite() {
  try {
    await openUrlInBrowser(WEBSITE_URL);
  } catch (e) {
    setStatus(`打开官网失败：${e}`, true);
  }
}

onMounted(async () => {
  autoStart.value = await isEnabled().catch(() => false);
  try {
    fullscreenPause.value = (await getSettings()).settings.fullscreenPause;
  } catch {
    // 读取失败时保持默认值
  }
  try {
    backupDir.value = await getBackupDir();
  } catch {
    // 打开时会再次尝试创建
  }
});

async function toggleAutoStart() {
  const next = !autoStart.value;
  try {
    if (next) {
      await enable();
    } else {
      await disable();
    }
    autoStart.value = next;
    setStatus(next ? "已开启开机自启" : "已关闭开机自启");
  } catch (e) {
    setStatus(`设置开机自启失败：${e}`, true);
    // 以系统实际状态为准回显
    autoStart.value = await isEnabled().catch(() => false);
  }
}

async function toggleFullscreenPause() {
  const next = !fullscreenPause.value;
  try {
    await setFullscreenPause(next);
    fullscreenPause.value = next;
    setStatus(next ? "全屏时将暂停壁纸" : "全屏时壁纸继续播放");
  } catch (e) {
    setStatus(`设置失败：${e}`, true);
  }
}
</script>

<template>
  <section class="settings-page">
    <div class="settings-block">
      <h3>存储位置</h3>
      <p class="settings-path" :title="store.storageDir">{{ store.storageDir }}</p>
      <p class="settings-note">视频与图片统一保存在应用数据目录，无需手动管理。</p>
      <div class="settings-actions">
        <button @click="openDirInExplorer(store.storageDir)">打开视频目录</button>
        <button @click="openDirInExplorer(backupDir)" :title="backupDir">
          打开备份目录
        </button>
      </div>
    </div>

    <div class="settings-block">
      <div class="settings-row">
        <div>
          <h3>开机自启</h3>
          <p class="settings-note">登录 Windows 后自动启动 幻梦桌面（默认关闭）。</p>
        </div>
        <button
          class="switch"
          :class="{ on: autoStart }"
          role="switch"
          :aria-checked="autoStart"
          :title="autoStart ? '点击关闭开机自启' : '点击开启开机自启'"
          @click="toggleAutoStart"
        >
          <span class="knob" />
        </button>
      </div>
    </div>

    <div class="settings-block">
      <div class="settings-row">
        <div>
          <h3>全屏时暂停壁纸</h3>
          <p class="settings-note">
            某块屏幕被全屏应用（如游戏）占据时，只暂停该屏的壁纸以节省资源，其他屏幕不受影响。
          </p>
        </div>
        <button
          class="switch"
          :class="{ on: fullscreenPause }"
          role="switch"
          :aria-checked="fullscreenPause"
          @click="toggleFullscreenPause"
        >
          <span class="knob" />
        </button>
      </div>
    </div>

    <div class="settings-block">
      <h3>关于</h3>
      <p class="settings-about">
        幻梦桌面 <span class="version">v{{ store.appVersion || "…" }}</span>
      </p>
      <p class="settings-note">Windows 动态壁纸工具。</p>
      <div class="settings-actions">
        <button @click="openWebsite">访问官网</button>
      </div>
      <!-- TODO: 版权信息与许可声明 -->
    </div>
  </section>
</template>

<style scoped>
.settings-page {
  margin: 18px auto 0;
  flex: 1;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 16px;
  width: 100%;
  max-width: 640px;
}

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

.settings-path {
  margin-top: 10px;
  background: #14161b;
  border: 1px solid #262932;
  border-radius: 8px;
  padding: 9px 12px;
  font-size: 12px;
  color: #cdd2dd;
  font-family: Consolas, monospace;
  word-break: break-all;
}

.settings-note {
  margin-top: 8px;
  font-size: 11px;
  color: #6f7480;
}

.settings-actions {
  margin-top: 12px;
  display: flex;
  gap: 10px;
}

.settings-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
}

/* 开关 */
.switch {
  flex-shrink: 0;
  width: 42px;
  height: 23px;
  padding: 2px;
  border-radius: 999px;
  background: #33363f;
  border: 1px solid #3d414b;
  transition: background 0.15s, border-color 0.15s;
}

.switch:hover {
  border-color: #4a4f5a;
}

.switch.on {
  background: #3b6ef5;
  border-color: #3b6ef5;
}

.knob {
  display: block;
  width: 17px;
  height: 17px;
  border-radius: 50%;
  background: #cdd2dd;
  transition: transform 0.15s;
}

.switch.on .knob {
  background: #fff;
  transform: translateX(19px);
}

.settings-about {
  margin-top: 10px;
  font-size: 14px;
  color: #dfe2ea;
}

.version {
  color: #8a8f9c;
  font-size: 12px;
  margin-left: 4px;
}
</style>
