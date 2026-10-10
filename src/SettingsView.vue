<script setup lang="ts">
/// 设置页：存储位置、开机自启、关于（后续设置项继续往这里加区块）。
import { onMounted, onUnmounted, ref } from "vue";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { ask, open } from "@tauri-apps/plugin-dialog";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  loadStorageDir,
  openDirInExplorer,
  refresh,
  setStatus,
  store,
} from "./store";
import {
  deleteDefaultDirs,
  getBackupDir,
  getSettings,
  migrateStorage,
  openUrlInBrowser,
  setFullscreenPause,
  type SettingsView,
} from "./settings";

const autoStart = ref(false);
const fullscreenPause = ref(true);
const backupDir = ref("");
const info = ref<SettingsView | null>(null);
const migrating = ref(false);
const migratePercent = ref(0);
let unlistenProgress: UnlistenFn | null = null;

const WEBSITE_URL = "https://zm.xmpanel.cn";

async function openWebsite() {
  try {
    await openUrlInBrowser(WEBSITE_URL);
  } catch (e) {
    setStatus(`打开官网失败：${e}`, true);
  }
}

async function loadInfo() {
  try {
    info.value = await getSettings();
  } catch {
    // 保持上次结果
  }
}

onMounted(async () => {
  autoStart.value = await isEnabled().catch(() => false);
  await loadInfo();
  if (info.value) {
    fullscreenPause.value = info.value.fullscreenPause;
  }
  try {
    backupDir.value = await getBackupDir();
  } catch {
    // 打开时会再次尝试创建
  }
  unlistenProgress = await listen<{ copied: number; total: number }>(
    "migrate:progress",
    (event) => {
      const { copied, total } = event.payload;
      migratePercent.value =
        total > 0 ? Math.min(100, Math.floor((copied / total) * 100)) : 0;
    },
  );
});

onUnmounted(() => {
  unlistenProgress?.();
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

/** 更改存储位置：确认 → 选目录 → 迁移（进度条）→ 刷新画廊与设置 */
async function pickAndMigrate() {
  if (migrating.value) return;
  const confirmed = await ask(
    "更改存储位置会整体迁移视频与备份目录：\n" +
      "· 需要先关闭所有屏幕的动态壁纸\n" +
      "· 迁移期间会临时占用额外磁盘空间\n" +
      "· 素材较多时耗时较长，过程中请勿退出应用\n\n确定继续吗？",
    { title: "更改存储位置", kind: "warning" },
  );
  if (!confirmed) return;
  const dir = await open({ directory: true, title: "选择新的存储位置" });
  if (typeof dir !== "string" || !dir) return;

  migrating.value = true;
  migratePercent.value = 0;
  try {
    const res = await migrateStorage(dir);
    setStatus(
      res.warning ? `存储位置迁移完成。注意：${res.warning}` : "存储位置迁移完成",
      Boolean(res.warning),
    );
  } catch (e) {
    setStatus(`迁移失败：${e}`, true);
  } finally {
    migrating.value = false;
    await Promise.all([refresh(), loadStorageDir(), loadInfo()]);
    try {
      backupDir.value = await getBackupDir();
    } catch {
      // 忽略
    }
  }
}

/** 一键清理默认位置残留的 wallpapers 与 backup 目录 */
async function cleanDefaultDirs() {
  const confirmed = await ask(
    "将删除默认位置下的 wallpapers 与 backup 文件夹（不影响当前存储位置），确定？",
    { title: "清理默认目录", kind: "warning" },
  );
  if (!confirmed) return;
  try {
    await deleteDefaultDirs();
    setStatus("默认目录已清理");
  } catch (e) {
    setStatus(`清理失败：${e}`, true);
  }
  await loadInfo();
}
</script>

<template>
  <section class="settings-page">
    <div class="settings-block">
      <h3>存储位置</h3>
      <p class="settings-path" :title="store.storageDir">{{ store.storageDir }}</p>
      <p class="settings-note">
        {{ info?.customStorageActive ? "当前使用自定义位置" : "当前使用默认位置" }}。更改位置会整体迁移视频与备份目录，需先关闭所有屏幕的动态壁纸。
      </p>
      <div class="settings-actions">
        <button :disabled="migrating" @click="openDirInExplorer(store.storageDir)">
          打开视频目录
        </button>
        <button :disabled="migrating" :title="backupDir" @click="openDirInExplorer(backupDir)">
          打开备份目录
        </button>
        <button :disabled="migrating" @click="pickAndMigrate">更改存储位置…</button>
      </div>
      <div v-if="migrating" class="migrate-progress">
        <div class="migrate-bar">
          <div class="migrate-bar-fill" :style="{ width: migratePercent + '%' }" />
        </div>
        <p class="settings-note">正在迁移 {{ migratePercent }}%，过程中请勿退出应用…</p>
      </div>
      <template v-if="info?.customStorageActive && info.defaultDirsExist">
        <p class="settings-note">
          默认位置仍保留迁移前的旧数据，可打开查看或一键清理：
          <span class="mono">{{ info.defaultStorageRoot }}</span>
        </p>
        <div class="settings-actions">
          <button :disabled="migrating" @click="openDirInExplorer(info.defaultStorageRoot)">
            打开默认目录
          </button>
          <button :disabled="migrating" class="danger" @click="cleanDefaultDirs">
            一键清理默认目录
          </button>
        </div>
      </template>
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

.settings-actions .danger {
  color: #ff7b72;
  border-color: #5a2f2f;
}

.migrate-progress {
  margin-top: 12px;
}

.migrate-bar {
  height: 8px;
  border-radius: 999px;
  background: #14161b;
  border: 1px solid #262932;
  overflow: hidden;
}

.migrate-bar-fill {
  height: 100%;
  border-radius: 999px;
  background: #3b6ef5;
  transition: width 0.15s;
}

.mono {
  font-family: Consolas, monospace;
  word-break: break-all;
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
