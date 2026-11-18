<script setup lang="ts">
/// 定时切换页：规则编辑 + timed.json 持久化。保存后由后端调度引擎立即生效
/// （无状态幂等判定：失效素材自动跳过、恢复后自动继续，启用定时的屏幕
/// 不可手动应用/清除——后端守卫兜底）。
///
/// 交互模型：所有屏幕的规则在一个工作副本上编辑，「保存规则」整份提交；
/// 素材用文件名标识（迁移存储位置不受影响）；hour 字段在间隔模式下保留
/// 但不显示不校验（两种模式来回切换不丢已配置的时刻）。
import { computed, onMounted, reactive, ref, watch } from "vue";
import {
  getTimedRules,
  INTERVAL_OPTIONS,
  setTimedRules,
  type ScreenRule,
  type TimedFile,
  type TimedItem,
} from "./timed";
import { refresh, screenShortName, setStatus, store, updateTimedState } from "./store";
import SettingsBlock from "./components/settings/SettingsBlock.vue";

const HOUR_OPTIONS = Array.from({ length: 24 }, (_, h) => h);

/** 工作副本：连接中的屏幕（播种默认规则）+ 文件里有规则但未连接的屏幕 */
const rules = reactive<Record<string, ScreenRule>>({});
/** 加载时文件里已有的键：保存时无条件保留（停用但留配置的规则不丢）。
 *  必须是 ref：payload/dirty 依赖它，普通变量重新赋值不会触发重算 */
const originalKeys = ref(new Set<string>());
const loaded = ref(false);
/** 规则文件读取失败（损坏等）：展示重试 / 重置入口而不是永久卡在加载中 */
const loadFailed = ref(false);
const saving = ref(false);
/** 最近一次保存（或加载）的载荷快照，用于脏判定（同上必须是 ref） */
const lastSaved = ref("");

const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v));
const defaultRule = (): ScreenRule => ({
  enabled: false,
  mode: 0,
  intervalMin: 30,
  items: [],
});

const connectedLabels = computed(() => store.screens.map((s) => s.label));
const disconnectedLabels = computed(() =>
  Object.keys(rules).filter((l) => !connectedLabels.value.includes(l)),
);
/** 渲染顺序：连接中的屏在前（按屏幕顺序），未连接的规则屏随后 */
const orderedLabels = computed(() => [
  ...connectedLabels.value.filter((l) => l in rules),
  ...disconnectedLabels.value,
]);

const isConnected = (label: string) => connectedLabels.value.includes(label);

const screenTitle = (label: string) => {
  const s = store.screens.find((x) => x.label === label);
  return s ? `${screenShortName(s.name)} · ${s.width}×${s.height}` : "未连接的屏幕";
};

/** 未连接屏的卡片摘要文案 */
const ruleSummary = (rule: ScreenRule) =>
  rule.items.length === 0
    ? "未配置素材"
    : rule.mode === 0
      ? `整点时刻表 · ${rule.items.length} 个素材`
      : `每 ${rule.intervalMin} 分钟轮换 · ${rule.items.length} 个素材`;

/** 保存载荷：过滤「从未配置过」的屏幕（禁用 + 空列表且文件里本来就没有） */
const payload = computed<TimedFile>(() => {
  const screens: Record<string, ScreenRule> = {};
  for (const label of orderedLabels.value) {
    const r = rules[label];
    if (r.enabled || r.items.length > 0 || originalKeys.value.has(label)) {
      screens[label] = clone(r);
    }
  }
  return { version: 1, screens };
});

const dirty = computed(
  () => loaded.value && JSON.stringify(payload.value) !== lastSaved.value,
);

const entryByName = (name: string) => store.entries.find((e) => e.name === name);

/** 素材下拉可选项：排除已被**其他行**占用的、带关键帧风险的（挂上会冻结）。
 *  self = 当前行：自身当前值始终保留在选项里——否则选中的那项会被自己
 *  的"已占用"过滤掉，下拉框匹配不到 option 而显示空白 */
const materialOptions = (label: string, self?: TimedItem) => {
  const used = new Set(
    (rules[label] ?? { items: [] as TimedItem[] })
      .items.filter((i) => i !== self)
      .map((i) => i.name),
  );
  return store.entries.filter(
    (e) => e.name === self?.name || (!e.meta?.kfRisk && !used.has(e.name)),
  );
};

/** 热插拔：页面停留期间新接入的屏幕补一张默认卡片——不播种的话
 *  orderedLabels 会把它过滤掉（`l in rules` 不成立），卡片凭空消失 */
watch(connectedLabels, (labels) => {
  if (!loaded.value) return;
  for (const l of labels) {
    if (!(l in rules)) rules[l] = defaultRule();
  }
});

