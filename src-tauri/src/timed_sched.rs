//! 定时切换调度引擎（D2）：无状态自愈幂等判定。
//!
//! 每 tick 用「当前本地时刻 + timed.json 规则 + 文件系统实况」重算每块启用屏
//! 的应显示素材，与实际播放比对，变化才动作：
//! - 整点时刻表（mode=0）：取 hour ≤ 当前小时的最近条目；全部晚于当前小时
//!   时环绕到最晚一条（昨天设的那个）
//! - 间隔轮换（mode=1）：序号 = floor(零点以来分钟数 / 间隔) % 有效条数，
//!   锚定本地零点
//! - 文件已删除或带关键帧风险的条目在推导时剔除；全部失效 → 卸载该屏并
//!   提示一次（素材恢复后自动重挂，不再提示）
//! - 屏未连接：规则保留，跳过（重新连接后自动跟上当前时段）
//! - 挂载失败：单屏退避冷却，避免每 tick 的重试风暴；期望素材变化立即重试
//!
//! 无状态的意义：睡眠唤醒、规则手改、素材增删、进程重启都不需要对账逻辑，
//! 下一次 tick 自然收敛到正确状态。

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::settings;
use crate::timed::{self, ScreenRule, TimedItem, MODE_HOUR, MODE_INTERVAL};
use crate::wallpaper;

/// 轮询间隔：分钟级切换精度下 2s 绰绰有余，与全屏检测轮询同节奏
const TICK: Duration = Duration::from_secs(2);
/// 挂载失败后的单屏冷却：attach 内部自带 WorkerW 退避重试，连续失败通常是
/// explorer / 桌面层异常，紧贴着重试只会刷日志
const FAIL_COOLDOWN: Duration = Duration::from_secs(15);

/// 失效提示事件载荷（主窗口 toast 展示；label 由前端映射成显示器短名）
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TimedNotice {
    label: String,
    reason: String,
}

/// 挂载失败退避表：label -> (失败时的期望素材, 冷却截止时刻)
fn failures() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    static S: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 当前能推导出应显示素材的屏（健康集合）：仅在「健康 → 全失效」翻转时
/// 提示一次，恢复方向静默自愈（画面重新出现本身就是反馈）
fn healthy_screens() -> &'static Mutex<HashSet<String>> {
    static S: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 启动调度线程（lib.rs setup 挂载；等启动恢复完成后才开始工作）
pub fn start(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(TICK);
        // 启动恢复未完成不介入：恢复流程可能正在建窗挂载，此时比对会误判
        if !wallpaper::restore_done() {
            continue;
        }
        // 存储迁移期间不动作：目录正在整体搬运，任何挂载都可能读到半份数据
        if crate::storage::is_migrating() {
            continue;
        }
        if let Err(e) = tick(&app) {
            println!("[timed-sched] tick 失败: {e}");
        }
    });
}

fn tick(app: &AppHandle) -> Result<(), String> {
    let rules = timed::load_unlocked(app)?;
    if rules.screens.values().all(|r| !r.enabled) {
        return Ok(());
    }
    let connected: HashSet<String> = wallpaper::monitor_labels(app)?.into_iter().collect();
    let dir = settings::resolve_wallpapers_dir(app)?;
    let (hour, minute_of_day) = local_now();
    for (label, rule) in &rules.screens {
        // 非法 mode（手改文件才会出现，保存校验已拦截）：静默跳过，不猜意图
        if rule.mode != MODE_HOUR && rule.mode != MODE_INTERVAL {
            continue;
        }
        if !rule.enabled || !connected.contains(label) {
            continue;
        }
        // 单屏失败不拖垮整轮：该屏进入退避，其余屏照常收敛
        if let Err(e) = converge_screen(app, label, rule, hour, minute_of_day, &dir) {
            println!("[timed-sched] {label} 收敛失败: {e}");
        }
    }
    Ok(())
}

