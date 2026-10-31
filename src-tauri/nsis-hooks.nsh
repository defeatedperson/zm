; Tauri NSIS 安装器钩子（tauri.conf.json > bundle.windows.nsis.installerHooks）
; 作用与 src/lib.rs 的 repair_autostart_entry 相同，但时机提前到安装完成：
; 老版本升级上来（旧 Run 值缺 --hidden / 路径没引号 / 换过安装目录）时，
; 升级后的第一次开机自启就是静默的，而不是弹一次主窗口再靠 app 侧修复。
;
; 只改写已存在的值、绝不新建：Run 值不存在说明用户没开（或已关）开机自启，
; 不去碰；任务管理器禁用项的 StartupApproved 状态也不受影响。
;
; 注意：不要在 NSIS_HOOK_POSTUNINSTALL 里删除该值！Tauri 升级流程会先静默
; 运行旧版卸载器再装新版，若卸载即删除，用户已开启的自启会在每次升级时被
; 悄悄关掉。纯卸载留下的残留项无害：指向不存在的 exe 时 Windows 静默忽略。

!macro NSIS_HOOK_POSTINSTALL
  ClearErrors
  ReadRegStr $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}"
  IfErrors no_autostart 0
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}" '"$INSTDIR\${MAINBINARYNAME}.exe" --hidden'
  DetailPrint "Autostart entry normalized to: $INSTDIR\${MAINBINARYNAME}.exe --hidden"
  Goto done
  no_autostart:
  DetailPrint "Autostart not configured, skip normalization"
  done:
!macroend