onMounted(() => {
  void loadRules();
});

/** 读取规则文件并播种工作副本（首载与「重试读取」共用） */
async function loadRules() {
  // 直达本页时 store.screens 可能还没就绪（initApp 并行加载中），
  // 空屏会让工作副本漏播种——补一次刷新再读规则
  if (store.screens.length === 0) await refresh();
  try {
    const file = await getTimedRules();
    for (const s of store.screens) {
      rules[s.label] = file.screens[s.label]
        ? clone(file.screens[s.label])
        : defaultRule();
    }
    for (const [label, rule] of Object.entries(file.screens)) {
      if (!(label in rules)) rules[label] = clone(rule);
    }
    originalKeys.value = new Set(Object.keys(file.screens));
    lastSaved.value = JSON.stringify(payload.value);
    loaded.value = true;
    loadFailed.value = false;
  } catch (e) {
    console.error("[timed] 加载规则失败", e);
    setStatus(`加载定时规则失败：${e}`, true);
    loadFailed.value = true;
  }
}

/** timed.json 损坏时的兜底：整份重置为空。两步确认防误触（3 秒内再点一次
 *  才执行）；走 setTimedRules 正常链路——空文件天然通过全部校验 */
const resetArmed = ref(false);
const resetting = ref(false);
let resetArmedTimer: ReturnType<typeof setTimeout> | undefined;

async function resetRules() {
  if (!resetArmed.value) {
    resetArmed.value = true;
    resetArmedTimer = setTimeout(() => (resetArmed.value = false), 3000);
    return;
  }
  resetArmed.value = false;
  clearTimeout(resetArmedTimer);
  resetting.value = true;
  try {
    const empty: TimedFile = { version: 1, screens: {} };
    await setTimedRules(empty);
    updateTimedState(empty);
    for (const s of store.screens) {
      rules[s.label] = defaultRule();
    }
    originalKeys.value = new Set();
    lastSaved.value = JSON.stringify(payload.value);
    loaded.value = true;
    loadFailed.value = false;
    setStatus("定时规则已重置");
  } catch (e) {
    console.error("[timed] 重置规则失败", e);
    setStatus(`重置失败：${e}`, true);
  } finally {
    resetting.value = false;
  }
}

/** 添加素材：自动选中第一个未用的素材；整点模式顺带分配第一个空闲时刻 */
function addItem(label: string) {
  const rule = rules[label];
  const opts = materialOptions(label);
  if (opts.length === 0) {
    setStatus("没有可添加的素材（已全部加入或素材库为空）");
    return;
  }
  const item: TimedItem = { name: opts[0].name };
  if (rule.mode === 0) {
    const used = new Set(rule.items.map((i) => i.hour).filter((h) => h != null));
    const free = HOUR_OPTIONS.find((h) => !used.has(h));
    if (free === undefined) {
      setStatus("24 个时刻都已占用，请先移除部分素材");
      return;
    }
    item.hour = free;
  }
  rule.items.push(item);
}

function removeItem(rule: ScreenRule, idx: number) {
  rule.items.splice(idx, 1);
}

/** 间隔模式的顺序调整（顺序即轮换顺序） */
function moveItem(rule: ScreenRule, idx: number, dir: -1 | 1) {
  const j = idx + dir;
  if (j < 0 || j >= rule.items.length) return;
  const [it] = rule.items.splice(idx, 1);
  rule.items.splice(j, 0, it);
}

/** 切到整点模式时为无时刻的条目自动分配空闲时刻；装不下则拒绝切换 */
function setMode(label: string, mode: 0 | 1) {
  const rule = rules[label];
  if (mode === 0 && rule.mode !== 0) {
    const used = new Set(
      rule.items.map((i) => i.hour).filter((h): h is number => h != null),
    );
    const missing = rule.items.filter((i) => i.hour == null).length;
    if (24 - used.size < missing) {
      setStatus("素材超过 24 个，无法全部分配整点时刻", true);
      return;
    }
    for (const item of rule.items) {
      if (item.hour == null) {
        const free = HOUR_OPTIONS.find((h) => !used.has(h));
        if (free !== undefined) {
          item.hour = free;
          used.add(free);
        }
      }
    }
  }
  rule.mode = mode;
}

/** 删除未连接屏的规则（连接中的屏用关闭开关代替删除） */
function deleteRule(label: string) {
  delete rules[label];
}

