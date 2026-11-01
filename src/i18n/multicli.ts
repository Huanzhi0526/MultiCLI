import i18n from "@/lib/i18n";

i18n.addResourceBundle("zh", "multicli", {
  stage: "S00 基线验证窗口。产品功能尚未实施。",
  profile: "配置环境", dataRoot: "独立数据目录", commit: "固定上游提交", loading: "读取中",
  disabled: "远程访问、relay、遥测、电脑控制、插件安装、自动更新及 CLI 配置写入均未启用。",
  run: "验证真实 shell PTY", pending: "正在运行真实 Windows PTY 探针…", notRun: "尚未运行",
});
i18n.addResourceBundle("en", "multicli", {
  stage: "S00 baseline verification. Product features have not been implemented.",
  profile: "Profile", dataRoot: "Isolated data directory", commit: "Pinned upstream commit", loading: "Loading",
  disabled: "Remote access, relay, telemetry, computer use, plugins, auto-update and CLI configuration writes are disabled.",
  run: "Verify real shell PTY", pending: "Running the real Windows PTY probe…", notRun: "Not run",
});