/// 把一块屏收敛到「此刻应显示的素材」
fn converge_screen(
    app: &AppHandle,
    label: &str,
    rule: &ScreenRule,
    hour: u32,
    minute_of_day: u32,
    dir: &Path,
) -> Result<(), String> {
    let expected = derive_current(rule, hour, minute_of_day, dir);
    update_health(app, label, expected.is_some());
    let actual = wallpaper::current_media(label);
    match (&expected, actual) {
        // 已在播应显示的素材：幂等，无事可做
        (Some(p), Some(a)) if p == &a => Ok(()),
        (Some(p), _) => {
            // 退避：同一期望素材冷却期内不重试（期望变化则立即处理）
            if cooldown_active(label, p) {
                return Ok(());
            }
            match wallpaper::sched_apply(app, label, p) {
                Ok(()) => {
                    clear_failure(label);
                    println!("[timed-sched] {label} 已切换: {}", short_name(p));
                    Ok(())
                }
                Err(e) => {
                    record_failure(label, p);
                    Err(e)
                }
            }
        }
        // 全部素材失效：卸载回静态壁纸（规则保留，素材恢复后自动重挂）
        (None, Some(_)) => {
            wallpaper::sched_teardown(app, label);
            clear_failure(label);
            Ok(())
        }
        (None, None) => Ok(()),
    }
}

/// 推导此屏此刻应显示的素材完整路径；None = 无可显示（列表空 / 全失效）
fn derive_current(rule: &ScreenRule, hour: u32, minute_of_day: u32, dir: &Path) -> Option<String> {
    // 有效条目 = 文件存在且无关键帧风险（挂上会冻结的按无效处理）
    let valid: Vec<&TimedItem> = rule
        .items
        .iter()
        .filter(|i| {
            dir.join(&i.name).is_file() && crate::gallery::kf_risk_named(dir, &i.name).is_none()
        })
        .collect();
    let chosen = if rule.mode == MODE_HOUR {
        // 缺 hour 的条目（手改文件才会出现，保存校验已拦截）视为无效
        let with_hour: Vec<&TimedItem> =
            valid.iter().copied().filter(|i| i.hour.is_some()).collect();
        if with_hour.is_empty() {
            return None;
        }
        // hour ≤ 当前小时的最近条目；全部晚于当前小时 → 环绕到最晚一条
        with_hour
            .iter()
            .copied()
            .filter(|i| i.hour.is_some_and(|h| (h as u32) <= hour))
            .max_by_key(|i| i.hour)
            .or_else(|| with_hour.iter().copied().max_by_key(|i| i.hour))?
    } else {
        if valid.is_empty() {
            return None;
        }
        // 间隔轮换：锚定本地零点取序号。interval 保存时已白名单校验，
        // 手改文件的非法值兜底按 1 分钟，防除零
        let interval = rule.interval_min.max(1);
        let idx = (minute_of_day / interval) as usize % valid.len();
        valid[idx]
    };
    Some(dir.join(&chosen.name).to_string_lossy().into_owned())
}

/// 本地时刻 (当前小时, 零点以来分钟数)。一次 GetLocalTime 同时服务两种模式
fn local_now() -> (u32, u32) {
    let st = unsafe { GetLocalTime() };
    let h = st.wHour as u32;
    (h, h * 60 + st.wMinute as u32)
}

/// 日志友好：路径 → 文件名
fn short_name(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

/// 记录素材健康翻转；「健康 → 全失效」时向主窗口发一次提示
fn update_health(app: &AppHandle, label: &str, healthy: bool) {
    let mut h = match healthy_screens().lock() {
        Ok(h) => h,
        Err(_) => return,
    };
    let was = h.contains(label);
    if was == healthy {
        return;
    }
    if healthy {
        h.insert(label.to_string());
    } else {
        h.remove(label);
        let _ = app.emit(
            "timed:notice",
            TimedNotice {
                label: label.to_string(),
                reason:
                    "定时素材已全部失效（被删除或移动），该屏暂恢复静态壁纸；素材恢复后自动继续"
                        .into(),
            },
        );
    }
}

/// 该屏是否处于「同一期望素材」的失败冷却期
fn cooldown_active(label: &str, expected: &str) -> bool {
    failures().lock().ok().is_some_and(|f| {
        f.get(label)
            .is_some_and(|(p, until)| p == expected && Instant::now() < *until)
    })
}

fn record_failure(label: &str, expected: &str) {
    if let Ok(mut f) = failures().lock() {
        f.insert(
            label.to_string(),
            (expected.to_string(), Instant::now() + FAIL_COOLDOWN),
        );
    }
}

fn clear_failure(label: &str) {
    if let Ok(mut f) = failures().lock() {
        f.remove(label);
    }
}