// ---- 表单事件（select 的 v-model 拿到的是字符串，这里统一转数字） ----
function pickName(item: TimedItem, ev: Event) {
  item.name = (ev.target as HTMLSelectElement).value;
}
function pickHour(item: TimedItem, ev: Event) {
  item.hour = Number((ev.target as HTMLSelectElement).value);
}
function pickInterval(rule: ScreenRule, ev: Event) {
  rule.intervalMin = Number((ev.target as HTMLSelectElement).value);
}

/** 保存前的友好校验（显示器名提示）；后端还有同规则的白名单兜底 */
function validateClient(): string | null {
  for (const label of orderedLabels.value) {
    const rule = rules[label];
    if (!rule.enabled) continue;
    const title = screenTitle(label);
    if (rule.items.length === 0) {
      return `「${title}」已开启定时但素材列表为空`;
    }
    if (rule.mode === 0) {
      const hours = new Set<number>();
      for (const i of rule.items) {
        if (i.hour == null || i.hour < 0 || i.hour > 23) {
          return `「${title}」有素材未选择切换时刻`;
        }
        if (hours.has(i.hour)) {
          return `「${title}」${String(i.hour).padStart(2, "0")}:00 已有素材，时刻不能重复`;
        }
        hours.add(i.hour);
      }
    } else if (!(INTERVAL_OPTIONS as readonly number[]).includes(rule.intervalMin)) {
      return `「${title}」切换间隔无效`;
    }
    for (const i of rule.items) {
      if (!entryByName(i.name)) {
        return `「${title}」的素材「${i.name}」已不存在（被删除或移动），请先移除`;
      }
    }
  }
  return null;
}

