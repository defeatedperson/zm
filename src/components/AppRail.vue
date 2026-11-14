<script setup lang="ts">
/// 左侧竖条导航：logo（可拖拽窗口）+ 上部功能页 + 底部固定项（设置）。
/// 导航项来自 store 的 NAV_ITEMS 注册表，按 position 分区渲染。
import { NAV_ITEMS, store } from "../store";
import NavIcon from "./NavIcon.vue";
import logoUrl from "../assets/logo.png";

const topNavs = NAV_ITEMS.filter((n) => n.position === "top");
const bottomNavs = NAV_ITEMS.filter((n) => n.position === "bottom");
</script>

<template>
  <aside class="rail">
    <div class="rail-top">
      <!-- draggable=false：禁掉浏览器对 img 的原生拖拽（否则按住 logo 会把
           图片文件拖出窗口）；data-tauri-drag-region：按住 logo 拖动窗口、
           双击最大化由 Tauri 内置脚本处理（勿再挂 @dblclick，会双触发抵消）。
           该属性只认事件目标自身，所以直接挂在 img 上 -->
      <img
        class="logo"
        :src="logoUrl"
        alt="幻梦桌面"
        draggable="false"
        data-tauri-drag-region
        title="按住拖动窗口"
        @click="store.view = 'gallery'"
      />
      <div class="rail-divider" />
      <nav class="nav">
        <el-tooltip
          v-for="n in topNavs"
          :key="n.id"
          :content="n.label"
          placement="right"
          :show-after="300"
          :hide-after="0"
        >
          <button
            class="nav-btn"
            :class="{ active: store.view === n.id }"
            :aria-label="n.label"
            @click="store.view = n.id"
          >
            <NavIcon :kind="n.id" />
          </button>
        </el-tooltip>
      </nav>
    </div>

    <nav class="nav rail-bottom">
      <el-tooltip
        v-for="n in bottomNavs"
        :key="n.id"
        :content="n.label"
        placement="right"
        :show-after="300"
        :hide-after="0"
      >
        <button
          class="nav-btn"
          :class="{ active: store.view === n.id }"
          :aria-label="n.label"
          @click="store.view = n.id"
        >
          <NavIcon :kind="n.id" />
        </button>
      </el-tooltip>
    </nav>
  </aside>
</template>

<style scoped>
.rail {
  flex: 0 0 64px;
  display: flex;
  flex-direction: column;
  justify-content: space-between;
  align-items: center;
  background: #121318;
  border-right: 1px solid #23262e;
  padding: 14px 0;
  user-select: none;
}

.rail-top {
  display: flex;
  flex-direction: column;
  align-items: center;
}

.logo {
  width: 36px;
  height: 36px;
  border-radius: 9px;
  opacity: 0.82;
  cursor: grab;
  transition: opacity 0.2s;
  /* 双保险防拖出文件：属性 draggable=false 之外再禁 WebKit 原生拖拽 */
  -webkit-user-drag: none;
}

.logo:hover {
  opacity: 1;
}

.rail-divider {
  width: 32px;
  height: 1px;
  background: #23262e;
  margin: 14px 0 10px;
}

.nav {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.nav-btn {
  position: relative;
  width: 40px;
  height: 40px;
  /* 重置全局 button 的 padding：否则内容盒被吃到 4px，图标作为 flex 子项被压没 */
  padding: 0;
  border: none;
  border-radius: 10px;
  background: transparent;
  color: #9aa0ad;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: background 0.15s, color 0.15s;
}

.nav-btn svg {
  flex: none;
}

/* 选中指示条：常驻节点 + scaleY 收起，选中时从中线展开 */
.nav-btn::before {
  content: "";
  position: absolute;
  left: -12px;
  width: 3px;
  height: 20px;
  border-radius: 2px;
  background: #3b6ef5;
  transform: scaleY(0);
  opacity: 0;
  transition: transform 0.22s ease, opacity 0.22s ease;
}

.nav-btn:hover {
  background: rgba(59, 110, 245, 0.1);
  color: #dfe2ea;
}

.nav-btn.active {
  background: rgba(59, 110, 245, 0.15);
  color: #6f9bff;
}

.nav-btn.active::before {
  transform: scaleY(1);
  opacity: 1;
}
</style>