async function save() {
  if (!loaded.value || saving.value) return;
  const err = validateClient();
  if (err) {
    setStatus(err, true);
    return;
  }
  saving.value = true;
  try {
    await setTimedRules(payload.value);
    lastSaved.value = JSON.stringify(payload.value);
    originalKeys.value = new Set(Object.keys(payload.value.screens));
    updateTimedState(payload.value);
    setStatus("定时规则已保存");
  } catch (e) {
    console.error("[timed] 保存规则失败", e);
    setStatus(`保存失败：${e}`, true);
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <section class="timed-page">
    <p v-if="!loaded && !loadFailed" class="hint">正在加载定时规则…</p>

    <!-- 规则文件损坏：坏数据不自动清空，给重试 / 重置（两步确认）出口 -->
    <div v-else-if="loadFailed" class="load-error">
      <p class="hint">
        定时规则文件读取失败（可能已损坏，内容不会被自动清空）。可重试读取，或重置后重新配置：
      </p>
      <div class="row-actions">
        <button :disabled="resetting" @click="loadRules">重试读取</button>
        <button class="danger" :disabled="resetting" @click="resetRules">
          {{ resetArmed ? "确认重置？" : "重置规则" }}
        </button>
      </div>
    </div>

    <template v-else>
      <SettingsBlock v-for="label in orderedLabels" :key="label">
        <div class="rule-head">
          <h3 class="rule-title">
            {{ screenTitle(label) }}
            <span v-if="!isConnected(label)" class="tag">未连接</span>
          </h3>
          <el-switch v-model="rules[label].enabled" />
        </div>

        <!-- 未连接的屏：摘要 + 删除，不提供明细编辑 -->
        <template v-if="!isConnected(label)">
          <p class="hint">
            {{ ruleSummary(rules[label]) }}。屏幕重新连接后自动恢复生效，可回到本页编辑。
          </p>
          <div class="row-actions">
            <button class="danger" @click="deleteRule(label)">删除规则</button>
          </div>
        </template>

        <template v-else>
          <div class="seg">
            <button
              :class="{ active: rules[label].mode === 0 }"
              @click="setMode(label, 0)"
            >
              整点时刻表
            </button>
            <button
              :class="{ active: rules[label].mode === 1 }"
              @click="setMode(label, 1)"
            >
              间隔轮换
            </button>
          </div>

          <div v-if="rules[label].mode === 1" class="interval-row">
            <span class="field-label">切换间隔</span>
            <select
              :value="rules[label].intervalMin"
              @change="pickInterval(rules[label], $event)"
            >
              <option v-for="m in INTERVAL_OPTIONS" :key="m" :value="m">
                每 {{ m }} 分钟
              </option>
            </select>
          </div>

          <p class="hint">
            {{
              rules[label].mode === 0
                ? "每天到达所选整点时切换到对应素材；两个时刻之间显示较早的那个。"
                : "按列表顺序轮换（箭头调整顺序），到点循环往复。"
            }}
          </p>

          <div
            v-for="(item, idx) in rules[label].items"
            :key="idx"
            class="item-row"
          >
            <select class="mat" :value="item.name" @change="pickName(item, $event)">
              <!-- 素材已被删除/改名：保留占位让用户看到并主动移除 -->
              <option v-if="!entryByName(item.name)" :value="item.name">
                {{ item.name }}（已缺失）
              </option>
              <option
                v-for="e in materialOptions(label, item)"
                :key="e.path"
                :value="e.name"
              >
                {{ e.name }}
              </option>
            </select>

            <select
              v-if="rules[label].mode === 0"
              class="hour"
              :value="item.hour ?? ''"
              @change="pickHour(item, $event)"
            >
              <option v-for="h in HOUR_OPTIONS" :key="h" :value="h">
                {{ String(h).padStart(2, "0") }}:00
              </option>
            </select>

            <template v-if="rules[label].mode === 1">
              <button
                class="row-btn"
                title="上移"
                :disabled="idx === 0"
                @click="moveItem(rules[label], idx, -1)"
              >
                ↑
              </button>
              <button
                class="row-btn"
                title="下移"
                :disabled="idx === rules[label].items.length - 1"
                @click="moveItem(rules[label], idx, 1)"
              >
                ↓
              </button>
            </template>
            <button
              class="row-btn danger"
              title="移除"
              @click="removeItem(rules[label], idx)"
            >
              ×
            </button>
          </div>

          <div class="row-actions">
            <button class="add" @click="addItem(label)">＋ 添加素材</button>
          </div>

          <p v-if="store.entries.length === 0" class="hint">
            素材库为空：请先在「桌面壁纸」页导入素材后再配置定时。
          </p>
        </template>
      </SettingsBlock>

      <div v-if="orderedLabels.length === 0" class="empty">
        未检测到显示器，无法配置定时切换。
      </div>

      <div class="save-bar">
        <span class="dirty">{{ dirty ? "有未保存的更改" : "已保存" }}</span>
        <button class="primary" :disabled="!dirty || saving" @click="save">
          {{ saving ? "保存中…" : "保存规则" }}
        </button>
      </div>

      <p class="hint foot-note">
        规则保存后立即生效：由后台调度按整点 / 间隔自动切换；素材失效的时段自动跳过（该屏暂回静态壁纸），恢复后自动继续。
      </p>
    </template>
  </section>
</template>

<style scoped>
.timed-page {
  margin: 18px auto 0;
  flex: 1;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 16px;
  width: 100%;
  max-width: 640px;
}

.rule-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.rule-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  color: #dfe2ea;
  font-weight: 600;
}

.tag {
  font-size: 11px;
  color: #ffb86b;
  background: rgba(255, 184, 107, 0.12);
  border-radius: 4px;
  padding: 1px 6px;
}

.hint {
  margin-top: 8px;
  font-size: 11px;
  color: #6f7480;
}

.load-error {
  margin-top: 12px;
}

/* 模式切换的分段控件 */
.seg {
  display: inline-flex;
  margin-top: 12px;
  border: 1px solid #33363f;
  border-radius: 8px;
  overflow: hidden;
}

.seg button {
  border: none;
  border-radius: 0;
  background: transparent;
  color: #9aa0ad;
  padding: 7px 14px;
}

.seg button + button {
  border-left: 1px solid #33363f;
}

.seg button.active {
  background: #3b6ef5;
  color: #fff;
}

.interval-row {
  margin-top: 12px;
  display: flex;
  align-items: center;
  gap: 10px;
}

.field-label {
  font-size: 12px;
  color: #9aa0ad;
}

/* 原生 select + :root 的 color-scheme: dark，下拉列表即深色，无需 EP 组件 */
select {
  background: #14161b;
  border: 1px solid #33363f;
  color: #dfe2ea;
  border-radius: 6px;
  padding: 7px 9px;
  font: inherit;
  font-size: 12px;
  cursor: pointer;
}

.item-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 8px;
}

.mat {
  flex: 1;
  min-width: 0;
}

.hour {
  width: 88px;
  flex: none;
}

.row-btn {
  width: 26px;
  height: 30px;
  padding: 0;
  border-radius: 6px;
  flex: none;
}

.row-btn.danger:hover {
  color: #ff7b72;
}

.row-actions {
  margin-top: 12px;
  display: flex;
  gap: 10px;
}

button.add {
  color: #8fb0ff;
  border-style: dashed;
}

.danger {
  color: #ff7b72;
  border-color: #5a2f2f;
}

.save-bar {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 14px;
  padding: 4px 2px 0;
}

.dirty {
  font-size: 11px;
  color: #6f7480;
}

.empty {
  color: #6f7480;
  font-size: 13px;
  padding: 24px 0;
  text-align: center;
}

.foot-note {
  text-align: center;
}
</style>
